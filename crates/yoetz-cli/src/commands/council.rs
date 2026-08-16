use anyhow::{anyhow, Result};

use crate::notifications;
use crate::{
    add_usage, call_model, maybe_write_output, normalize_model_name_with_aliases, render_bundle_md,
    resolve_max_output_tokens_for_provider, resolve_prompt, resolve_provider_for_model,
    resolve_registry_model_id, resolve_response_format, validate_cursor_options, AppContext,
    CouncilArgs, CouncilModelArtifact, CouncilModelResult, CouncilPricing, CouncilSummary,
    ModelEstimate, PartialPolicy,
};
use crate::{budget, registry};
use crate::{CouncilModelError, CouncilResult};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use yoetz_core::bundle::{build_bundle, estimate_tokens, BundleOptions};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, Usage};

pub(crate) async fn handle_council(
    ctx: &AppContext,
    args: CouncilArgs,
    format: OutputFormat,
) -> Result<()> {
    let started_at = Instant::now();
    let prompt = resolve_prompt(args.prompt.clone(), args.prompt_file.clone())?;
    let config = &ctx.config;

    if args.models.is_empty() {
        return Err(anyhow!("at least one model is required"));
    }

    let default_provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .map(|provider| provider.to_lowercase());
    // Load registry early so council can auto-resolve providers (e.g. x-ai/grok-4 → openrouter)
    let registry_cache = registry::load_registry_with_auto_sync(&ctx.client, &ctx.config)
        .await
        .ok()
        .flatten();
    let mut resolved_models = Vec::new();
    let mut provider_keys = BTreeSet::new();
    for model in &args.models {
        let normalized = normalize_model_name_with_aliases(model, &config.aliases);
        let provider = resolve_council_provider(
            &normalized,
            default_provider.as_deref(),
            registry_cache.as_ref(),
        )?;
        provider_keys.insert(provider.clone());
        resolved_models.push((normalized, provider));
    }
    let council_provider = if provider_keys.len() == 1 {
        provider_keys
            .iter()
            .next()
            .cloned()
            .unwrap_or_else(|| "mixed".to_string())
    } else {
        "mixed".to_string()
    };
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;

    let include_files = args.files.clone();
    let exclude_files = args.exclude.clone();

    let bundle = if include_files.is_empty() {
        None
    } else {
        let options = BundleOptions {
            include: include_files,
            exclude: exclude_files,
            max_file_bytes: args.max_file_bytes,
            max_total_bytes: args.max_total_bytes,
            ..Default::default()
        };
        Some(build_bundle(&prompt, options)?)
    };

    let input_tokens = bundle
        .as_ref()
        .map(|b| b.stats.estimated_tokens)
        .unwrap_or_else(|| estimate_tokens(prompt.len()));
    // Validate each model against registry
    for (model, _provider) in &resolved_models {
        let reg_id =
            resolve_registry_model_id(Some(_provider), Some(model), registry_cache.as_ref());
        if let Some(ref id) = reg_id {
            crate::validate_model_or_suggest(id, registry_cache.as_ref(), ctx.allow_unknown)?;
        }
    }
    // Resolve registry IDs up front so we can derive model-aware max_output_tokens
    let resolved_registry_ids: Vec<Option<String>> = resolved_models
        .iter()
        .map(|(model, provider)| {
            resolve_registry_model_id(Some(provider), Some(model), registry_cache.as_ref())
        })
        .collect();
    // Resolve per-model max_output_tokens so each model gets its own limit.
    let per_model_max_output_tokens: Vec<Option<usize>> = resolved_models
        .iter()
        .zip(&resolved_registry_ids)
        .map(|((_model, provider), reg_id)| {
            resolve_max_output_tokens_for_provider(
                Some(provider),
                args.max_output_tokens,
                config,
                registry_cache.as_ref(),
                reg_id.as_deref(),
            )
        })
        .collect();
    for (idx, (_model, provider)) in resolved_models.iter().enumerate() {
        validate_cursor_options(
            Some(provider),
            per_model_max_output_tokens[idx],
            response_format.as_ref(),
            false,
            args.temperature,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?;
    }

    let mut per_model = Vec::new();
    let mut per_model_pricing = Vec::new();
    let mut estimate_sum = 0.0;
    let mut estimate_complete = true;
    for (idx, (model, _provider)) in resolved_models.iter().enumerate() {
        let registry_id = &resolved_registry_ids[idx];
        let output_tokens = per_model_max_output_tokens[idx].unwrap_or(4096);
        let estimate = registry::estimate_pricing(
            registry_cache.as_ref(),
            registry_id.as_deref().unwrap_or(model),
            input_tokens,
            output_tokens,
        )?;
        per_model_pricing.push(estimate.clone());
        if let Some(cost) = estimate.estimate_usd {
            estimate_sum += cost;
        } else {
            estimate_complete = false;
        }
        per_model.push(ModelEstimate {
            model: model.clone(),
            estimate_usd: estimate.estimate_usd,
        });
    }
    let total_estimate = if estimate_complete {
        Some(estimate_sum)
    } else {
        None
    };

    let budget_enabled = args.max_cost_usd.is_some() || args.daily_budget_usd.is_some();
    let budget_reservation = if budget_enabled {
        budget::ensure_budget(total_estimate, args.max_cost_usd, args.daily_budget_usd)?
    } else {
        None
    };

    let session = create_session_dir()?;
    let mut artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };

    if let Some(bundle_ref) = &bundle {
        let bundle_json = session.path.join("bundle.json");
        let bundle_md = session.path.join("bundle.md");
        write_json_file(&bundle_json, bundle_ref)?;
        write_text(&bundle_md, &render_bundle_md(bundle_ref))?;
        artifacts.bundle_json = Some(bundle_json.to_string_lossy().to_string());
        artifacts.bundle_md = Some(bundle_md.to_string_lossy().to_string());
    }

    let mut results = Vec::new();
    let mut total_usage = Usage::default();
    let mut errors = Vec::new();
    let mut model_artifacts = Vec::new();
    let model_prompt = std::sync::Arc::new(if let Some(bundle_ref) = &bundle {
        render_bundle_md(bundle_ref)
    } else {
        prompt.clone()
    });

    if args.dry_run {
        for (idx, (model, provider)) in resolved_models.iter().enumerate() {
            let registry_id =
                resolve_registry_model_id(Some(provider), Some(model), registry_cache.as_ref());
            let output_tokens = per_model_max_output_tokens[idx].unwrap_or(4096);
            let result = CouncilModelResult {
                model: model.clone(),
                content: "(dry-run) no provider call executed".to_string(),
                usage: Usage::default(),
                pricing: registry::estimate_pricing(
                    registry_cache.as_ref(),
                    registry_id.as_deref().unwrap_or(model),
                    input_tokens,
                    output_tokens,
                )?,
                response_id: None,
            };
            model_artifacts.push((
                idx,
                successful_model_artifact(model.clone(), provider.clone(), &result),
            ));
            results.push(result);
        }
    } else {
        let max_parallel = args.max_parallel.max(1);
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_parallel));
        let mut join_set = tokio::task::JoinSet::new();
        for (idx, (model, provider)) in resolved_models.iter().cloned().enumerate() {
            let prompt = std::sync::Arc::clone(&model_prompt);
            let provider = provider.clone();
            let litellm = ctx.litellm.clone();
            let cursor_timeout = ctx.timeout_duration;
            let semaphore = std::sync::Arc::clone(&semaphore);
            let temperature = args.temperature;
            let response_format = response_format.clone();
            let model_max_output_tokens = per_model_max_output_tokens[idx];
            join_set.spawn(async move {
                let _permit = semaphore.acquire_owned().await.map_err(|err| {
                    (
                        idx,
                        model.clone(),
                        provider.clone(),
                        anyhow!("failed to acquire council permit: {err}"),
                    )
                })?;
                let call = call_model(
                    &litellm,
                    cursor_timeout,
                    Some(&provider),
                    &model,
                    prompt.as_str(),
                    temperature,
                    model_max_output_tokens,
                    response_format,
                    &[],
                    None,
                )
                .await;
                match call {
                    Ok(call) => Ok((idx, model, provider, call)),
                    Err(err) => Err((idx, model, provider, err)),
                }
            });
        }

        let mut ordered: Vec<Option<CouncilModelResult>> =
            (0..resolved_models.len()).map(|_| None).collect();
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(Ok((idx, model, provider, call))) => {
                    let mut usage = call.usage;
                    if usage.cost_usd.is_none() {
                        usage.cost_usd = call.header_cost;
                    }
                    if usage.cost_usd.is_none() && provider == "openrouter" {
                        if let Some(id) = call.response_id.as_deref() {
                            if let Ok(cost) =
                                crate::fetch_openrouter_cost(&ctx.client, config, id).await
                            {
                                usage.cost_usd = cost;
                            }
                        }
                    }

                    total_usage = add_usage(total_usage, &usage);

                    let registry_id = resolve_registry_model_id(
                        Some(&provider),
                        Some(&model),
                        registry_cache.as_ref(),
                    );
                    let output_tokens = per_model_max_output_tokens[idx].unwrap_or(4096);
                    let pricing = registry::estimate_pricing(
                        registry_cache.as_ref(),
                        registry_id.as_deref().unwrap_or(&model),
                        input_tokens,
                        output_tokens,
                    )?;

                    let result = CouncilModelResult {
                        model: model.clone(),
                        content: call.content,
                        usage,
                        pricing,
                        response_id: call.response_id,
                    };
                    model_artifacts
                        .push((idx, successful_model_artifact(model, provider, &result)));
                    ordered[idx] = Some(result);
                }
                Ok(Err((idx, model, provider, err))) => {
                    let error = err.to_string();
                    model_artifacts.push((
                        idx,
                        failed_model_artifact(
                            model.clone(),
                            provider.clone(),
                            per_model_pricing[idx].clone(),
                            error.clone(),
                        ),
                    ));
                    errors.push(CouncilModelError {
                        model,
                        provider,
                        error,
                    });
                }
                Err(err) => {
                    let error = err.to_string();
                    model_artifacts.push((
                        usize::MAX,
                        failed_model_artifact(
                            "<task>".to_string(),
                            "internal".to_string(),
                            Default::default(),
                            error.clone(),
                        ),
                    ));
                    errors.push(CouncilModelError {
                        model: "<task>".to_string(),
                        provider: "internal".to_string(),
                        error,
                    });
                }
            }
        }

        results = ordered.into_iter().flatten().collect();
    }

    model_artifacts.sort_by_key(|(index, _)| *index);

    if results.is_empty() && !errors.is_empty() {
        write_model_artifacts(&session.path, &model_artifacts);
        let joined = errors
            .iter()
            .map(|error| format!("- {} ({}): {}", error.model, error.provider, error.error))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!("all council models failed:\n{joined}"));
    }

    if budget_enabled && !args.dry_run {
        let mut spend = 0.0;
        let mut has_spend = false;
        for r in &results {
            if let Some(cost) = r.usage.cost_usd.or(r.pricing.estimate_usd) {
                spend += cost;
                has_spend = true;
            }
        }
        if has_spend {
            if let Some(reservation) = budget_reservation {
                if let Err(e) = reservation.commit(spend) {
                    eprintln!("warning: budget commit failed: {e}");
                }
            } else if let Err(e) = budget::record_spend_standalone(spend) {
                eprintln!("warning: budget commit failed: {e}");
            }
        }
    }

    let summary = CouncilSummary {
        succeeded: results.len(),
        failed: errors.len(),
        total: results.len() + errors.len(),
        cost_usd: results
            .iter()
            .filter_map(|result| result.usage.cost_usd)
            .sum(),
        elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    let strict_partial_failure = matches!(args.partial, PartialPolicy::Fail) && !errors.is_empty();

    let mut council = CouncilResult {
        id: session.id,
        provider: council_provider,
        bundle,
        results,
        errors,
        summary,
        pricing: CouncilPricing {
            estimate_usd_total: total_estimate,
            per_model,
        },
        usage: total_usage,
        artifacts,
    };

    let response_json = PathBuf::from(&council.artifacts.session_dir).join("council.json");
    council.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &council)?;

    maybe_write_output(ctx, &council)?;
    write_model_artifacts(&session.path, &model_artifacts);
    let notified_target = if council.summary.total == 1 {
        council
            .results
            .first()
            .map(|result| result.model.as_str())
            .unwrap_or("model")
            .to_string()
    } else {
        format!("{} models", council.summary.total)
    };
    let notified_preview = council
        .results
        .first()
        .map(|result| result.content.as_str())
        .unwrap_or("");
    let notified_cost = council
        .results
        .iter()
        .any(|result| result.usage.cost_usd.is_some())
        .then_some(council.summary.cost_usd);
    notifications::maybe_notify_completion(
        &ctx.config,
        args.no_notify,
        "council",
        &notified_target,
        notified_preview,
        started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        notified_cost,
        ctx.debug,
    );

    // Omit bundle from stdout to keep JSON output compact (full result is in session file)
    council.bundle = None;

    match format {
        OutputFormat::Json => write_json(&council),
        OutputFormat::Jsonl => write_jsonl("council", &council),
        OutputFormat::Text => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            if !council.errors.is_empty() {
                println!("## Errors");
                for error in &council.errors {
                    println!("- {} ({}): {}", error.model, error.provider, error.error);
                }
                println!();
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            if !council.errors.is_empty() {
                println!("## Errors");
                for error in &council.errors {
                    println!("- {} ({}): {}", error.model, error.provider, error.error);
                }
                println!();
            }
            Ok(())
        }
    }?;

    if strict_partial_failure {
        return Err(anyhow!(
            "council completed with {} failed model(s) under --partial fail",
            council.summary.failed
        ));
    }
    Ok(())
}

fn write_model_artifacts(session_dir: &Path, artifacts: &[(usize, CouncilModelArtifact)]) {
    let models_dir = session_dir.join("models");
    if let Err(error) = fs::create_dir_all(&models_dir) {
        eprintln!(
            "warning: could not create council model artifact directory {}: {error}",
            models_dir.display()
        );
        return;
    }
    let mut slug_counts = BTreeMap::new();
    for (_, artifact) in artifacts {
        let slug = model_artifact_slug(&artifact.model);
        let count = slug_counts.entry(slug.clone()).or_insert(0_usize);
        *count += 1;
        let filename = if *count == 1 {
            format!("{slug}.json")
        } else {
            format!("{slug}-{}.json", *count)
        };
        let path = models_dir.join(filename);
        if let Err(error) = write_json_file(&path, artifact) {
            eprintln!(
                "warning: could not write council model artifact {}: {error}",
                path.display()
            );
        }
    }
}

fn successful_model_artifact(
    model: String,
    provider: String,
    result: &CouncilModelResult,
) -> CouncilModelArtifact {
    CouncilModelArtifact {
        status: "succeeded",
        model,
        provider,
        content: Some(result.content.clone()),
        usage: result.usage.clone(),
        pricing: result.pricing.clone(),
        response_id: result.response_id.clone(),
        error: None,
    }
}

fn failed_model_artifact(
    model: String,
    provider: String,
    pricing: yoetz_core::types::PricingEstimate,
    error: String,
) -> CouncilModelArtifact {
    CouncilModelArtifact {
        status: "failed",
        model,
        provider,
        content: None,
        usage: Usage::default(),
        pricing,
        response_id: None,
        error: Some(error),
    }
}

fn model_artifact_slug(model: &str) -> String {
    let mut slug = String::new();
    let mut pending_separator = false;
    for ch in model.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(ch.to_ascii_lowercase());
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    if slug.is_empty() {
        "model".to_string()
    } else {
        slug
    }
}

fn resolve_council_provider(
    model: &str,
    default_provider: Option<&str>,
    registry: Option<&yoetz_core::registry::ModelRegistry>,
) -> Result<String> {
    // Prefer local/registry lookup — it knows Cursor locally and that
    // x-ai/grok-4 is openrouter in the API registry.
    if let Some(provider) = resolve_provider_for_model(model, registry) {
        return Ok(provider);
    }
    if let Some(provider) = prefixed_council_provider(model) {
        return Ok(provider);
    }
    if let Some(provider) = default_provider {
        return Ok(provider.to_string());
    }
    Err(anyhow!(
        "provider is required for model '{model}'. Use --provider or prefix the model (e.g. openai/{model})"
    ))
}

fn prefixed_council_provider(model: &str) -> Option<String> {
    let (prefix, _rest) = model.split_once('/')?;
    if prefix.eq_ignore_ascii_case("models") {
        return None;
    }
    Some(prefix.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{model_artifact_slug, write_model_artifacts, CouncilModelArtifact};
    use std::fs;
    use yoetz_core::types::{PricingEstimate, Usage};

    #[test]
    fn model_artifact_slug_is_safe_for_provider_qualified_ids() {
        assert_eq!(
            model_artifact_slug("openai/gpt-5.4-pro"),
            "openai-gpt-5-4-pro"
        );
        assert_eq!(model_artifact_slug("///"), "model");
    }

    #[test]
    fn model_artifact_write_failure_is_best_effort() {
        let session = tempfile::tempdir().unwrap();
        fs::write(session.path().join("models"), "not a directory").unwrap();
        let artifacts = vec![(
            0,
            CouncilModelArtifact {
                status: "succeeded",
                model: "test/model".to_string(),
                provider: "test".to_string(),
                content: Some("paid result".to_string()),
                usage: Usage::default(),
                pricing: PricingEstimate::default(),
                response_id: None,
                error: None,
            },
        )];

        write_model_artifacts(session.path(), &artifacts);
        assert!(session.path().join("models").is_file());
    }
}

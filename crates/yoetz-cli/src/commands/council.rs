use anyhow::{anyhow, Result};

use crate::CouncilResult;
use crate::{
    add_usage, call_litellm, maybe_write_output, normalize_model_name, render_bundle_md,
    resolve_max_output_tokens, resolve_prompt, resolve_registry_model_id, resolve_response_format,
    AppContext, CouncilArgs, CouncilModelResult, CouncilPricing, ModelEstimate,
};
use crate::{budget, registry};
use std::collections::BTreeSet;
use std::path::PathBuf;
use yoetz_core::bundle::{build_bundle, estimate_tokens, BundleOptions};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, Usage};

pub(crate) async fn handle_council(
    ctx: &AppContext,
    args: CouncilArgs,
    format: OutputFormat,
) -> Result<()> {
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
    let mut resolved_models = Vec::new();
    let mut provider_keys = BTreeSet::new();
    for model in &args.models {
        let normalized = normalize_model_name(model);
        let provider = resolve_council_provider(&normalized, default_provider.as_deref())?;
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

    let registry_cache = registry::load_registry_cache().ok().flatten();
    let input_tokens = bundle
        .as_ref()
        .map(|b| b.stats.estimated_tokens)
        .unwrap_or_else(|| estimate_tokens(prompt.len()));
    // Resolve registry IDs up front so we can derive model-aware max_output_tokens
    let resolved_registry_ids: Vec<Option<String>> = resolved_models
        .iter()
        .map(|(model, provider)| {
            resolve_registry_model_id(Some(provider), Some(model), registry_cache.as_ref())
        })
        .collect();
    // Resolve per-model and take the maximum so no model gets starved in mixed councils.
    let max_output_tokens: Option<usize> = {
        let mut best: Option<usize> = None;
        for reg_id in &resolved_registry_ids {
            if let Some(val) = resolve_max_output_tokens(
                args.max_output_tokens,
                config,
                registry_cache.as_ref(),
                reg_id.as_deref(),
            ) {
                best = Some(best.map_or(val, |b: usize| b.max(val)));
            }
        }
        best
    };
    let output_tokens = max_output_tokens.unwrap_or(4096);

    let mut per_model = Vec::new();
    let mut estimate_sum = 0.0;
    let mut estimate_complete = true;
    for (idx, (model, _provider)) in resolved_models.iter().enumerate() {
        let registry_id = &resolved_registry_ids[idx];
        let estimate = registry::estimate_pricing(
            registry_cache.as_ref(),
            registry_id.as_deref().unwrap_or(model),
            input_tokens,
            output_tokens,
        )?;
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
    let model_prompt = std::sync::Arc::new(if let Some(bundle_ref) = &bundle {
        render_bundle_md(bundle_ref)
    } else {
        prompt.clone()
    });

    if args.dry_run {
        for (model, provider) in &resolved_models {
            let registry_id =
                resolve_registry_model_id(Some(provider), Some(model), registry_cache.as_ref());
            results.push(CouncilModelResult {
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
            });
        }
    } else {
        let max_parallel = args.max_parallel.max(1);
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_parallel));
        let mut join_set = tokio::task::JoinSet::new();
        for (idx, (model, provider)) in resolved_models.iter().cloned().enumerate() {
            let prompt = std::sync::Arc::clone(&model_prompt);
            let provider = provider.clone();
            let litellm = ctx.litellm.clone();
            let semaphore = std::sync::Arc::clone(&semaphore);
            let temperature = args.temperature;
            let response_format = response_format.clone();
            join_set.spawn(async move {
                let _permit = semaphore.acquire_owned().await?;
                let call = call_litellm(
                    &litellm,
                    Some(&provider),
                    &model,
                    prompt.as_str(),
                    temperature,
                    max_output_tokens,
                    response_format,
                    &[],
                    None,
                )
                .await?;
                Ok::<_, anyhow::Error>((idx, model, provider, call))
            });
        }

        let mut ordered: Vec<Option<CouncilModelResult>> =
            (0..args.models.len()).map(|_| None).collect();
        while let Some(res) = join_set.join_next().await {
            let (idx, model, provider, call) = res??;
            let mut usage = call.usage;
            if usage.cost_usd.is_none() {
                usage.cost_usd = call.header_cost;
            }
            if usage.cost_usd.is_none() && provider == "openrouter" {
                if let Some(id) = call.response_id.as_deref() {
                    if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                        usage.cost_usd = cost;
                    }
                }
            }

            total_usage = add_usage(total_usage, &usage);

            let registry_id =
                resolve_registry_model_id(Some(&provider), Some(&model), registry_cache.as_ref());
            let pricing = registry::estimate_pricing(
                registry_cache.as_ref(),
                registry_id.as_deref().unwrap_or(&model),
                input_tokens,
                output_tokens,
            )?;

            ordered[idx] = Some(CouncilModelResult {
                model,
                content: call.content,
                usage,
                pricing,
                response_id: call.response_id,
            });
        }

        results = ordered.into_iter().flatten().collect();
    }

    if budget_enabled {
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
                let _ = reservation.commit(spend);
            } else {
                let _ = budget::record_spend_standalone(spend);
            }
        }
    }

    let mut council = CouncilResult {
        id: session.id,
        provider: council_provider,
        bundle,
        results,
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

    // Omit bundle from stdout to keep JSON output compact (full result is in session file)
    council.bundle = None;

    match format {
        OutputFormat::Json => write_json(&council),
        OutputFormat::Jsonl => write_jsonl("council", &council),
        OutputFormat::Text => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            for r in &council.results {
                println!("## {}\n{}\n", r.model, r.content);
            }
            Ok(())
        }
    }
}

fn resolve_council_provider(model: &str, default_provider: Option<&str>) -> Result<String> {
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

use anyhow::{anyhow, Result};

use crate::CouncilResult;
use crate::{
    add_usage, call_litellm, maybe_write_output, render_bundle_md, resolve_max_output_tokens,
    resolve_prompt, resolve_registry_model_id, resolve_response_format, AppContext, CouncilArgs,
    CouncilModelResult, CouncilPricing, ModelEstimate,
};
use crate::{budget, registry};
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

    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .ok_or_else(|| anyhow!("provider is required"))?;
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
    let max_output_tokens = resolve_max_output_tokens(args.max_output_tokens, config);
    let output_tokens = max_output_tokens;

    let mut per_model = Vec::new();
    let mut estimate_sum = 0.0;
    let mut estimate_complete = true;
    for model in &args.models {
        let registry_id =
            resolve_registry_model_id(Some(&provider), Some(model), registry_cache.as_ref());
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
        for model in &args.models {
            let registry_id =
                resolve_registry_model_id(Some(&provider), Some(model), registry_cache.as_ref());
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
        for (idx, model) in args.models.iter().cloned().enumerate() {
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
                Ok::<_, anyhow::Error>((idx, model, call))
            });
        }

        let mut ordered: Vec<Option<CouncilModelResult>> =
            (0..args.models.len()).map(|_| None).collect();
        while let Some(res) = join_set.join_next().await {
            let (idx, model, call) = res??;
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
        provider,
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

use anyhow::{anyhow, Result};

use crate::providers::{gemini, openai};
use crate::{
    apply_capability_warnings, call_litellm, maybe_write_output, normalize_model_name,
    parse_media_input, parse_media_inputs, resolve_max_output_tokens, resolve_prompt,
    resolve_registry_model_id, resolve_response_format, AppContext, AskArgs,
};
use crate::{budget, providers, registry};
use std::env;
use std::path::PathBuf;
use yoetz_core::bundle::{build_bundle, estimate_tokens, BundleOptions};
use yoetz_core::media::MediaType;
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, PricingEstimate, RunResult, Usage};

pub(crate) async fn handle_ask(
    ctx: &AppContext,
    args: AskArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt.clone(), args.prompt_file.clone())?;
    let config = &ctx.config;
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;

    let image_inputs = parse_media_inputs(&args.image, &args.image_mime, MediaType::Image)?;
    if args.video.is_none() && args.video_mime.is_some() {
        return Err(anyhow!("--video-mime requires --video"));
    }
    let video_input = match args.video.as_deref() {
        Some(value) => Some(parse_media_input(
            value,
            args.video_mime.as_deref(),
            MediaType::Video,
        )?),
        None => None,
    };

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

    let session = create_session_dir()?;
    let mut artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };

    if let Some(bundle_ref) = &bundle {
        let bundle_json = session.path.join("bundle.json");
        let bundle_md = session.path.join("bundle.md");
        write_json_file(&bundle_json, bundle_ref)?;
        write_text(&bundle_md, &crate::render_bundle_md(bundle_ref))?;
        artifacts.bundle_json = Some(bundle_json.to_string_lossy().to_string());
        artifacts.bundle_md = Some(bundle_md.to_string_lossy().to_string());
    }

    let model_id = args
        .model
        .clone()
        .or(config.defaults.model.clone())
        .map(|m| normalize_model_name(&m));
    let provider_id = args.provider.clone().or(config.defaults.provider.clone());
    let registry_cache = registry::load_registry_cache().ok().flatten();
    let registry_model_id = resolve_registry_model_id(
        provider_id.as_deref(),
        model_id.as_deref(),
        registry_cache.as_ref(),
    );
    let max_output_tokens = resolve_max_output_tokens(
        args.max_output_tokens,
        config,
        registry_cache.as_ref(),
        registry_model_id.as_deref(),
    );
    let input_tokens = bundle
        .as_ref()
        .map(|b| b.stats.estimated_tokens)
        .unwrap_or_else(|| estimate_tokens(prompt.len()));
    let output_tokens = max_output_tokens;
    let mut pricing = if let Some(model_id) = registry_model_id.as_deref() {
        registry::estimate_pricing(
            registry_cache.as_ref(),
            model_id,
            input_tokens,
            output_tokens,
        )?
    } else {
        PricingEstimate::default()
    };

    apply_capability_warnings(
        registry_cache.as_ref(),
        registry_model_id.as_deref(),
        !image_inputs.is_empty(),
        video_input.is_some(),
        &mut pricing,
    )?;

    let budget_enabled = args.max_cost_usd.is_some() || args.daily_budget_usd.is_some();
    let budget_reservation = if budget_enabled {
        budget::ensure_budget(
            pricing.estimate_usd,
            args.max_cost_usd,
            args.daily_budget_usd,
        )?
    } else {
        None
    };

    let model_prompt = if let Some(bundle_ref) = &bundle {
        crate::render_bundle_md(bundle_ref)
    } else {
        prompt.clone()
    };

    let (content, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else if !image_inputs.is_empty() || video_input.is_some() {
        let provider = provider_id
            .as_deref()
            .ok_or_else(|| anyhow!("provider is required"))?;
        let model = model_id
            .as_deref()
            .ok_or_else(|| anyhow!("model is required"))?;
        if video_input.is_some() && provider != "gemini" {
            return Err(anyhow!(
                "video inputs are only supported for provider gemini"
            ));
        }
        match provider {
            "openai" => {
                if video_input.is_some() {
                    return Err(anyhow!("openai provider does not support video inputs"));
                }
                let auth = providers::resolve_provider_auth(config, provider)?;
                let result = openai::call_responses_vision(
                    &ctx.client,
                    &auth,
                    &model_prompt,
                    model,
                    &image_inputs,
                    response_format.clone(),
                    args.temperature,
                    max_output_tokens,
                )
                .await?;
                (result.content, result.usage, result.response_id, None)
            }
            "gemini" => {
                let auth = providers::resolve_provider_auth(config, provider)?;
                let result = gemini::generate_content(
                    &ctx.client,
                    &auth,
                    &model_prompt,
                    model,
                    &image_inputs,
                    video_input.as_ref(),
                    args.temperature,
                    max_output_tokens,
                )
                .await?;
                if ctx.debug || env::var("YOETZ_GEMINI_DEBUG").ok().as_deref() == Some("1") {
                    let raw_path = session.path.join("gemini_response.json");
                    let _ = write_json_file(&raw_path, &result.raw);
                }
                (result.content, result.usage, None, None)
            }
            _ => {
                let call = call_litellm(
                    &ctx.litellm,
                    Some(provider),
                    model,
                    &model_prompt,
                    args.temperature,
                    max_output_tokens,
                    response_format.clone(),
                    &image_inputs,
                    video_input.as_ref(),
                )
                .await?;
                (call.content, call.usage, call.response_id, call.header_cost)
            }
        }
    } else {
        let provider = provider_id
            .as_deref()
            .ok_or_else(|| anyhow!("provider is required"))?;
        let model = model_id
            .as_deref()
            .ok_or_else(|| anyhow!("model is required"))?;
        let result = call_litellm(
            &ctx.litellm,
            Some(provider),
            model,
            &model_prompt,
            args.temperature,
            max_output_tokens,
            response_format.clone(),
            &[],
            None,
        )
        .await?;
        (
            result.content,
            result.usage,
            result.response_id,
            result.header_cost,
        )
    };

    if usage.cost_usd.is_none() {
        usage.cost_usd = header_cost;
    }

    if usage.cost_usd.is_none() {
        if let Some(provider) = provider_id.as_deref() {
            if provider == "openrouter" {
                if let Some(id) = response_id.as_deref() {
                    if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                        usage.cost_usd = cost;
                    }
                }
            }
        }
    }

    if provider_id.as_deref() == Some("gemini") && content.trim().is_empty() {
        if let Some(thoughts) = usage.thoughts_tokens.filter(|t| *t > 0) {
            let model_max_hint = registry_model_id
                .as_deref()
                .and_then(|id| registry_cache.as_ref()?.find(id))
                .and_then(|e| e.max_output_tokens)
                .map(|m| format!(" (model supports up to {m})"))
                .unwrap_or_default();
            eprintln!(
                "warning: gemini returned empty content but used {thoughts} thought tokens; \
                 try increasing --max-output-tokens (current: {max_output_tokens}){model_max_hint}"
            );
        }
    }

    if budget_enabled {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            if let Some(reservation) = budget_reservation {
                let _ = reservation.commit(spend);
            } else {
                let _ = budget::record_spend_standalone(spend);
            }
        }
    }

    let mut result = RunResult {
        id: session.id,
        model: model_id,
        provider: provider_id,
        bundle,
        pricing,
        usage,
        content,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("response.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    // Omit bundle from stdout to keep JSON output compact (full result is in session file)
    result.bundle = None;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("ask", &result),
        OutputFormat::Text => {
            println!("{}", result.content);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("{}", result.content);
            Ok(())
        }
    }
}

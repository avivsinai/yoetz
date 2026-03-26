use anyhow::{anyhow, Result};

use crate::ReviewResult;
use crate::{budget, registry};
use crate::{
    build_review_diff_prompt, build_review_file_prompt, call_litellm, git_diff, maybe_write_output,
    normalize_model_name_with_aliases, read_text_file, resolve_max_output_tokens,
    resolve_provider_from_registry, resolve_registry_model_id, resolve_response_format, AppContext,
    ReviewArgs, ReviewCommand, ReviewDiffArgs, ReviewFileArgs,
};
use std::path::PathBuf;
use yoetz_core::bundle::estimate_tokens;
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, Usage};

const MAX_REVIEW_DIFF_TOKENS: usize = 50_000;

pub(crate) async fn handle_review(
    ctx: &AppContext,
    args: ReviewArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        ReviewCommand::Diff(diff_args) => handle_review_diff(ctx, diff_args, format).await,
        ReviewCommand::File(file_args) => handle_review_file(ctx, file_args, format).await,
    }
}

async fn handle_review_diff(
    ctx: &AppContext,
    args: ReviewDiffArgs,
    format: OutputFormat,
) -> Result<()> {
    let config = &ctx.config;
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;
    let model = normalize_model_name_with_aliases(
        &args
            .model
            .clone()
            .or(config.defaults.model.clone())
            .ok_or_else(|| anyhow!("model is required"))?,
        &config.aliases,
    );
    let registry_cache = registry::load_registry_with_auto_sync(&ctx.client, &ctx.config)
        .await
        .ok()
        .flatten();
    // Auto-resolve provider from registry (e.g. x-ai/grok-4 → openrouter)
    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .or_else(|| {
            let reg = registry_cache.as_ref()?;
            resolve_provider_from_registry(&model, reg)
        })
        .ok_or_else(|| anyhow!("provider is required"))?;
    let registry_id =
        resolve_registry_model_id(Some(&provider), Some(&model), registry_cache.as_ref());
    if let Some(ref reg_id) = registry_id {
        crate::validate_model_or_suggest(reg_id, registry_cache.as_ref(), ctx.allow_unknown)?;
    }
    let max_output_tokens = resolve_max_output_tokens(
        args.max_output_tokens,
        config,
        registry_cache.as_ref(),
        registry_id.as_deref(),
    );

    let mut diff = git_diff(args.staged, &args.paths)?;
    if diff.trim().is_empty() {
        return Err(anyhow!("diff is empty"));
    }

    let max_diff_bytes = args.max_diff_bytes;
    if diff.len() > max_diff_bytes {
        eprintln!(
            "warning: diff is {} bytes, truncating to {max_diff_bytes} bytes",
            diff.len()
        );
        // Truncate on a char boundary
        let mut end = max_diff_bytes;
        while end > 0 && !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
        diff.push_str("\n\n... [diff truncated — exceeded --max-diff-bytes limit]");
    }

    let review_prompt = build_review_diff_prompt(&diff, args.prompt.as_deref());
    let input_tokens = estimate_tokens(review_prompt.len());
    ensure_review_diff_size(input_tokens)?;
    let output_tokens = max_output_tokens.unwrap_or(4096);
    let pricing = registry::estimate_pricing(
        registry_cache.as_ref(),
        registry_id.as_deref().unwrap_or(&model),
        input_tokens,
        output_tokens,
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

    let session = create_session_dir()?;
    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };
    let review_input_path = session.path.join("review_input.txt");
    write_text(&review_input_path, &review_prompt)?;

    let (content, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else {
        let result = call_litellm(
            &ctx.litellm,
            Some(&provider),
            &model,
            &review_prompt,
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
    if usage.cost_usd.is_none() && provider == "openrouter" {
        if let Some(id) = response_id.as_deref() {
            if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                usage.cost_usd = cost;
            }
        }
    }

    if budget_enabled && !args.dry_run {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            if let Some(reservation) = budget_reservation {
                if let Err(e) = reservation.commit(spend) {
                    eprintln!("warning: budget commit failed: {e}");
                }
            } else if let Err(e) = budget::record_spend_standalone(spend) {
                eprintln!("warning: budget commit failed: {e}");
            }
        }
    }

    let mut result = ReviewResult {
        id: session.id,
        provider,
        model,
        pricing,
        usage,
        content,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("review.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("review", &result),
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

fn ensure_review_diff_size(input_tokens: usize) -> Result<()> {
    if input_tokens > MAX_REVIEW_DIFF_TOKENS {
        return Err(anyhow!(
            "diff too large for `yoetz review diff` (~{input_tokens} tokens > {MAX_REVIEW_DIFF_TOKENS}); narrow `--paths` or split the review into smaller chunks"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_diff_guardrail_rejects_large_prompts() {
        let err = ensure_review_diff_size(MAX_REVIEW_DIFF_TOKENS + 1).unwrap_err();
        assert!(err.to_string().contains("diff too large"));
    }

    #[test]
    fn review_diff_guardrail_allows_limit_boundary() {
        ensure_review_diff_size(MAX_REVIEW_DIFF_TOKENS).unwrap();
    }
}

async fn handle_review_file(
    ctx: &AppContext,
    args: ReviewFileArgs,
    format: OutputFormat,
) -> Result<()> {
    let config = &ctx.config;
    let response_format = resolve_response_format(
        args.response_format.clone(),
        args.response_schema.clone(),
        args.response_schema_name.clone(),
    )?;
    let model = normalize_model_name_with_aliases(
        &args
            .model
            .clone()
            .or(config.defaults.model.clone())
            .ok_or_else(|| anyhow!("model is required"))?,
        &config.aliases,
    );
    let registry_cache = registry::load_registry_with_auto_sync(&ctx.client, &ctx.config)
        .await
        .ok()
        .flatten();
    // Auto-resolve provider from registry (e.g. x-ai/grok-4 → openrouter)
    let provider = args
        .provider
        .clone()
        .or(config.defaults.provider.clone())
        .or_else(|| {
            let reg = registry_cache.as_ref()?;
            resolve_provider_from_registry(&model, reg)
        })
        .ok_or_else(|| anyhow!("provider is required"))?;
    let registry_id =
        resolve_registry_model_id(Some(&provider), Some(&model), registry_cache.as_ref());
    if let Some(ref reg_id) = registry_id {
        crate::validate_model_or_suggest(reg_id, registry_cache.as_ref(), ctx.allow_unknown)?;
    }
    let max_output_tokens = resolve_max_output_tokens(
        args.max_output_tokens,
        config,
        registry_cache.as_ref(),
        registry_id.as_deref(),
    );

    let max_file_bytes = args.max_file_bytes.unwrap_or(200_000);
    let max_total_bytes = args.max_total_bytes.unwrap_or(max_file_bytes);
    let max_bytes = max_file_bytes.min(max_total_bytes);
    let (content, truncated) = read_text_file(args.path.as_path(), max_bytes)?;
    let review_prompt = build_review_file_prompt(
        args.path.as_path(),
        &content,
        truncated,
        args.prompt.as_deref(),
    );
    let input_tokens = estimate_tokens(review_prompt.len());
    let output_tokens = max_output_tokens.unwrap_or(4096);
    let pricing = registry::estimate_pricing(
        registry_cache.as_ref(),
        registry_id.as_deref().unwrap_or(&model),
        input_tokens,
        output_tokens,
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

    let session = create_session_dir()?;
    let artifacts = ArtifactPaths {
        session_dir: session.path.to_string_lossy().to_string(),
        ..Default::default()
    };
    let review_input_path = session.path.join("review_input.txt");
    write_text(&review_input_path, &review_prompt)?;

    let (output, mut usage, response_id, header_cost) = if args.dry_run {
        (
            "(dry-run) no provider call executed".to_string(),
            Usage::default(),
            None,
            None,
        )
    } else {
        let result = call_litellm(
            &ctx.litellm,
            Some(&provider),
            &model,
            &review_prompt,
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
    if usage.cost_usd.is_none() && provider == "openrouter" {
        if let Some(id) = response_id.as_deref() {
            if let Ok(cost) = crate::fetch_openrouter_cost(&ctx.client, config, id).await {
                usage.cost_usd = cost;
            }
        }
    }

    if budget_enabled && !args.dry_run {
        if let Some(spend) = usage.cost_usd.or(pricing.estimate_usd) {
            if let Some(reservation) = budget_reservation {
                if let Err(e) = reservation.commit(spend) {
                    eprintln!("warning: budget commit failed: {e}");
                }
            } else if let Err(e) = budget::record_spend_standalone(spend) {
                eprintln!("warning: budget commit failed: {e}");
            }
        }
    }

    let mut result = ReviewResult {
        id: session.id,
        provider,
        model,
        pricing,
        usage,
        content: output,
        artifacts,
    };

    let response_json = PathBuf::from(&result.artifacts.session_dir).join("review.json");
    result.artifacts.response_json = Some(response_json.to_string_lossy().to_string());
    write_json_file(&response_json, &result)?;

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("review", &result),
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

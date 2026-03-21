use anyhow::Result;
use std::collections::HashMap;

use crate::fuzzy;
use crate::{
    maybe_write_output, registry, AppContext, ModelsArgs, ModelsCommand, ModelsFrontierArgs,
    ModelsListArgs, ModelsResolveArgs,
};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::registry::ModelRegistry;

pub(crate) async fn handle_models(
    ctx: &AppContext,
    args: ModelsArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        ModelsCommand::List(list_args) => {
            let registry = registry::load_registry_with_auto_sync(&ctx.client, &ctx.config)
                .await?
                .unwrap_or_default()
                .with_inferred_tiers();
            let filtered = filter_registry(&registry, &list_args);
            maybe_write_output(ctx, &filtered)?;
            match format {
                OutputFormat::Json => write_json(&filtered),
                OutputFormat::Jsonl => write_jsonl("models_list", &filtered),
                OutputFormat::Text | OutputFormat::Markdown => {
                    for model in &filtered.models {
                        let provider = model.provider.as_deref().unwrap_or("-");
                        let tier_str = model.tier.map(|t| t.to_string()).unwrap_or_default();
                        if tier_str.is_empty() {
                            println!("{:<14}{}", provider, model.id);
                        } else {
                            println!("{:<14}{:<43}{}", provider, model.id, tier_str);
                        }
                    }
                    Ok(())
                }
            }
        }
        ModelsCommand::Sync => {
            let fetch = registry::fetch_registry(&ctx.client, &ctx.config).await?;
            let path = registry::save_registry_cache(&fetch.registry)?;
            let payload = serde_json::json!({
                "saved_to": path,
                "model_count": fetch.registry.models.len(),
                "warnings": fetch.warnings,
            });
            maybe_write_output(ctx, &payload)?;
            match format {
                OutputFormat::Json => write_json(&payload),
                OutputFormat::Jsonl => write_jsonl("models_sync", &payload),
                OutputFormat::Text | OutputFormat::Markdown => {
                    println!(
                        "Saved {} models to {}",
                        fetch.registry.models.len(),
                        path.display()
                    );
                    if !fetch.warnings.is_empty() {
                        eprintln!("Warnings:");
                        for warning in &fetch.warnings {
                            eprintln!("- {warning}");
                        }
                    }
                    Ok(())
                }
            }
        }
        ModelsCommand::Resolve(resolve_args) => handle_resolve(ctx, resolve_args, format).await,
        ModelsCommand::Frontier(frontier_args) => handle_frontier(ctx, frontier_args, format).await,
    }
}

async fn handle_resolve(
    ctx: &AppContext,
    args: ModelsResolveArgs,
    format: OutputFormat,
) -> Result<()> {
    let registry = registry::load_registry_with_auto_sync(&ctx.client, &ctx.config)
        .await?
        .unwrap_or_default()
        .with_inferred_tiers();
    let results = fuzzy::fuzzy_search(&registry, &args.query, args.max_results);
    maybe_write_output(ctx, &results)?;
    match format {
        OutputFormat::Json => write_json(&results),
        OutputFormat::Jsonl => write_jsonl("models_resolve", &results),
        OutputFormat::Text | OutputFormat::Markdown => {
            if results.is_empty() {
                println!("No matches for '{}'", args.query);
            } else {
                for m in &results {
                    let provider = m.provider.as_deref().unwrap_or("-");
                    let tier_str = m.tier.map(|t| format!("  {t}")).unwrap_or_default();
                    println!(
                        "{:<14}{:<40} (score: {}){tier_str}",
                        provider, m.id, m.score,
                    );
                }
            }
            Ok(())
        }
    }
}

/// Major frontier lab families, in display order.
/// Last reviewed: 2026-03-20. Update when a new frontier lab emerges.
const MAJOR_FAMILIES: &[&str] = &[
    "openai",
    "anthropic",
    "google",
    "x-ai",
    "meta-llama",
    "deepseek",
    "mistralai",
    "qwen",
    "moonshotai",
];

const FAMILY_ALIASES: &[(&str, &str)] = &[
    ("xai", "x-ai"),
    ("meta", "meta-llama"),
    ("moonshot", "moonshotai"),
    ("mistral", "mistralai"),
    ("alibaba", "qwen"),
];

async fn handle_frontier(
    ctx: &AppContext,
    args: ModelsFrontierArgs,
    format: OutputFormat,
) -> Result<()> {
    let registry = registry::load_registry_with_auto_sync(&ctx.client, &ctx.config)
        .await?
        .unwrap_or_default();
    let all_entries = registry.frontier();

    let entries: Vec<_> = if let Some(ref family) = args.family {
        // --family bypasses the default filter.
        let family_lower = normalize_family_name(family);
        all_entries
            .into_iter()
            .filter(|e| e.family.eq_ignore_ascii_case(&family_lower))
            .collect()
    } else if args.all {
        all_entries
    } else {
        // Default: curated major labs in fixed order
        MAJOR_FAMILIES
            .iter()
            .filter_map(|&fam| all_entries.iter().find(|e| e.family == fam).cloned())
            .collect()
    };

    maybe_write_output(ctx, &entries)?;
    match format {
        OutputFormat::Json => write_json(&entries),
        OutputFormat::Jsonl => write_jsonl("models_frontier", &entries),
        OutputFormat::Text | OutputFormat::Markdown => {
            if entries.is_empty() {
                println!(
                    "No frontier models found (registry may be empty — try `yoetz models sync`)"
                );
            } else {
                println!("{:<13}{:<43}{:<9}$/1k-out", "FAMILY", "MODEL", "CTX");
                for e in &entries {
                    let ctx_str = e
                        .model
                        .context_length
                        .map(format_context_length)
                        .unwrap_or_else(|| "-".to_string());
                    let price_str = e
                        .model
                        .pricing
                        .completion_per_1k
                        .map(|p| format!("${:.3}", p))
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "{:<13}{:<43}{:<9}{}",
                        e.family, e.model.id, ctx_str, price_str
                    );
                }
            }
            Ok(())
        }
    }
}

fn normalize_family_name(family: &str) -> String {
    let family_lower = family.to_lowercase();
    FAMILY_ALIASES
        .iter()
        .find_map(|(alias, canonical)| (*alias == family_lower).then(|| (*canonical).to_string()))
        .unwrap_or(family_lower)
}

fn format_context_length(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        format!("{}m", tokens / 1_000_000)
    } else {
        format!("{}k", tokens / 1000)
    }
}

fn filter_registry(registry: &ModelRegistry, args: &ModelsListArgs) -> ModelRegistry {
    let has_filter = args.search.is_some() || args.provider.is_some();
    if !has_filter {
        return registry.clone();
    }

    let provider_lower = args.provider.as_deref().map(|p| p.to_lowercase());

    // When search is provided, use fuzzy scoring for relevance-ordered results.
    // Score all models (no truncation before provider filtering).
    if let Some(ref search) = args.search {
        let score_map: HashMap<String, u32> = registry
            .models
            .iter()
            .filter_map(|entry| {
                let score = fuzzy::score_model(search, &entry.id);
                if score >= 50 {
                    Some((entry.id.clone(), score))
                } else {
                    None
                }
            })
            .collect();

        let mut models: Vec<_> = registry
            .models
            .iter()
            .filter(|m| {
                if !score_map.contains_key(&m.id) {
                    return false;
                }
                if let Some(ref prov) = provider_lower {
                    match m.provider.as_deref() {
                        Some(p) if p.to_lowercase() == *prov => {}
                        _ => return false,
                    }
                }
                true
            })
            .cloned()
            .collect();

        // Sort by fuzzy score (best first)
        models.sort_by(|a, b| {
            let sa = score_map.get(&a.id).copied().unwrap_or(0);
            let sb = score_map.get(&b.id).copied().unwrap_or(0);
            sb.cmp(&sa)
        });

        let mut filtered = ModelRegistry::default();
        filtered.version = registry.version;
        filtered.updated_at = registry.updated_at.clone();
        filtered.models = models;
        filtered.rebuild_index();
        return filtered;
    }

    // Provider-only filter (no search term)
    let models: Vec<_> = registry
        .models
        .iter()
        .filter(|m| {
            if let Some(ref prov) = provider_lower {
                match m.provider.as_deref() {
                    Some(p) if p.to_lowercase() == *prov => {}
                    _ => return false,
                }
            }
            true
        })
        .cloned()
        .collect();
    let mut filtered = ModelRegistry::default();
    filtered.version = registry.version;
    filtered.updated_at = registry.updated_at.clone();
    filtered.models = models;
    filtered.rebuild_index();
    filtered
}

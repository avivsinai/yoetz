use anyhow::Result;

use crate::{maybe_write_output, registry, AppContext, ModelsArgs, ModelsCommand};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};

pub(crate) async fn handle_models(
    ctx: &AppContext,
    args: ModelsArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        ModelsCommand::List => {
            let registry = registry::load_registry_cache()?.unwrap_or_default();
            maybe_write_output(ctx, &registry)?;
            match format {
                OutputFormat::Json => write_json(&registry),
                OutputFormat::Jsonl => write_jsonl("models_list", &registry),
                OutputFormat::Text | OutputFormat::Markdown => {
                    for model in registry.models {
                        println!("{}", model.id);
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
    }
}

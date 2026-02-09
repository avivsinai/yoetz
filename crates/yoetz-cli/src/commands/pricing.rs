use anyhow::Result;

use crate::registry;
use crate::{
    maybe_write_output, normalize_model_name, resolve_registry_model_id, AppContext, PricingArgs,
    PricingCommand,
};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};

pub(crate) async fn handle_pricing(
    ctx: &AppContext,
    args: PricingArgs,
    format: OutputFormat,
) -> Result<()> {
    match args.command {
        PricingCommand::Estimate(e) => {
            let model = normalize_model_name(&e.model);
            let registry = registry::load_registry_cache()?.unwrap_or_default();
            let registry_id = resolve_registry_model_id(None, Some(&model), Some(&registry));
            let estimate = registry::estimate_pricing(
                Some(&registry),
                registry_id.as_deref().unwrap_or(&model),
                e.input_tokens,
                e.output_tokens,
            )?;
            maybe_write_output(ctx, &estimate)?;
            match format {
                OutputFormat::Json => write_json(&estimate),
                OutputFormat::Jsonl => write_jsonl("pricing_estimate", &estimate),
                OutputFormat::Text | OutputFormat::Markdown => {
                    if let Some(cost) = estimate.estimate_usd {
                        println!("Estimated cost: ${:.6}", cost);
                    } else {
                        println!("Estimate unavailable");
                    }
                    Ok(())
                }
            }
        }
    }
}

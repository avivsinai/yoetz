use anyhow::{anyhow, Result};

use crate::{maybe_write_output, render_bundle_md, resolve_prompt, AppContext, BundleArgs};
use yoetz_core::bundle::{build_bundle, BundleOptions};
use yoetz_core::output::{write_json, write_jsonl, OutputFormat};
use yoetz_core::session::{create_session_dir, write_json as write_json_file, write_text};
use yoetz_core::types::{ArtifactPaths, BundleResult};

pub(crate) fn handle_bundle(
    ctx: &AppContext,
    args: BundleArgs,
    format: OutputFormat,
) -> Result<()> {
    let prompt = resolve_prompt(args.prompt, args.prompt_file)?;
    if args.files.is_empty() && !args.all {
        return Err(anyhow!("--files is required unless --all is set"));
    }
    let options = BundleOptions {
        include: args.files,
        exclude: args.exclude,
        max_file_bytes: args.max_file_bytes,
        max_total_bytes: args.max_total_bytes,
        ..Default::default()
    };

    let bundle = build_bundle(&prompt, options)?;
    let session = create_session_dir()?;

    let bundle_json = session.path.join("bundle.json");
    let bundle_md = session.path.join("bundle.md");

    write_json_file(&bundle_json, &bundle)?;
    write_text(&bundle_md, &render_bundle_md(&bundle))?;

    let result = BundleResult {
        id: session.id,
        bundle,
        artifacts: ArtifactPaths {
            session_dir: session.path.to_string_lossy().to_string(),
            bundle_json: Some(bundle_json.to_string_lossy().to_string()),
            bundle_md: Some(bundle_md.to_string_lossy().to_string()),
            response_json: None,
            media_dir: None,
        },
    };

    maybe_write_output(ctx, &result)?;

    match format {
        OutputFormat::Json => write_json(&result),
        OutputFormat::Jsonl => write_jsonl("bundle", &result),
        OutputFormat::Text => {
            println!("Bundle created at {}", result.artifacts.session_dir);
            Ok(())
        }
        OutputFormat::Markdown => {
            println!("Bundle created at `{}`", result.artifacts.session_dir);
            Ok(())
        }
    }
}

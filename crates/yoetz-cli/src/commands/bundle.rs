use anyhow::{bail, Result};
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

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
    let options = BundleOptions {
        include: args.files,
        exclude: args.exclude,
        max_file_bytes: args.max_file_bytes,
        max_total_bytes: args.max_total_bytes,
        include_all: args.all,
        include_hidden: args.include_hidden || args.all,
        ..Default::default()
    };

    let bundle = build_bundle(&prompt, options)?;
    let session = create_session_dir()?;

    let bundle_json = session.path.join("bundle.json");
    let bundle_md = session
        .path
        .join(bundle_file_name(args.name.as_deref(), &prompt)?);

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

const BUNDLE_TIMESTAMP_FORMAT: &[FormatItem<'static>] =
    format_description!("[year][month][day]-[hour][minute][second]Z");

fn bundle_file_name(requested_name: Option<&str>, prompt: &str) -> Result<String> {
    let source = requested_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| prompt.trim());
    let source = source.strip_suffix(".md").unwrap_or(source);
    let stem = slugify_bundle_name(source);
    if stem.is_empty() {
        bail!("bundle name is empty; pass --name with a descriptive name");
    }
    let timestamp = OffsetDateTime::now_utc().format(BUNDLE_TIMESTAMP_FORMAT)?;
    Ok(format!("{stem}_{timestamp}.md"))
}

fn slugify_bundle_name(value: &str) -> String {
    let mut slug = String::new();
    let mut pending_separator = false;
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            pending_separator = false;
            slug.extend(ch.to_lowercase());
        } else {
            pending_separator = true;
        }
        if slug.chars().count() >= 80 {
            break;
        }
    }
    slug.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::bundle_file_name;

    #[test]
    fn bundle_file_name_uses_agent_name_and_timestamp() {
        let name = bundle_file_name(Some("TASE market thesis"), "ignored").unwrap();
        assert!(name.starts_with("tase-market-thesis_"));
        assert!(name.ends_with("Z.md"));
    }

    #[test]
    fn bundle_file_name_derives_a_name_from_prompt() {
        let name = bundle_file_name(None, "Review quarterly bank earnings").unwrap();
        assert!(name.starts_with("review-quarterly-bank-earnings_"));
    }
}

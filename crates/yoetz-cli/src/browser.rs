use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::process::Command;
use std::thread;
use std::time::Duration;

use yoetz_core::output::{write_jsonl_event, OutputFormat};

#[derive(Debug, Deserialize)]
pub struct Recipe {
    pub name: Option<String>,
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Deserialize)]
pub struct RecipeStep {
    pub action: Option<String>,
    pub args: Option<Vec<String>>,
    pub sleep_ms: Option<u64>,
}

pub struct RecipeContext {
    pub bundle_path: Option<String>,
    pub bundle_text: Option<String>,
}

pub fn agent_browser_bin() -> String {
    env::var("YOETZ_AGENT_BROWSER_BIN").unwrap_or_else(|_| "agent-browser".to_string())
}

pub fn run_agent_browser(args: Vec<String>, format: OutputFormat) -> Result<String> {
    let mut cmd = Command::new(agent_browser_bin());
    let mut final_args = args;

    let wants_json = matches!(format, OutputFormat::Json | OutputFormat::Jsonl);
    if wants_json && !final_args.iter().any(|a| a == "--json") {
        final_args.push("--json".to_string());
    }

    let output = cmd
        .args(&final_args)
        .output()
        .with_context(|| "failed to run agent-browser")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("agent-browser failed: {stderr}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn run_recipe(recipe: Recipe, ctx: RecipeContext, format: OutputFormat) -> Result<()> {
    if matches!(format, OutputFormat::Jsonl) {
        if let Some(name) = recipe.name.as_deref() {
            let event = json!({
                "type": "recipe_start",
                "name": name,
            });
            write_jsonl_event(&event)?;
        }
    }

    for (idx, step) in recipe.steps.iter().enumerate() {
        if let Some(ms) = step.sleep_ms {
            thread::sleep(Duration::from_millis(ms));
            continue;
        }

        let action = step
            .action
            .as_ref()
            .ok_or_else(|| anyhow!("recipe step {idx} missing action"))?;
        let commands = expand_step(action, step.args.as_deref(), &ctx)?;

        for args in commands {
            let stdout = run_agent_browser(args.clone(), format)?;

            if matches!(format, OutputFormat::Jsonl) {
                let stdout_value = if matches!(format, OutputFormat::Json | OutputFormat::Jsonl) {
                    parse_stdout_json(&stdout).unwrap_or(Value::String(stdout.clone()))
                } else {
                    Value::String(stdout.clone())
                };
                let event = json!({
                    "type": "browser_step",
                    "index": idx,
                    "action": action,
                    "args": step.args,
                    "stdout": stdout_value,
                });
                write_jsonl_event(&event)?;
            } else {
                print!("{stdout}");
            }
        }
    }

    Ok(())
}

fn parse_stdout_json(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn expand_step(
    action: &str,
    args: Option<&[String]>,
    ctx: &RecipeContext,
) -> Result<Vec<Vec<String>>> {
    let args = args.unwrap_or_default();
    if args.iter().any(|s| s.contains("{{bundle_text}}")) {
        let text = ctx
            .bundle_text
            .as_deref()
            .ok_or_else(|| anyhow!("bundle text requested but no bundle provided"))?;
        return expand_bundle_text_step(action, args, text, ctx);
    }

    let mut command = vec![action.to_string()];
    for arg in args {
        command.push(interpolate(arg, ctx, None));
    }
    Ok(vec![command])
}

fn expand_bundle_text_step(
    action: &str,
    args: &[String],
    text: &str,
    ctx: &RecipeContext,
) -> Result<Vec<Vec<String>>> {
    const CHUNK_BYTES: usize = 4000;
    let chunks = chunk_text(text, CHUNK_BYTES);
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    if action == "find" {
        if args.len() < 4 {
            return Err(anyhow!(
                "find step requires locator, value, action, and text"
            ));
        }
        let locator = interpolate(&args[0], ctx, None);
        let value = interpolate(&args[1], ctx, None);
        let mut commands = Vec::new();

        let first = vec![
            action.to_string(),
            locator.clone(),
            value.clone(),
            "fill".to_string(),
            chunks[0].clone(),
        ];
        commands.push(first);

        for chunk in chunks.iter().skip(1) {
            commands.push(vec![
                action.to_string(),
                locator.clone(),
                value.clone(),
                "type".to_string(),
                chunk.clone(),
            ]);
        }

        return Ok(commands);
    }

    if action == "fill" || action == "type" {
        if args.len() < 2 {
            return Err(anyhow!("{action} step requires selector and text"));
        }
        let selector = interpolate(&args[0], ctx, None);
        let mut commands = Vec::new();
        commands.push(vec![
            action.to_string(),
            selector.clone(),
            chunks[0].clone(),
        ]);
        for chunk in chunks.iter().skip(1) {
            commands.push(vec!["type".to_string(), selector.clone(), chunk.clone()]);
        }
        return Ok(commands);
    }

    let mut command = vec![action.to_string()];
    for arg in args {
        command.push(interpolate(arg, ctx, Some(text)));
    }
    Ok(vec![command])
}

fn chunk_text(text: &str, max_bytes: usize) -> Vec<String> {
    if text.is_empty() || max_bytes == 0 {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut end = (start + max_bytes).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            break;
        }
        chunks.push(text[start..end].to_string());
        start = end;
    }
    chunks
}

fn interpolate(value: &str, ctx: &RecipeContext, bundle_text: Option<&str>) -> String {
    let mut out = value.to_string();
    if let Some(path) = &ctx.bundle_path {
        out = out.replace("{{bundle_path}}", path);
    }
    if let Some(text) = bundle_text {
        out = out.replace("{{bundle_text}}", text);
    }
    out
}

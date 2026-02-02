use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;
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
    pub action: String,
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

        let mut args = vec![step.action.clone()];
        if let Some(step_args) = &step.args {
            let expanded: Vec<String> = step_args
                .iter()
                .map(|s| interpolate(s, &ctx))
                .collect();
            args.extend(expanded);
        }

        let stdout = run_agent_browser(args.clone(), format)?;

        if matches!(format, OutputFormat::Jsonl) {
            let event = json!({
                "type": "browser_step",
                "index": idx,
                "action": step.action,
                "args": step.args,
                "stdout": stdout,
            });
            write_jsonl_event(&event)?;
        } else {
            print!("{stdout}");
        }
    }

    Ok(())
}

fn interpolate(value: &str, ctx: &RecipeContext) -> String {
    let mut out = value.to_string();
    if let Some(path) = &ctx.bundle_path {
        out = out.replace("{{bundle_path}}", path);
    }
    if let Some(text) = &ctx.bundle_text {
        out = out.replace("{{bundle_text}}", text);
    }
    out
}

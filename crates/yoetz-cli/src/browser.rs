use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use yoetz_core::config::Config;
use yoetz_core::output::{write_json, write_jsonl_event, OutputFormat};
use yoetz_core::paths::home_dir;

/// Realistic Chrome user agent to avoid automation detection
const STEALTH_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Browser launch args to disable automation detection
const STEALTH_ARGS: &str = "--disable-blink-features=AutomationControlled";

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
    pub profile_dir: Option<PathBuf>,
    pub use_stealth: bool,
    pub headed: bool,
}

/// Returns (program, extra_prefix_args) for launching agent-browser.
/// Checks YOETZ_AGENT_BROWSER_BIN env, then PATH, then falls back to npx.
fn resolve_agent_browser() -> (String, Vec<String>) {
    if let Ok(bin) = env::var("YOETZ_AGENT_BROWSER_BIN") {
        return (bin, vec![]);
    }
    // Check if agent-browser is in PATH
    if Command::new("agent-browser")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
    {
        return ("agent-browser".to_string(), vec![]);
    }
    // Fall back to npx (handles npm cache / npx-installed packages)
    (
        "npx".to_string(),
        vec!["--yes".to_string(), "agent-browser".to_string()],
    )
}

pub fn run_agent_browser(
    args: Vec<String>,
    format: OutputFormat,
    profile_dir: Option<&Path>,
) -> Result<String> {
    run_agent_browser_with_options(
        args,
        format,
        profile_dir,
        /* use_stealth */ true,
        /* headed */ false,
    )
}

/// Run agent-browser with optional stealth mode and headed display
pub fn run_agent_browser_with_options(
    args: Vec<String>,
    format: OutputFormat,
    profile_dir: Option<&Path>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let (bin, prefix_args) = resolve_agent_browser();
    let mut cmd = Command::new(&bin);
    let mut final_args = args;

    if headed && !final_args.iter().any(|a| a == "--headed") {
        final_args.insert(0, "--headed".to_string());
    }

    if let Some(profile_dir) = profile_dir {
        let state_path = state_file(profile_dir);

        // Use --state if state.json exists and stealth is enabled (for cookie injection)
        // Otherwise use --profile (for persistent browser data)
        // Note: --state and --profile are mutually exclusive in agent-browser
        if use_stealth && state_path.exists() {
            if !final_args
                .iter()
                .any(|a| a == "--state" || a.starts_with("--state="))
            {
                final_args.insert(0, state_path.to_string_lossy().to_string());
                final_args.insert(0, "--state".to_string());
            }
        } else if !final_args
            .iter()
            .any(|a| a == "--profile" || a.starts_with("--profile="))
        {
            final_args.insert(0, profile_dir.to_string_lossy().to_string());
            final_args.insert(0, "--profile".to_string());
        }
    }

    // Apply stealth options to avoid automation detection
    if use_stealth {
        if !final_args
            .iter()
            .any(|a| a == "--user-agent" || a.starts_with("--user-agent="))
        {
            final_args.insert(0, STEALTH_USER_AGENT.to_string());
            final_args.insert(0, "--user-agent".to_string());
        }
        if !final_args
            .iter()
            .any(|a| a == "--args" || a.starts_with("--args="))
        {
            final_args.insert(0, STEALTH_ARGS.to_string());
            final_args.insert(0, "--args".to_string());
        }
    }

    let wants_json = matches!(format, OutputFormat::Json | OutputFormat::Jsonl);
    if wants_json && !final_args.iter().any(|a| a == "--json") {
        final_args.push("--json".to_string());
    }

    let mut all_args = prefix_args;
    all_args.extend(final_args);

    let output = cmd
        .args(&all_args)
        .output()
        .with_context(|| format!("failed to run agent-browser (via {bin})"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.is_empty() {
            stderr.to_string()
        } else if !stdout.is_empty() {
            stdout.to_string()
        } else {
            format!("exit code {:?}", output.status.code())
        };
        return Err(anyhow!("agent-browser failed: {detail}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn run_recipe(recipe: Recipe, ctx: RecipeContext, format: OutputFormat) -> Result<()> {
    // Close daemon to ensure stealth options are applied fresh
    let _ = close_browser();

    let wants_json = matches!(format, OutputFormat::Json);
    let wants_jsonl = matches!(format, OutputFormat::Jsonl);
    let mut events: Vec<Value> = Vec::new();

    if wants_jsonl {
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
            let stdout = run_agent_browser_with_options(
                args.clone(),
                format,
                ctx.profile_dir.as_deref(),
                ctx.use_stealth,
                ctx.headed,
            )?;

            if wants_json || wants_jsonl {
                let stdout_value =
                    parse_stdout_json(&stdout).unwrap_or(Value::String(stdout.clone()));
                let event = json!({
                    "type": "browser_step",
                    "index": idx,
                    "action": action,
                    "args": step.args,
                    "stdout": stdout_value,
                });
                if wants_jsonl {
                    write_jsonl_event(&event)?;
                } else {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
        }
    }

    if wants_json {
        let payload = json!({
            "name": recipe.name,
            "steps": events,
        });
        write_json(&payload)?;
    }

    Ok(())
}

pub fn resolve_profile_dir(config: &Config, override_profile: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(path) = override_profile {
        return expand_tilde(path);
    }
    if let Ok(path) = env::var("YOETZ_BROWSER_PROFILE") {
        return expand_tilde(Path::new(&path));
    }
    if let Some(path) = config.defaults.browser_profile.as_deref() {
        return expand_tilde(Path::new(path));
    }
    default_profile_dir()
}

fn default_profile_dir() -> Result<PathBuf> {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("yoetz").join("browser-profile"));
    }
    if let Some(home) = home_dir() {
        return Ok(home.join(".config").join("yoetz").join("browser-profile"));
    }
    Err(anyhow!(
        "unable to determine browser profile dir (set YOETZ_BROWSER_PROFILE)"
    ))
}

fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" || raw.starts_with("~/") {
        let home = home_dir().ok_or_else(|| anyhow!("unable to resolve home directory"))?;
        if raw == "~" {
            return Ok(home);
        }
        return Ok(home.join(raw.trim_start_matches("~/")));
    }
    Ok(path.to_path_buf())
}

pub fn login(profile_dir: &Path) -> Result<()> {
    fs::create_dir_all(profile_dir).with_context(|| format!("create {}", profile_dir.display()))?;
    // Close any existing daemon to ensure fresh options
    let _ = close_browser();
    let args = vec!["open".to_string(), "https://chatgpt.com/".to_string()];
    let _ = run_agent_browser_with_options(
        args,
        OutputFormat::Text,
        Some(profile_dir),
        /* use_stealth */ true,
        /* headed */ true,
    )?;
    Ok(())
}

/// Close the agent-browser daemon to ensure fresh options on next launch.
pub fn close_browser() -> Result<()> {
    let (bin, prefix_args) = resolve_agent_browser();
    let mut cmd = Command::new(bin);
    cmd.args(prefix_args);
    let _ = cmd.arg("close").output();
    // Give the daemon time to fully shutdown before starting new commands
    thread::sleep(Duration::from_millis(1000));
    Ok(())
}

pub fn state_file(profile_dir: &Path) -> PathBuf {
    profile_dir.join("state.json")
}

pub fn maybe_load_state(profile_dir: &Path, use_stealth: bool) -> Result<bool> {
    let state_path = state_file(profile_dir);
    if !state_path.exists() {
        return Ok(false);
    }
    let _ = run_agent_browser_with_options(
        vec![
            "state".to_string(),
            "load".to_string(),
            state_path.to_string_lossy().to_string(),
        ],
        OutputFormat::Text,
        Some(profile_dir),
        use_stealth,
        /* headed */ false,
    )?;
    Ok(true)
}

/// Sync cookies from real Chrome to agent-browser state file.
/// This extracts cookies from your logged-in Chrome session and saves them
/// in Playwright storageState format for agent-browser to use.
pub fn sync_cookies(profile_dir: &Path) -> Result<(usize, Vec<String>)> {
    fs::create_dir_all(profile_dir).with_context(|| format!("create {}", profile_dir.display()))?;

    let state_file = state_file(profile_dir);

    // Find the extract script relative to the yoetz binary or in known locations
    let script_path = find_extract_script()?;

    let output = Command::new("node")
        .arg(&script_path)
        .arg("--output")
        .arg(&state_file)
        .output()
        .with_context(|| "failed to run extract-cookies.mjs")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "cookie extraction failed: {stderr}\n\nMake sure Node >=22 is installed and `npm install -g @steipete/sweet-cookie`. Then log into ChatGPT in Chrome and try again."
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    let warnings = parsed
        .get("warnings")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let cookie_count = parsed
        .get("cookies")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);

    Ok((cookie_count, warnings))
}

fn find_extract_script() -> Result<PathBuf> {
    // Check YOETZ_SCRIPTS_DIR env var
    if let Ok(dir) = env::var("YOETZ_SCRIPTS_DIR") {
        let path = PathBuf::from(dir).join("extract-cookies.mjs");
        if path.exists() {
            return Ok(path);
        }
    }

    // Check relative to current exe
    if let Ok(exe) = env::current_exe() {
        // Try ../../../scripts/ (for development: target/debug/yoetz -> scripts/)
        if let Some(parent) = exe.parent() {
            for ancestor in [
                parent.join("scripts"),
                parent.join("../scripts"),
                parent.join("../../scripts"),
                parent.join("../../../scripts"),
            ] {
                let path = ancestor.join("extract-cookies.mjs");
                if path.exists() {
                    return Ok(path.canonicalize()?);
                }
            }
        }
    }

    // Check in XDG data dir
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        let path = PathBuf::from(xdg)
            .join("yoetz")
            .join("scripts")
            .join("extract-cookies.mjs");
        if path.exists() {
            return Ok(path);
        }
    }

    // Check ~/.local/share/yoetz/scripts/
    if let Some(home) = home_dir() {
        let path = home
            .join(".local")
            .join("share")
            .join("yoetz")
            .join("scripts")
            .join("extract-cookies.mjs");
        if path.exists() {
            return Ok(path);
        }
    }

    Err(anyhow!(
        "extract-cookies.mjs not found. Set YOETZ_SCRIPTS_DIR or install yoetz properly."
    ))
}

pub fn check_auth(profile_dir: &Path, headed: bool) -> Result<()> {
    if !profile_dir.exists() {
        return Err(anyhow!(
            "browser profile not found at {}. Run `yoetz browser login` to authenticate.",
            profile_dir.display()
        ));
    }
    // Close daemon to ensure stealth options are applied fresh
    let _ = close_browser();
    // State is now loaded via --state flag in run_agent_browser_with_options
    let _ = run_agent_browser_with_options(
        vec!["open".to_string(), "https://chatgpt.com/".to_string()],
        OutputFormat::Text,
        Some(profile_dir),
        /* use_stealth */ true,
        headed,
    )?;
    thread::sleep(Duration::from_millis(2500));
    let snapshot = run_agent_browser_with_options(
        vec![
            "snapshot".to_string(),
            "-i".to_string(),
            "-c".to_string(),
            "--json".to_string(),
        ],
        OutputFormat::Json,
        Some(profile_dir),
        /* use_stealth */ true,
        headed,
    )?;

    if let Some(issue) = detect_auth_issue(&snapshot) {
        return Err(anyhow!("{issue}"));
    }

    Ok(())
}

fn detect_auth_issue(snapshot: &str) -> Option<&'static str> {
    let haystack = snapshot.to_lowercase();
    let login_markers = [
        "log in",
        "login",
        "sign in",
        "sign up",
        "create account",
        "continue with google",
        "continue with microsoft",
        "continue with apple",
    ];
    let challenge_markers = [
        "cloudflare",
        "checking your browser",
        "attention required",
        "security check",
        "just a moment",
        "verify you are human",
        "cf-chl",
    ];

    if contains_any(&haystack, &challenge_markers) {
        return Some(
            "cloudflare challenge detected. Run `yoetz browser sync-cookies` or `yoetz browser login` and try again.",
        );
    }
    if contains_any(&haystack, &login_markers) {
        return Some("chatgpt login required. Run `yoetz browser login` and try again.");
    }
    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
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
        let first_action = interpolate(&args[2], ctx, None);
        let follow_action = if first_action == "fill" {
            "type".to_string()
        } else {
            first_action.clone()
        };
        let mut commands = Vec::new();

        let first = vec![
            action.to_string(),
            locator.clone(),
            value.clone(),
            first_action,
            chunks[0].clone(),
        ];
        commands.push(first);

        for chunk in chunks.iter().skip(1) {
            commands.push(vec![
                action.to_string(),
                locator.clone(),
                value.clone(),
                follow_action.clone(),
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

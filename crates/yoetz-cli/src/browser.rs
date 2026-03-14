use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use yoetz_core::config::Config;
use yoetz_core::output::{write_json, write_jsonl_event, OutputFormat};
use yoetz_core::paths::home_dir;

/// Realistic Chrome user agent to avoid automation detection
const STEALTH_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Browser launch args to disable automation detection
const STEALTH_ARGS: &str = "--disable-blink-features=AutomationControlled";
const COOKIE_SYNC_TIMEOUT_MS: &str = "30000";
const AUTH_CHECK_TIMEOUT_MS: u64 = 8_000;
const AUTH_CHECK_POLL_MS: u64 = 500;
const COOKIE_SYNC_NODE_MIN_VERSION: NodeVersion = NodeVersion {
    major: 24,
    minor: 4,
    patch: 0,
};
const MACOS_KEYCHAIN_GUIDANCE: &str =
    "If macOS shows a Keychain prompt for Chrome Safe Storage, click Always Allow.";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct NodeVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

#[derive(Debug, Deserialize)]
pub struct Recipe {
    pub name: Option<String>,
    pub defaults: Option<BTreeMap<String, String>>,
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
    pub profile_mode: BrowserProfileMode,
    pub use_stealth: bool,
    pub headed: bool,
    pub vars: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserProfileMode {
    PreferState,
    ProfileOnly,
}

/// Cached agent-browser resolution. Probed once per process, reused for all calls.
static AGENT_BROWSER: OnceLock<(String, Vec<String>)> = OnceLock::new();

/// Returns (program, extra_prefix_args) for launching agent-browser.
/// Checks YOETZ_AGENT_BROWSER_BIN env, then PATH, then falls back to npx.
/// Result is cached for the lifetime of the process.
fn resolve_agent_browser() -> (String, Vec<String>) {
    let cached = AGENT_BROWSER.get_or_init(|| {
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
    });
    cached.clone()
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
        BrowserProfileMode::PreferState,
    )
}

/// Run agent-browser with optional stealth mode and headed display
fn run_agent_browser_with_options(
    args: Vec<String>,
    format: OutputFormat,
    profile_dir: Option<&Path>,
    use_stealth: bool,
    headed: bool,
    profile_mode: BrowserProfileMode,
) -> Result<String> {
    let (bin, prefix_args) = resolve_agent_browser();
    let mut cmd = Command::new(&bin);
    let mut final_args = args;

    if headed && !final_args.iter().any(|a| a == "--headed") {
        final_args.insert(0, "--headed".to_string());
    }

    if let Some(profile_dir) = profile_dir {
        add_profile_args(
            &mut final_args,
            profile_dir,
            use_stealth,
            profile_mode,
            state_file(profile_dir).exists(),
        );
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
                ctx.profile_mode,
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
    clear_state_file(profile_dir)?;
    // Close any existing daemon to ensure fresh options
    let _ = close_browser();
    let args = vec!["open".to_string(), "https://chatgpt.com/".to_string()];
    let _ = run_agent_browser_with_options(
        args,
        OutputFormat::Text,
        Some(profile_dir),
        /* use_stealth */ true,
        /* headed */ true,
        BrowserProfileMode::ProfileOnly,
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

pub fn clear_state_file(profile_dir: &Path) -> Result<()> {
    let state_path = state_file(profile_dir);
    if !state_path.exists() {
        return Ok(());
    }
    fs::remove_file(&state_path)
        .with_context(|| format!("remove stale {}", state_path.display()))?;
    Ok(())
}

/// Sync cookies from real Chrome to agent-browser state file.
/// This extracts cookies from your logged-in Chrome session and saves them
/// in Playwright storageState format for agent-browser to use.
pub fn sync_cookies(profile_dir: &Path) -> Result<(usize, Vec<String>)> {
    fs::create_dir_all(profile_dir).with_context(|| format!("create {}", profile_dir.display()))?;
    ensure_supported_node_for_cookie_sync()?;

    let state_file = state_file(profile_dir);

    // Find the extract script relative to the yoetz binary or in known locations
    let script_path = find_extract_script()?;

    let args = cookie_sync_script_args(&script_path, &state_file);
    let output = Command::new("node")
        .args(&args)
        .output()
        .with_context(|| "failed to run extract-cookies.mjs")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "cookie extraction failed: {stderr}\n\nMake sure Node >=24.4 is installed. Release/Homebrew installs bundle the cookie extractor dependency; if you're running from a source checkout, run `npm ci --prefix scripts`. Then log into ChatGPT in Chrome and try again."
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

    if let Err(err) = validate_cookie_sync_result(cookie_count, &warnings) {
        let _ = fs::remove_file(&state_file);
        return Err(err);
    }

    Ok((cookie_count, warnings))
}

pub fn cookie_sync_guidance() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some(MACOS_KEYCHAIN_GUIDANCE)
    } else {
        None
    }
}

pub fn build_recipe_vars(
    defaults: Option<&BTreeMap<String, String>>,
    entries: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut vars = defaults.cloned().unwrap_or_default();
    for entry in entries {
        let (key, value) = parse_recipe_var(entry)?;
        vars.insert(key, value);
    }
    Ok(vars)
}

/// Search for a yoetz data file (script or recipe) across standard locations.
/// Order: YOETZ_SCRIPTS_DIR env, relative to exe, Homebrew share, XDG, ~/.local/share.
fn find_data_file(subdir: &str, filename: &str) -> Result<PathBuf> {
    // Check YOETZ_SCRIPTS_DIR env var (legacy, works for scripts)
    if subdir == "scripts" {
        if let Ok(dir) = env::var("YOETZ_SCRIPTS_DIR") {
            let path = PathBuf::from(dir).join(filename);
            if path.exists() {
                return Ok(path);
            }
        }
    }

    // Check relative to current exe (development builds)
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            for ancestor in [
                parent.join(subdir),
                parent.join(format!("../{subdir}")),
                parent.join(format!("../../{subdir}")),
                parent.join(format!("../../../{subdir}")),
            ] {
                let path = ancestor.join(filename);
                if path.exists() {
                    return Ok(path.canonicalize()?);
                }
            }

            // Check Homebrew share dir (relative to exe: ../share/yoetz/)
            let brew_share = parent.join("../share/yoetz").join(subdir).join(filename);
            if brew_share.exists() {
                return Ok(brew_share.canonicalize()?);
            }
        }
    }

    // Check well-known Homebrew prefixes
    for prefix in ["/opt/homebrew/share/yoetz", "/usr/local/share/yoetz"] {
        let path = PathBuf::from(prefix).join(subdir).join(filename);
        if path.exists() {
            return Ok(path);
        }
    }

    // Check in XDG data dir
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        let path = PathBuf::from(xdg).join("yoetz").join(subdir).join(filename);
        if path.exists() {
            return Ok(path);
        }
    }

    // Check ~/.local/share/yoetz/
    if let Some(home) = home_dir() {
        let path = home
            .join(".local")
            .join("share")
            .join("yoetz")
            .join(subdir)
            .join(filename);
        if path.exists() {
            return Ok(path);
        }
    }

    Err(anyhow!(
        "{filename} not found in {subdir}/. Set YOETZ_SCRIPTS_DIR or reinstall yoetz (brew reinstall yoetz)."
    ))
}

fn find_extract_script() -> Result<PathBuf> {
    find_data_file("scripts", "extract-cookies.mjs")
}

fn cookie_sync_script_args(script_path: &Path, state_file: &Path) -> Vec<String> {
    vec![
        script_path.to_string_lossy().to_string(),
        "--output".to_string(),
        state_file.to_string_lossy().to_string(),
        "--timeout-ms".to_string(),
        COOKIE_SYNC_TIMEOUT_MS.to_string(),
        "--browsers".to_string(),
        "chrome".to_string(),
    ]
}

fn ensure_supported_node_for_cookie_sync() -> Result<()> {
    let output = Command::new("node")
        .arg("--version")
        .output()
        .with_context(|| {
            "failed to run `node --version` (browser cookie sync requires Node 24.4+)"
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "browser cookie sync requires Node 24.4+, but `node --version` failed: {}",
            stderr.trim()
        ));
    }

    let version = parse_node_version(String::from_utf8_lossy(&output.stdout).trim())
        .ok_or_else(|| anyhow!("could not parse Node version from `node --version` output"))?;

    if node_version_supported(version) {
        return Ok(());
    }

    Err(anyhow!(
        "browser cookie sync requires Node 24.4+ because Chrome cookie timestamps overflow older node:sqlite builds. Detected Node {}.{}.{}.\n\nUpgrade Node and retry.",
        version.major,
        version.minor,
        version.patch
    ))
}

fn parse_node_version(raw: &str) -> Option<NodeVersion> {
    let trimmed = raw.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some(NodeVersion {
        major,
        minor,
        patch,
    })
}

fn node_version_supported(version: NodeVersion) -> bool {
    version >= COOKIE_SYNC_NODE_MIN_VERSION
}

fn validate_cookie_sync_result(cookie_count: usize, warnings: &[String]) -> Result<()> {
    if cookie_count > 0 {
        return Ok(());
    }

    let mut message =
        "Chrome cookie sync found 0 cookies. Make sure ChatGPT is logged into Chrome, then fully quit Chrome and try again.".to_string();
    if let Some(guidance) = cookie_sync_guidance() {
        message.push_str("\n\n");
        message.push_str(guidance);
    }
    if !warnings.is_empty() {
        message.push_str("\n\nWarnings: ");
        message.push_str(&warnings.join("; "));
    }

    Err(anyhow!(message))
}

/// Resolve a recipe path. If the path exists as-is, use it. Otherwise treat it
/// as a recipe name and search in standard locations (e.g. "chatgpt" -> "chatgpt.yaml").
pub fn resolve_recipe(path: &Path) -> Result<PathBuf> {
    // Absolute or relative path that exists — use directly
    if path.exists() {
        return Ok(path.to_path_buf());
    }

    // Treat as a recipe name: try with .yaml extension
    let name = path.to_string_lossy();
    let filename = if name.ends_with(".yaml") || name.ends_with(".yml") {
        name.to_string()
    } else {
        format!("{name}.yaml")
    };

    find_data_file("recipes", &filename)
}

pub fn resolve_auth_mode(profile_dir: &Path, headed: bool) -> Result<BrowserProfileMode> {
    if !profile_dir.exists() {
        return Err(anyhow!(
            "browser profile not found at {}. Run `yoetz browser login` to authenticate.",
            profile_dir.display()
        ));
    }
    if state_file(profile_dir).exists()
        && check_auth_with_mode(profile_dir, headed, BrowserProfileMode::PreferState).is_ok()
    {
        return Ok(BrowserProfileMode::PreferState);
    }
    check_auth_with_mode(profile_dir, headed, BrowserProfileMode::ProfileOnly)?;
    Ok(BrowserProfileMode::ProfileOnly)
}

pub fn check_auth(profile_dir: &Path, headed: bool) -> Result<()> {
    let _ = resolve_auth_mode(profile_dir, headed)?;
    Ok(())
}

fn check_auth_with_mode(
    profile_dir: &Path,
    headed: bool,
    profile_mode: BrowserProfileMode,
) -> Result<()> {
    // Close daemon to ensure stealth options are applied fresh
    let _ = close_browser();
    let _ = run_agent_browser_with_options(
        vec!["open".to_string(), "https://chatgpt.com/".to_string()],
        OutputFormat::Text,
        Some(profile_dir),
        /* use_stealth */ true,
        headed,
        profile_mode,
    )?;
    let deadline = Instant::now() + Duration::from_millis(AUTH_CHECK_TIMEOUT_MS);
    let mut last_issue: Option<&'static str>;
    loop {
        let snapshot = run_agent_browser_with_options(
            vec![
                "snapshot".to_string(),
                "-c".to_string(),
                "--json".to_string(),
            ],
            OutputFormat::Json,
            Some(profile_dir),
            /* use_stealth */ true,
            headed,
            profile_mode,
        )?;

        last_issue = detect_auth_issue(&snapshot);

        // Positive confirmation: page loaded and no auth issues detected
        if last_issue.is_none() && looks_authenticated(&snapshot) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(AUTH_CHECK_POLL_MS));
    }

    if let Some(issue) = last_issue {
        return Err(anyhow!("{issue}"));
    }
    Err(anyhow!(
        "auth check timed out without confirming authentication. \
         The page may still be loading. Try again or run `yoetz browser login`."
    ))
}

fn add_profile_args(
    final_args: &mut Vec<String>,
    profile_dir: &Path,
    use_stealth: bool,
    profile_mode: BrowserProfileMode,
    state_exists: bool,
) {
    let wants_state =
        matches!(profile_mode, BrowserProfileMode::PreferState) && use_stealth && state_exists;

    if wants_state {
        if !final_args
            .iter()
            .any(|a| a == "--state" || a.starts_with("--state="))
        {
            final_args.insert(0, state_file(profile_dir).to_string_lossy().to_string());
            final_args.insert(0, "--state".to_string());
        }
        return;
    }

    if !final_args
        .iter()
        .any(|a| a == "--profile" || a.starts_with("--profile="))
    {
        final_args.insert(0, profile_dir.to_string_lossy().to_string());
        final_args.insert(0, "--profile".to_string());
    }
}

/// Positive confirmation that the page is authenticated (ChatGPT loaded successfully).
fn looks_authenticated(snapshot: &str) -> bool {
    let haystack = snapshot.to_lowercase();
    let positive_markers = ["chatgpt", "new chat", "send a message", "message chatgpt"];
    contains_any(&haystack, &positive_markers)
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
    if args.iter().any(|s| s.contains("{{bundle_path}}")) && ctx.bundle_path.is_none() {
        return Err(anyhow!("bundle path requested but no bundle provided"));
    }

    let mut command = vec![action.to_string()];
    for arg in args {
        command.push(interpolate(arg, ctx, None)?);
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
        let locator = interpolate(&args[0], ctx, None)?;
        let value = interpolate(&args[1], ctx, None)?;
        let first_action = interpolate(&args[2], ctx, None)?;
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
        let selector = interpolate(&args[0], ctx, None)?;
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
        command.push(interpolate(arg, ctx, Some(text))?);
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

fn interpolate(value: &str, ctx: &RecipeContext, bundle_text: Option<&str>) -> Result<String> {
    let mut out = value.to_string();
    if let Some(path) = &ctx.bundle_path {
        out = out.replace("{{bundle_path}}", path);
    }
    if let Some(text) = bundle_text {
        out = out.replace("{{bundle_text}}", text);
    }
    for (key, value) in &ctx.vars {
        let needle = format!("{{{{{key}}}}}");
        out = out.replace(&needle, value);
    }
    if let Some(var) = first_unresolved_placeholder(&out) {
        return Err(anyhow!(
            "recipe variable {{{{{var}}}}} not provided. Pass `--var {var}=...` or add it under `defaults:` in the recipe."
        ));
    }
    Ok(out)
}

fn parse_recipe_var(entry: &str) -> Result<(String, String)> {
    let (key, value) = entry
        .split_once('=')
        .ok_or_else(|| anyhow!("invalid --var `{entry}` (expected KEY=VALUE)"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(anyhow!("invalid --var `{entry}` (key cannot be empty)"));
    }
    Ok((key.to_string(), value.to_string()))
}

fn first_unresolved_placeholder(value: &str) -> Option<&str> {
    let start = value.find("{{")?;
    let rest = &value[start + 2..];
    let end = rest.find("}}")?;
    let placeholder = rest[..end].trim();
    if placeholder.is_empty() {
        None
    } else {
        Some(placeholder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe_context() -> RecipeContext {
        RecipeContext {
            bundle_path: Some("/tmp/bundle.md".to_string()),
            bundle_text: Some("hello world".to_string()),
            profile_dir: None,
            profile_mode: BrowserProfileMode::ProfileOnly,
            use_stealth: true,
            headed: false,
            vars: BTreeMap::from([("model".to_string(), "gpt-5-4-pro".to_string())]),
        }
    }

    #[test]
    fn cookie_sync_script_args_include_timeout_and_chrome_only() {
        let args = cookie_sync_script_args(
            Path::new("/tmp/extract-cookies.mjs"),
            Path::new("/tmp/state.json"),
        );
        assert_eq!(
            args,
            vec![
                "/tmp/extract-cookies.mjs",
                "--output",
                "/tmp/state.json",
                "--timeout-ms",
                "30000",
                "--browsers",
                "chrome",
            ]
        );
    }

    #[test]
    fn parse_node_version_accepts_v_prefix() {
        let version = parse_node_version("v24.14.0").unwrap();
        assert_eq!(
            version,
            NodeVersion {
                major: 24,
                minor: 14,
                patch: 0,
            }
        );
    }

    #[test]
    fn node_version_supported_requires_24_4_or_newer() {
        assert!(!node_version_supported(NodeVersion {
            major: 24,
            minor: 3,
            patch: 0,
        }));
        assert!(node_version_supported(NodeVersion {
            major: 24,
            minor: 4,
            patch: 0,
        }));
    }

    #[test]
    fn zero_cookie_sync_is_an_error() {
        let warnings = vec!["timed out".to_string()];
        let err = validate_cookie_sync_result(0, &warnings).unwrap_err();
        assert!(err.to_string().contains("0 cookies"));
        assert!(err.to_string().contains("timed out"));
    }

    #[test]
    fn build_recipe_vars_merges_defaults_and_cli_overrides() {
        let defaults = BTreeMap::from([
            ("model".to_string(), "gpt-5-4-pro".to_string()),
            ("theme".to_string(), "light".to_string()),
        ]);
        let vars = build_recipe_vars(
            Some(&defaults),
            &["model=gpt-5-2-pro".to_string(), "mode=fast".to_string()],
        )
        .unwrap();
        assert_eq!(vars.get("model").map(String::as_str), Some("gpt-5-2-pro"));
        assert_eq!(vars.get("theme").map(String::as_str), Some("light"));
        assert_eq!(vars.get("mode").map(String::as_str), Some("fast"));
    }

    #[test]
    fn interpolate_replaces_bundle_and_recipe_vars() {
        let ctx = recipe_context();
        let value = interpolate("open {{bundle_path}} {{model}}", &ctx, Some("ignored")).unwrap();
        assert_eq!(value, "open /tmp/bundle.md gpt-5-4-pro");
    }

    #[test]
    fn interpolate_errors_on_missing_recipe_var() {
        let ctx = recipe_context();
        let err = interpolate("{{missing}}", &ctx, None).unwrap_err();
        assert!(err.to_string().contains("--var missing="));
    }

    #[test]
    fn profile_only_mode_uses_persistent_profile_even_if_state_exists() {
        let mut args = vec!["open".to_string(), "https://chatgpt.com/".to_string()];
        add_profile_args(
            &mut args,
            Path::new("/tmp/browser-profile"),
            true,
            BrowserProfileMode::ProfileOnly,
            true,
        );
        assert_eq!(args[0], "--profile");
        assert_eq!(args[1], "/tmp/browser-profile");
        assert!(!args.iter().any(|arg| arg == "--state"));
    }

    #[test]
    fn prefer_state_mode_keeps_cookie_sync_snapshot_path() {
        let mut args = vec!["open".to_string(), "https://chatgpt.com/".to_string()];
        add_profile_args(
            &mut args,
            Path::new("/tmp/browser-profile"),
            true,
            BrowserProfileMode::PreferState,
            true,
        );
        assert_eq!(args[0], "--state");
        assert_eq!(args[1], "/tmp/browser-profile/state.json");
    }

    #[test]
    fn prefer_state_no_stealth_falls_back_to_profile() {
        let mut args = vec!["open".to_string()];
        add_profile_args(
            &mut args,
            Path::new("/tmp/bp"),
            false,
            BrowserProfileMode::PreferState,
            true,
        );
        assert_eq!(args[0], "--profile");
        assert!(!args.iter().any(|arg| arg == "--state"));
    }

    #[test]
    fn prefer_state_no_state_file_falls_back_to_profile() {
        let mut args = vec!["open".to_string()];
        add_profile_args(
            &mut args,
            Path::new("/tmp/bp"),
            true,
            BrowserProfileMode::PreferState,
            false,
        );
        assert_eq!(args[0], "--profile");
        assert!(!args.iter().any(|arg| arg == "--state"));
    }

    #[test]
    fn looks_authenticated_detects_chatgpt() {
        assert!(looks_authenticated(r#"{"text": "ChatGPT - New chat"}"#));
        assert!(looks_authenticated(r#"{"text": "Send a message"}"#));
        assert!(!looks_authenticated(r#"{"text": "Loading..."}"#));
        assert!(!looks_authenticated(""));
    }
}

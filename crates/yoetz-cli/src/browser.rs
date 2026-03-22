use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, IsTerminal};
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
const LIVE_ATTACH_AUTH_CHECK_TIMEOUT_MS: u64 = 30_000;
const AUTH_CHECK_POLL_MS: u64 = 500;
const CHATGPT_WAIT_ACTION: &str = "chatgpt_wait_response";
const CHATGPT_POLL_ATTEMPTS_DEFAULT: usize = 30;
const CHATGPT_POLL_INTERVAL_MS_DEFAULT: u64 = 15_000;
const CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT: u64 = 10_000;
const CDP_SESSION_NAME: &str = "yoetz-cdp";
pub const CHATGPT_URL: &str = "https://chatgpt.com/";
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
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: Option<String>,
    pub defaults: Option<BTreeMap<String, String>>,
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeStep {
    pub action: Option<String>,
    pub args: Option<Vec<String>>,
    pub sleep_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
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

/// How yoetz connects to a browser for authenticated sessions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserConnection {
    /// Connected to a live Chrome instance via CDP with explicit endpoint.
    Cdp { endpoint: String },
    /// Connected via agent-browser auto-discovery to a live Chrome instance.
    AutoConnect,
    /// Cookie-extracted Playwright storageState file.
    CookieState { state_file: PathBuf },
    /// Persistent browser profile directory managed by yoetz.
    Profile { profile_dir: PathBuf },
}

impl BrowserConnection {
    /// Whether this connection attaches to the user's live Chrome session.
    pub fn is_live_attach(&self) -> bool {
        matches!(self, Self::Cdp { .. } | Self::AutoConnect)
    }
}

/// Cached agent-browser resolution. Probed once per process, reused for all calls.
static AGENT_BROWSER: OnceLock<Result<(String, Vec<String>), String>> = OnceLock::new();

/// Returns (program, extra_prefix_args) for launching agent-browser.
/// Checks YOETZ_AGENT_BROWSER_BIN env, then PATH, then falls back to npx
/// (only if `YOETZ_ALLOW_NPX_FALLBACK=1` is set).
/// Result is cached for the lifetime of the process.
fn resolve_agent_browser() -> Result<(String, Vec<String>)> {
    let cached = AGENT_BROWSER.get_or_init(|| {
        if let Ok(bin) = env::var("YOETZ_AGENT_BROWSER_BIN") {
            return Ok((bin, vec![]));
        }
        // Check if agent-browser is in PATH
        if Command::new("agent-browser")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            return Ok(("agent-browser".to_string(), vec![]));
        }
        // Fall back to npx only if explicitly allowed — downloading and executing
        // arbitrary npm code at runtime is a security risk.
        if env::var("YOETZ_ALLOW_NPX_FALLBACK").as_deref() == Ok("1") {
            eprintln!("warning: agent-browser not found in PATH; falling back to `npx --yes agent-browser`. Set YOETZ_AGENT_BROWSER_BIN or install agent-browser globally to avoid this.");
            return Ok((
                "npx".to_string(),
                vec!["--yes".to_string(), "agent-browser".to_string()],
            ));
        }
        Err("agent-browser not found in PATH and npx fallback is disabled.\n\
             Install it globally:  npm install -g agent-browser\n\
             Or allow npx fallback: export YOETZ_ALLOW_NPX_FALLBACK=1"
            .to_string())
    });
    match cached {
        Ok(v) => Ok(v.clone()),
        Err(msg) => Err(anyhow!("{msg}")),
    }
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

fn legacy_connection(
    profile_dir: Option<&Path>,
    profile_mode: BrowserProfileMode,
    use_stealth: bool,
) -> Option<BrowserConnection> {
    let profile_dir = profile_dir?;
    let state_path = state_file(profile_dir);
    if matches!(profile_mode, BrowserProfileMode::PreferState) && use_stealth && state_path.exists()
    {
        return Some(BrowserConnection::CookieState {
            state_file: state_path,
        });
    }
    Some(BrowserConnection::Profile {
        profile_dir: profile_dir.to_path_buf(),
    })
}

fn build_agent_browser_args(
    mut args: Vec<String>,
    format: OutputFormat,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Vec<String> {
    let live_attach = connection.is_some_and(BrowserConnection::is_live_attach);

    if headed && !live_attach && !args.iter().any(|a| a == "--headed") {
        args.insert(0, "--headed".to_string());
    }

    match connection {
        Some(BrowserConnection::Cdp { endpoint }) => {
            if !args
                .iter()
                .any(|a| a == "--session" || a.starts_with("--session="))
            {
                args.insert(0, CDP_SESSION_NAME.to_string());
                args.insert(0, "--session".to_string());
            }
            if !args.iter().any(|a| a == "--cdp" || a.starts_with("--cdp=")) {
                args.insert(0, endpoint.clone());
                args.insert(0, "--cdp".to_string());
            }
        }
        Some(BrowserConnection::AutoConnect) => {
            // For auto-connect, do NOT add --session. A managed session creates
            // an isolated context that can't see the real Chrome tabs/DOM.
            // Auto-connect should attach to the real browser directly.
            if !args.iter().any(|a| a == "--auto-connect") {
                args.insert(0, "--auto-connect".to_string());
            }
        }
        Some(BrowserConnection::CookieState { state_file }) => {
            if !args
                .iter()
                .any(|a| a == "--state" || a.starts_with("--state="))
            {
                args.insert(0, state_file.to_string_lossy().to_string());
                args.insert(0, "--state".to_string());
            }
        }
        Some(BrowserConnection::Profile { profile_dir }) => {
            if !args
                .iter()
                .any(|a| a == "--profile" || a.starts_with("--profile="))
            {
                args.insert(0, profile_dir.to_string_lossy().to_string());
                args.insert(0, "--profile".to_string());
            }
        }
        None => {}
    }

    if use_stealth && !live_attach {
        if !args
            .iter()
            .any(|a| a == "--user-agent" || a.starts_with("--user-agent="))
        {
            args.insert(0, STEALTH_USER_AGENT.to_string());
            args.insert(0, "--user-agent".to_string());
        }
        if !args
            .iter()
            .any(|a| a == "--args" || a.starts_with("--args="))
        {
            args.insert(0, STEALTH_ARGS.to_string());
            args.insert(0, "--args".to_string());
        }
    }

    let wants_json = matches!(format, OutputFormat::Json | OutputFormat::Jsonl);
    if wants_json && !args.iter().any(|a| a == "--json") {
        args.push("--json".to_string());
    }

    args
}

fn run_agent_browser_with_connection(
    args: Vec<String>,
    format: OutputFormat,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    run_agent_browser_with_connection_timeout(args, format, connection, use_stealth, headed, None)
}

fn run_agent_browser_with_connection_timeout(
    args: Vec<String>,
    format: OutputFormat,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
    timeout_ms: Option<u64>,
) -> Result<String> {
    let (bin, prefix_args) = resolve_agent_browser()?;
    let mut cmd = Command::new(&bin);
    let final_args = build_agent_browser_args(args, format, connection, use_stealth, headed);
    let mut all_args = prefix_args;
    all_args.extend(final_args);

    if let Some(timeout) = timeout_ms {
        cmd.env("AGENT_BROWSER_DEFAULT_TIMEOUT", timeout.to_string());
    }

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

/// Run agent-browser in CDP mode with the given connection.
/// Stealth and headed are disabled — we're using the user's real Chrome.
#[allow(dead_code)] // Will be used when RecipeContext migrates to BrowserConnection
pub fn run_agent_browser_cdp(
    args: Vec<String>,
    format: OutputFormat,
    connection: &BrowserConnection,
) -> Result<String> {
    run_agent_browser_with_connection(
        args,
        format,
        Some(connection),
        /* use_stealth */ false,
        /* headed */ false,
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
    let connection = legacy_connection(profile_dir, profile_mode, use_stealth);
    run_agent_browser_with_connection(args, format, connection.as_ref(), use_stealth, headed)
}

pub fn run_recipe(recipe: Recipe, ctx: RecipeContext, format: OutputFormat) -> Result<()> {
    let connection = legacy_connection(
        ctx.profile_dir.as_deref(),
        ctx.profile_mode,
        ctx.use_stealth,
    );
    run_recipe_with_connection(recipe, ctx, connection.as_ref(), format)
}

/// Run a recipe using a live browser connection (CDP or auto_connect).
pub fn run_recipe_with_live_connection(
    recipe: Recipe,
    ctx: RecipeContext,
    connection: &BrowserConnection,
    format: OutputFormat,
) -> Result<()> {
    run_recipe_with_connection(recipe, ctx, Some(connection), format)
}

fn run_recipe_with_connection(
    recipe: Recipe,
    ctx: RecipeContext,
    connection: Option<&BrowserConnection>,
    format: OutputFormat,
) -> Result<()> {
    // For live-attach (auto-connect / CDP), skip the pre-recipe close.
    // The close creates a managed session that opens a blank tab in Chrome,
    // and then the recipe's `open` (rewritten to `tab new`) opens a second tab.
    // For non-live connections (cookie-state, profile), close the managed daemon
    // so the recipe starts with a fresh browser.
    if let Some(connection) = connection {
        if !connection.is_live_attach() {
            let _ = close_browser_for_connection(connection);
        }
    } else {
        let _ = close_browser();
    }

    let wants_json = matches!(format, OutputFormat::Json);
    let wants_jsonl = matches!(format, OutputFormat::Jsonl);
    let mut events: Vec<Value> = Vec::new();
    let mut headed = ctx.headed;

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

        if action == CHATGPT_WAIT_ACTION {
            let stdout = run_chatgpt_wait_response(
                step.args.as_deref(),
                step.timeout_ms,
                connection,
                ctx.use_stealth,
                headed,
            )
            .with_context(|| format!("recipe step {idx} ({action}) failed"))?;

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
            continue;
        }

        let commands = expand_step(action, step.args.as_deref(), &ctx)?;

        for args in commands {
            let stdout = match run_agent_browser_with_connection_timeout(
                args.clone(),
                format,
                connection,
                ctx.use_stealth,
                headed,
                step.timeout_ms,
            ) {
                Ok(stdout) => stdout,
                Err(err) => {
                    // Fetch current page state to check for challenge
                    let snapshot = run_agent_browser_with_connection(
                        vec![
                            "snapshot".to_string(),
                            "-c".to_string(),
                            "--json".to_string(),
                        ],
                        OutputFormat::Json,
                        connection,
                        ctx.use_stealth,
                        headed,
                    )
                    .ok();
                    if maybe_pause_for_captcha_challenge(
                        connection,
                        CHATGPT_URL,
                        headed,
                        snapshot.as_deref(),
                    )? {
                        headed = true;
                        run_agent_browser_with_connection_timeout(
                            args.clone(),
                            format,
                            connection,
                            ctx.use_stealth,
                            headed,
                            step.timeout_ms,
                        )?
                    } else {
                        return Err(err.context(format!("recipe step {idx} ({action}) failed")));
                    }
                }
            };

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatgptSendState {
    Enabled,
    Disabled,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChatgptDomState {
    send_state: ChatgptSendState,
    has_stop_button: bool,
    copy_button_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChatgptPollOptions {
    attempts: usize,
    interval_ms: u64,
}

fn run_chatgpt_wait_response(
    args: Option<&[String]>,
    timeout_ms: Option<u64>,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let options = parse_chatgpt_poll_args(args)?;
    let command_timeout_ms = timeout_ms.unwrap_or(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT);
    let baseline_snapshot =
        take_chatgpt_snapshot(connection, use_stealth, headed, command_timeout_ms)?;
    let baseline = normalize_snapshot_for_compare(&baseline_snapshot);
    let mut last_dom = ChatgptDomState {
        send_state: ChatgptSendState::Missing,
        has_stop_button: false,
        copy_button_count: 0,
    };

    for attempt in 1..=options.attempts {
        thread::sleep(Duration::from_millis(options.interval_ms));

        let snapshot = take_chatgpt_snapshot(connection, use_stealth, headed, command_timeout_ms)?;
        if maybe_pause_for_captcha_challenge(connection, CHATGPT_URL, headed, Some(&snapshot))? {
            continue;
        }
        if let Some(issue) = detect_chatgpt_response_issue(&snapshot) {
            return Err(anyhow!("{issue}"));
        }

        let dom = inspect_chatgpt_dom_state(connection, use_stealth, headed, command_timeout_ms)?;
        let changed = normalize_snapshot_for_compare(&snapshot) != baseline;
        last_dom = dom;

        if chatgpt_response_complete(changed, dom) {
            let payload = json!({
                "status": "ok",
                "attempt": attempt,
                "attempts": options.attempts,
                "interval_ms": options.interval_ms,
                "send_state": match dom.send_state {
                    ChatgptSendState::Enabled => "enabled",
                    ChatgptSendState::Disabled => "disabled",
                    ChatgptSendState::Missing => "missing",
                },
                "has_stop_button": dom.has_stop_button,
                "copy_button_count": dom.copy_button_count,
            });
            return Ok(payload.to_string());
        }
    }

    Err(anyhow!(
        "timed out waiting for ChatGPT response after {} polls (~{}s). last_state: send={:?}, stop={}, copy_buttons={}",
        options.attempts,
        (options.attempts as u64 * options.interval_ms) / 1000,
        last_dom.send_state,
        last_dom.has_stop_button,
        last_dom.copy_button_count
    ))
}

fn take_chatgpt_snapshot(
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
) -> Result<String> {
    run_agent_browser_with_connection_timeout(
        vec![
            "snapshot".to_string(),
            "-c".to_string(),
            "--json".to_string(),
        ],
        OutputFormat::Json,
        connection,
        use_stealth,
        headed,
        Some(timeout_ms),
    )
}

fn inspect_chatgpt_dom_state(
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
) -> Result<ChatgptDomState> {
    let script = r#"(() => {
  const send = document.querySelector("button[data-testid='send-button']");
  const stop = document.querySelector("button[data-testid='stop-button'], button[aria-label*='Stop']");
  const copyButtons = document.querySelectorAll("button[aria-label*='Copy'], button[data-testid*='copy']").length;
  const sendState = !send ? "missing" : send.disabled ? "disabled" : "enabled";
  return `send=${sendState}|stop=${stop ? 1 : 0}|copy=${copyButtons}`;
})()"#;
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["eval".to_string(), script.to_string()],
        OutputFormat::Text,
        connection,
        use_stealth,
        headed,
        Some(timeout_ms),
    )?;
    // agent-browser eval sometimes wraps the result in double quotes
    let raw = stdout.trim();
    let raw = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    parse_chatgpt_dom_state(raw)
}

fn parse_chatgpt_poll_args(args: Option<&[String]>) -> Result<ChatgptPollOptions> {
    let mut options = ChatgptPollOptions {
        attempts: CHATGPT_POLL_ATTEMPTS_DEFAULT,
        interval_ms: CHATGPT_POLL_INTERVAL_MS_DEFAULT,
    };
    let mut iter = args.unwrap_or_default().iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--attempts" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| anyhow!("--attempts requires a value"))?;
                options.attempts = raw
                    .parse()
                    .with_context(|| format!("invalid --attempts value `{raw}`"))?;
            }
            "--interval-ms" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| anyhow!("--interval-ms requires a value"))?;
                options.interval_ms = raw
                    .parse()
                    .with_context(|| format!("invalid --interval-ms value `{raw}`"))?;
            }
            other => return Err(anyhow!("unsupported {CHATGPT_WAIT_ACTION} arg `{other}`")),
        }
    }
    if options.attempts == 0 {
        return Err(anyhow!("--attempts must be greater than 0"));
    }
    if options.interval_ms == 0 {
        return Err(anyhow!("--interval-ms must be greater than 0"));
    }
    Ok(options)
}

fn parse_chatgpt_dom_state(raw: &str) -> Result<ChatgptDomState> {
    let mut send_state = None;
    let mut has_stop_button = None;
    let mut copy_button_count = None;

    for part in raw.split('|') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key {
            "send" => {
                send_state = Some(match value {
                    "enabled" => ChatgptSendState::Enabled,
                    "disabled" => ChatgptSendState::Disabled,
                    "missing" => ChatgptSendState::Missing,
                    other => return Err(anyhow!("invalid send state `{other}`")),
                });
            }
            "stop" => {
                has_stop_button = Some(match value {
                    "0" => false,
                    "1" => true,
                    other => return Err(anyhow!("invalid stop flag `{other}`")),
                });
            }
            "copy" => {
                copy_button_count = Some(
                    value
                        .parse()
                        .with_context(|| format!("invalid copy count `{value}`"))?,
                );
            }
            _ => {}
        }
    }

    Ok(ChatgptDomState {
        send_state: send_state.ok_or_else(|| anyhow!("missing send state"))?,
        has_stop_button: has_stop_button.ok_or_else(|| anyhow!("missing stop flag"))?,
        copy_button_count: copy_button_count.ok_or_else(|| anyhow!("missing copy count"))?,
    })
}

fn normalize_snapshot_for_compare(snapshot: &str) -> String {
    snapshot.chars().filter(|c| !c.is_whitespace()).collect()
}

fn detect_chatgpt_response_issue(snapshot: &str) -> Option<&'static str> {
    let haystack = snapshot.to_lowercase();
    let error_markers = [
        "network error",
        "something went wrong",
        "error generating",
        "attachment failed",
        "upload failed",
    ];
    error_markers
        .iter()
        .find(|needle| haystack.contains(**needle))
        .copied()
}

fn chatgpt_response_complete(snapshot_changed: bool, dom: ChatgptDomState) -> bool {
    dom.send_state == ChatgptSendState::Enabled
        && !dom.has_stop_button
        && (dom.copy_button_count > 0 || snapshot_changed)
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(profile_dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 700 {}", profile_dir.display()))?;
    }
    clear_state_file(profile_dir)?;
    // Close any existing daemon to ensure fresh options
    let _ = close_browser();
    let args = vec!["open".to_string(), CHATGPT_URL.to_string()];
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
    close_browser_daemon()
}

pub fn close_browser_for_connection(connection: &BrowserConnection) -> Result<()> {
    if connection.is_live_attach() {
        let (bin, prefix_args) = resolve_agent_browser()?;
        let mut cmd = Command::new(bin);
        cmd.args(prefix_args);
        match cmd.args(["--session", CDP_SESSION_NAME, "close"]).output() {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("warning: session close failed: {stderr}");
            }
            Err(e) => eprintln!("warning: session close error: {e}"),
            _ => {}
        }
        return Ok(());
    }
    close_browser_daemon()
}

fn close_browser_daemon() -> Result<()> {
    let (bin, prefix_args) = resolve_agent_browser()?;
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(profile_dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 700 {}", profile_dir.display()))?;
    }
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
    let parsed: Value = serde_json::from_str(stdout.trim()).with_context(|| {
        format!(
            "cookie extractor returned invalid JSON (first 200 chars): {}",
            &stdout[..stdout.len().min(200)]
        )
    })?;
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

    // Restrict state file permissions — it contains session cookies.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if state_file.exists() {
            fs::set_permissions(&state_file, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 600 {}", state_file.display()))?;
        }
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

/// Resolve CDP endpoint from flag → env → config (first non-empty wins).
pub fn resolve_cdp_endpoint(cdp_override: Option<&str>, config: &Config) -> Option<String> {
    cdp_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| env::var("YOETZ_BROWSER_CDP").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| config.defaults.browser_cdp.clone())
        .filter(|value| !value.is_empty())
}

fn resolve_browser_connection_fallback(
    profile_dir: &Path,
    headed: bool,
    target_url: &str,
) -> Result<BrowserConnection> {
    if !profile_dir.exists() {
        return Err(anyhow!(
            "browser profile not found at {}. Run `yoetz browser login` to authenticate.",
            profile_dir.display()
        ));
    }

    let cookie_state = BrowserConnection::CookieState {
        state_file: state_file(profile_dir),
    };
    if matches!(
        &cookie_state,
        BrowserConnection::CookieState { state_file } if state_file.exists()
    ) && check_auth_with_connection(&cookie_state, headed, target_url).is_ok()
    {
        return Ok(cookie_state);
    }

    let profile = BrowserConnection::Profile {
        profile_dir: profile_dir.to_path_buf(),
    };
    check_auth_with_connection(&profile, headed, target_url)?;
    Ok(profile)
}

pub fn resolve_browser_connection(
    config: &Config,
    cdp_override: Option<&str>,
    profile_dir: &Path,
    target_url: &str,
) -> Result<BrowserConnection> {
    if let Some(endpoint) = resolve_cdp_endpoint(cdp_override, config) {
        if try_cdp_attach(&endpoint, target_url).is_ok() {
            return Ok(BrowserConnection::Cdp { endpoint });
        }
    }

    if try_auto_connect(target_url).is_ok() {
        return Ok(BrowserConnection::AutoConnect);
    }

    resolve_browser_connection_fallback(profile_dir, /* headed */ false, target_url)
}

pub fn try_cdp_attach(endpoint: &str, target_url: &str) -> Result<()> {
    let connection = BrowserConnection::Cdp {
        endpoint: endpoint.to_string(),
    };
    verify_auth_cdp(target_url, &connection).map_err(|e| {
        if is_localhost_endpoint(endpoint) {
            e.context(chrome136_cdp_warning(endpoint))
        } else {
            e
        }
    })
}

/// Returns true if the endpoint targets localhost (affected by Chrome 136+ changes).
/// Extracts the host portion to avoid false positives on remote hostnames.
fn is_localhost_endpoint(endpoint: &str) -> bool {
    // Strip scheme (http://, ws://, etc.) to get authority.
    let authority = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    // Strip path, query, fragment.
    let host_port = authority.split('/').next().unwrap_or(authority);
    // Strip port.
    let host = if host_port.starts_with('[') {
        // IPv6: [::1]:9222 → [::1]
        host_port
            .split_once(']')
            .map(|(h, _)| h.trim_start_matches('['))
            .unwrap_or(host_port)
    } else {
        host_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port)
    };
    matches!(
        host.to_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// Warning message explaining the Chrome 136+ breaking change for local CDP.
fn chrome136_cdp_warning(endpoint: &str) -> String {
    format!(
        "Chrome 136+ ignores --remote-debugging-port on the default profile.\n\
         \n\
         If '{endpoint}' is unreachable, try one of these:\n\
         \n\
         1. Enable chrome://inspect/#remote-debugging in Chrome (recommended, Chrome 144+)\n\
            Then use: yoetz browser attach   (auto-discovers the debug port)\n\
         \n\
         2. Launch Chrome with a non-default profile:\n\
            chrome --remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug\n\
         \n\
         3. Use Chrome for Testing (exempt from this restriction):\n\
            https://developer.chrome.com/blog/chrome-for-testing"
    )
}

pub fn try_auto_connect(target_url: &str) -> Result<()> {
    let connection = BrowserConnection::AutoConnect;
    verify_auth_cdp(target_url, &connection)
}

/// Lightweight auto-connect check: verifies Chrome is reachable via
/// auto-connect without opening new tabs. Used by the recipe path where
/// the recipe itself handles navigation and auth detection.
pub fn try_auto_connect_lite() -> Result<()> {
    let connection = BrowserConnection::AutoConnect;
    run_agent_browser_with_connection(
        vec!["snapshot".to_string(), "-c".to_string()],
        OutputFormat::Text,
        Some(&connection),
        /* use_stealth */ false,
        /* headed */ false,
    )?;
    Ok(())
}

pub fn verify_auth_cdp(target_url: &str, connection: &BrowserConnection) -> Result<()> {
    if !connection.is_live_attach() {
        return Err(anyhow!(
            "verify_auth_cdp requires a live browser connection"
        ));
    }
    check_auth_with_connection(connection, /* headed */ false, target_url)
}

pub fn resolve_auth(profile_dir: &Path, headed: bool) -> Result<BrowserConnection> {
    resolve_browser_connection_fallback(profile_dir, headed, CHATGPT_URL)
}

pub fn resolve_auth_mode(profile_dir: &Path, headed: bool) -> Result<BrowserProfileMode> {
    match resolve_auth(profile_dir, headed)? {
        BrowserConnection::CookieState { .. } => Ok(BrowserProfileMode::PreferState),
        BrowserConnection::Profile { .. } => Ok(BrowserProfileMode::ProfileOnly),
        BrowserConnection::Cdp { .. } | BrowserConnection::AutoConnect => Err(anyhow!(
            "legacy auth mode cannot map a live browser connection"
        )),
    }
}

pub fn check_auth(profile_dir: &Path, headed: bool) -> Result<()> {
    let connection = resolve_auth(profile_dir, headed)?;
    check_auth_with_connection(&connection, headed, CHATGPT_URL)
}

fn check_auth_with_connection(
    connection: &BrowserConnection,
    headed: bool,
    target_url: &str,
) -> Result<()> {
    let _ = close_browser_for_connection(connection);
    let mut current_headed = headed;
    let use_stealth = !connection.is_live_attach();
    let open_args = if connection.is_live_attach() {
        vec!["tab".to_string(), "new".to_string(), target_url.to_string()]
    } else {
        vec!["open".to_string(), target_url.to_string()]
    };
    let _ = run_agent_browser_with_connection(
        open_args,
        OutputFormat::Text,
        Some(connection),
        use_stealth,
        current_headed,
    )?;
    let deadline = Instant::now() + Duration::from_millis(auth_check_timeout_ms(connection));
    let mut last_issue: Option<&'static str>;
    loop {
        let snapshot = run_agent_browser_with_connection(
            vec![
                "snapshot".to_string(),
                "-c".to_string(),
                "--json".to_string(),
            ],
            OutputFormat::Json,
            Some(connection),
            use_stealth,
            current_headed,
        )?;

        if is_challenge_page(&snapshot)
            && maybe_pause_for_captcha_challenge(
                Some(connection),
                target_url,
                current_headed,
                Some(&snapshot),
            )?
        {
            if !connection.is_live_attach() {
                current_headed = true;
            }
            continue;
        }

        last_issue = detect_auth_issue_for_connection(&snapshot, Some(connection));

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

fn auth_check_timeout_ms(connection: &BrowserConnection) -> u64 {
    if connection.is_live_attach() {
        LIVE_ATTACH_AUTH_CHECK_TIMEOUT_MS
    } else {
        AUTH_CHECK_TIMEOUT_MS
    }
}

/// Positive confirmation that the page is authenticated (ChatGPT loaded successfully).
fn looks_authenticated(snapshot: &str) -> bool {
    let haystack = snapshot.to_lowercase();
    let positive_markers = ["chatgpt", "new chat", "send a message", "message chatgpt"];
    contains_any(&haystack, &positive_markers)
}

fn maybe_pause_for_captcha_challenge(
    connection: Option<&BrowserConnection>,
    target_url: &str,
    headed: bool,
    snapshot: Option<&str>,
) -> Result<bool> {
    let Some(connection) = connection else {
        return Ok(false);
    };

    let snapshot = snapshot.unwrap_or_default();
    if !is_challenge_page(snapshot) {
        return Ok(false);
    }

    if connection.is_live_attach() {
        if !io::stdin().is_terminal() {
            return Err(anyhow!(
                "captcha detected in the attached Chrome session, but stdin is not interactive. Re-run this command in a terminal so you can solve the challenge."
            ));
        }
        eprintln!(
            "Captcha detected — please solve it in your Chrome window, then press Enter to continue"
        );
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        return Ok(true);
    }

    if headed {
        return Ok(false);
    }

    if !io::stdin().is_terminal() {
        return Err(anyhow!(
            "captcha detected, but stdin is not interactive. Re-run this command in a terminal so you can solve the challenge."
        ));
    }

    let _ = close_browser_for_connection(connection);
    run_agent_browser_with_connection(
        vec!["open".to_string(), target_url.to_string()],
        OutputFormat::Text,
        Some(connection),
        /* use_stealth */ true,
        /* headed */ true,
    )?;
    eprintln!(
        "Captcha detected — please solve it in the browser window, then press Enter to continue"
    );
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(true)
}

fn is_challenge_page(snapshot: &str) -> bool {
    let haystack = snapshot.to_lowercase();
    let challenge_markers = [
        "cloudflare",
        "checking your browser",
        "attention required",
        "security check",
        "just a moment",
        "verify you are human",
        "cf-chl",
    ];
    contains_any(&haystack, &challenge_markers)
}

fn detect_auth_issue_for_connection(
    snapshot: &str,
    connection: Option<&BrowserConnection>,
) -> Option<&'static str> {
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

    if is_challenge_page(snapshot) {
        if connection.is_some_and(BrowserConnection::is_live_attach) {
            return Some(
                "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again.",
            );
        }
        return Some(
            "cloudflare challenge detected. Run `yoetz browser sync-cookies` or `yoetz browser login` and try again.",
        );
    }
    if contains_any(&haystack, &login_markers) {
        if connection.is_some_and(BrowserConnection::is_live_attach) {
            return Some(
                "chatgpt login required in the attached Chrome session. Log in there and try again.",
            );
        }
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
        // Interpolate the full template (replaces {{bundle_text}}, {{prompt}}, etc.)
        // then chunk the result. This ensures recipe vars like {{prompt}} aren't lost.
        let full_text = interpolate(&args[1], ctx, Some(text))?;
        let full_chunks = chunk_text(&full_text, CHUNK_BYTES);
        if full_chunks.is_empty() {
            return Ok(Vec::new());
        }
        let mut commands = Vec::new();
        commands.push(vec![
            action.to_string(),
            selector.clone(),
            full_chunks[0].clone(),
        ]);
        for chunk in full_chunks.iter().skip(1) {
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
    if (value.contains("{{bundle_path}}") || value.contains("{{bundle_path|json}}"))
        && ctx.bundle_path.is_none()
    {
        return Err(anyhow!("bundle path requested but no bundle provided"));
    }
    if (value.contains("{{bundle_text}}") || value.contains("{{bundle_text|json}}"))
        && bundle_text.is_none()
    {
        return Err(anyhow!("bundle text requested but no bundle provided"));
    }

    // Check for unresolved placeholders in the ORIGINAL template (before substitution)
    // so that {{...}} patterns inside bundle_text don't trigger false errors.
    let mut known_vars: std::collections::HashSet<&str> =
        ctx.vars.keys().map(|s| s.as_str()).collect();
    known_vars.insert("bundle_path");
    known_vars.insert("bundle_text");
    let mut scan = value;
    while let Some(start) = scan.find("{{") {
        let rest = &scan[start + 2..];
        if let Some(end) = rest.find("}}") {
            let placeholder = rest[..end].trim();
            // Strip |json filter before checking known vars
            let base_name = placeholder.strip_suffix("|json").unwrap_or(placeholder);
            if !base_name.is_empty() && !known_vars.contains(base_name) {
                return Err(anyhow!(
                    "recipe variable {{{{{base_name}}}}} not provided. Pass `--var {base_name}=...` or add it under `defaults:` in the recipe."
                ));
            }
            scan = &rest[end + 2..];
        } else {
            break;
        }
    }

    // Perform substitutions — process |json filtered variants first so they
    // aren't consumed by the plain replacement pass.
    let mut out = value.to_string();
    if let Some(path) = &ctx.bundle_path {
        if out.contains("{{bundle_path|json}}") {
            let json_value =
                serde_json::to_string(path.as_str()).unwrap_or_else(|_| format!("\"{}\"", path));
            out = out.replace("{{bundle_path|json}}", &json_value);
        }
        out = out.replace("{{bundle_path}}", path);
    }
    for (key, value) in &ctx.vars {
        let json_needle = format!("{{{{{key}|json}}}}");
        if out.contains(&json_needle) {
            let json_value =
                serde_json::to_string(value).unwrap_or_else(|_| format!("\"{}\"", value));
            out = out.replace(&json_needle, &json_value);
        }
        let needle = format!("{{{{{key}}}}}");
        out = out.replace(&needle, value);
    }
    if let Some(text) = bundle_text {
        if out.contains("{{bundle_text|json}}") {
            let json_value =
                serde_json::to_string(text).unwrap_or_else(|_| format!("\"{}\"", text));
            out = out.replace("{{bundle_text|json}}", &json_value);
        }
        out = out.replace("{{bundle_text}}", text);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock as TestOnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: TestOnceLock<Mutex<()>> = TestOnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

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

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("yoetz_{label}_{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn command_path(dir: &Path, name: &str) -> PathBuf {
        if cfg!(windows) {
            dir.join(format!("{name}.cmd"))
        } else {
            dir.join(name)
        }
    }

    fn write_executable_script(path: &Path, unix_contents: &str, windows_contents: &str) {
        let contents = if cfg!(windows) {
            windows_contents
        } else {
            unix_contents
        };
        fs::write(path, contents).unwrap();
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    fn fake_agent_browser_bin() -> PathBuf {
        static BIN: TestOnceLock<PathBuf> = TestOnceLock::new();
        BIN.get_or_init(|| {
            let dir = unique_test_dir("fake_agent_browser");
            let bin = command_path(&dir, "fake-agent-browser");
            write_executable_script(
                &bin,
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LOG_PATH\"\n",
                "@echo off\r\necho %*>> \"%LOG_PATH%\"\r\n",
            );
            bin
        })
        .clone()
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
    fn browser_connection_live_attach_detection() {
        assert!(BrowserConnection::Cdp {
            endpoint: "http://127.0.0.1:9222".to_string(),
        }
        .is_live_attach());
        assert!(BrowserConnection::AutoConnect.is_live_attach());
        assert!(!BrowserConnection::CookieState {
            state_file: PathBuf::from("/tmp/state.json"),
        }
        .is_live_attach());
        assert!(!BrowserConnection::Profile {
            profile_dir: PathBuf::from("/tmp/profile"),
        }
        .is_live_attach());
    }

    #[test]
    #[allow(unsafe_code)]
    fn resolve_cdp_endpoint_prefers_flag_then_env_then_config() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            env::set_var("YOETZ_BROWSER_CDP", "http://127.0.0.1:9000");
        }
        let mut config = Config::default();
        config.defaults.browser_cdp = Some("http://127.0.0.1:9222".to_string());

        let from_flag = resolve_cdp_endpoint(Some("http://127.0.0.1:9333"), &config);
        assert_eq!(from_flag.as_deref(), Some("http://127.0.0.1:9333"));

        let from_env = resolve_cdp_endpoint(None, &config);
        assert_eq!(from_env.as_deref(), Some("http://127.0.0.1:9000"));

        unsafe {
            env::remove_var("YOETZ_BROWSER_CDP");
        }
        let from_config = resolve_cdp_endpoint(None, &config);
        assert_eq!(from_config.as_deref(), Some("http://127.0.0.1:9222"));
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
    fn interpolate_bundle_text_with_template_syntax_no_false_positive() {
        let ctx = recipe_context();
        // bundle_text contains {{handlebars}} template syntax — must not trigger an error
        let bundle = "function render() { return `{{user_name}}`; }";
        let result = interpolate("Review this code:\n{{bundle_text}}", &ctx, Some(bundle)).unwrap();
        assert!(result.contains("{{user_name}}"));
        assert!(result.contains("Review this code:"));
    }

    #[test]
    fn expand_fill_step_includes_prompt_with_bundle_text() {
        let mut ctx = recipe_context();
        ctx.vars
            .insert("prompt".to_string(), "What does this do?".to_string());
        let args = vec![
            "#prompt-textarea".to_string(),
            "{{bundle_text}}\n\n{{prompt}}".to_string(),
        ];
        let commands =
            expand_bundle_text_step("fill", &args, ctx.bundle_text.as_deref().unwrap(), &ctx)
                .unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0][0], "fill");
        assert_eq!(commands[0][1], "#prompt-textarea");
        // Must contain both bundle text AND the prompt
        assert!(commands[0][2].contains("hello world")); // bundle_text
        assert!(commands[0][2].contains("What does this do?")); // {{prompt}}
    }

    #[test]
    fn interpolate_does_not_expand_recipe_vars_inside_bundle_text() {
        let ctx = recipe_context();
        let result = interpolate("{{bundle_text}}", &ctx, Some("literal {{model}}")).unwrap();
        assert_eq!(result, "literal {{model}}");
    }

    #[test]
    fn interpolate_json_filter_escapes_quotes_and_newlines() {
        let mut ctx = recipe_context();
        ctx.vars.insert(
            "prompt".to_string(),
            "line 1\nline \"2\"\nline \\3".to_string(),
        );
        let result = interpolate("const t = {{prompt|json}};", &ctx, None).unwrap();
        assert_eq!(result, r#"const t = "line 1\nline \"2\"\nline \\3";"#);
    }

    #[test]
    fn interpolate_json_filter_coexists_with_plain_var() {
        let mut ctx = recipe_context();
        ctx.vars.insert("prompt".to_string(), "hello".to_string());
        let result = interpolate("{{prompt|json}} {{prompt}}", &ctx, None).unwrap();
        assert_eq!(result, r#""hello" hello"#);
    }

    #[test]
    fn interpolate_json_filter_missing_var_reports_base_name() {
        let ctx = recipe_context();
        let err = interpolate("{{missing|json}}", &ctx, None).unwrap_err();
        assert!(err.to_string().contains("--var missing="));
    }

    #[test]
    fn interpolate_json_filter_works_for_bundle_path() {
        let ctx = recipe_context();
        let result = interpolate("path = {{bundle_path|json}}", &ctx, None).unwrap();
        assert_eq!(result, r#"path = "/tmp/bundle.md""#);
    }

    #[test]
    fn interpolate_json_filter_works_for_bundle_text() {
        let ctx = recipe_context();
        let result =
            interpolate("text = {{bundle_text|json}}", &ctx, Some("line 1\nline 2")).unwrap();
        assert_eq!(result, r#"text = "line 1\nline 2""#);
    }

    #[test]
    fn interpolate_json_filter_errors_on_missing_bundle_path() {
        let mut ctx = recipe_context();
        ctx.bundle_path = None;
        let err = interpolate("{{bundle_path|json}}", &ctx, None).unwrap_err();
        assert!(err.to_string().contains("bundle path requested"));
    }

    #[test]
    fn interpolate_json_filter_errors_on_missing_bundle_text() {
        let ctx = recipe_context();
        let err = interpolate("{{bundle_text|json}}", &ctx, None).unwrap_err();
        assert!(err.to_string().contains("bundle text requested"));
    }

    #[test]
    fn build_agent_browser_args_adds_cdp_session_flags() {
        let args = build_agent_browser_args(
            vec!["snapshot".to_string()],
            OutputFormat::Json,
            Some(&BrowserConnection::Cdp {
                endpoint: "http://127.0.0.1:9222".to_string(),
            }),
            /* use_stealth */ true,
            /* headed */ true,
        );
        assert!(args.iter().any(|arg| arg == "--session"));
        assert!(args.iter().any(|arg| arg == CDP_SESSION_NAME));
        assert!(args.iter().any(|arg| arg == "--cdp"));
        assert!(args.iter().any(|arg| arg == "http://127.0.0.1:9222"));
        assert!(!args.iter().any(|arg| arg == "--headed"));
        assert!(!args.iter().any(|arg| arg == "--user-agent"));
    }

    #[test]
    fn build_agent_browser_args_omits_auto_connect_session_flags() {
        let args = build_agent_browser_args(
            vec!["open".to_string(), CHATGPT_URL.to_string()],
            OutputFormat::Text,
            Some(&BrowserConnection::AutoConnect),
            /* use_stealth */ false,
            /* headed */ false,
        );
        assert!(args.iter().any(|arg| arg == "--auto-connect"));
        assert!(!args.iter().any(|arg| arg == "--session"));
        assert!(!args.iter().any(|arg| arg == CDP_SESSION_NAME));
    }

    #[test]
    fn looks_authenticated_detects_chatgpt() {
        assert!(looks_authenticated(r#"{"text": "ChatGPT - New chat"}"#));
        assert!(looks_authenticated(r#"{"text": "Send a message"}"#));
        assert!(!looks_authenticated(r#"{"text": "Loading..."}"#));
        assert!(!looks_authenticated(""));
    }

    #[test]
    fn auth_check_timeout_is_longer_for_live_attach() {
        assert_eq!(
            auth_check_timeout_ms(&BrowserConnection::AutoConnect),
            LIVE_ATTACH_AUTH_CHECK_TIMEOUT_MS
        );
        assert_eq!(
            auth_check_timeout_ms(&BrowserConnection::Cdp {
                endpoint: "http://127.0.0.1:9222".to_string(),
            }),
            LIVE_ATTACH_AUTH_CHECK_TIMEOUT_MS
        );
        assert_eq!(
            auth_check_timeout_ms(&BrowserConnection::Profile {
                profile_dir: PathBuf::from("/tmp/profile"),
            }),
            AUTH_CHECK_TIMEOUT_MS
        );
        assert_eq!(
            auth_check_timeout_ms(&BrowserConnection::CookieState {
                state_file: PathBuf::from("/tmp/state.json"),
            }),
            AUTH_CHECK_TIMEOUT_MS
        );
    }

    #[test]
    fn is_challenge_page_detects_cloudflare_markers() {
        assert!(is_challenge_page(r#"{"text": "Verify you are human"}"#));
        assert!(is_challenge_page(
            r#"{"text": "Cloudflare security check"}"#
        ));
        assert!(!is_challenge_page(r#"{"text": "ChatGPT - New chat"}"#));
    }

    #[test]
    fn detect_auth_issue_for_live_attach_uses_live_guidance() {
        let issue = detect_auth_issue_for_connection(
            r#"{"text":"Please log in"}"#,
            Some(&BrowserConnection::AutoConnect),
        );
        assert_eq!(
            issue,
            Some("chatgpt login required in the attached Chrome session. Log in there and try again.")
        );
    }

    #[test]
    fn detect_auth_issue_covers_challenge_and_non_live_paths() {
        let live_challenge = detect_auth_issue_for_connection(
            r#"{"text":"Verify you are human"}"#,
            Some(&BrowserConnection::Cdp {
                endpoint: "http://127.0.0.1:9222".to_string(),
            }),
        );
        assert_eq!(
            live_challenge,
            Some(
                "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again."
            )
        );

        let managed_challenge = detect_auth_issue_for_connection(
            r#"{"text":"Checking your browser"}"#,
            Some(&BrowserConnection::Profile {
                profile_dir: PathBuf::from("/tmp/profile"),
            }),
        );
        assert_eq!(
            managed_challenge,
            Some(
                "cloudflare challenge detected. Run `yoetz browser sync-cookies` or `yoetz browser login` and try again."
            )
        );

        let managed_login = detect_auth_issue_for_connection(
            r#"{"text":"Please sign in"}"#,
            Some(&BrowserConnection::CookieState {
                state_file: PathBuf::from("/tmp/state.json"),
            }),
        );
        assert_eq!(
            managed_login,
            Some("chatgpt login required. Run `yoetz browser login` and try again.")
        );

        let authenticated = detect_auth_issue_for_connection(
            r#"{"text":"ChatGPT - New chat"}"#,
            Some(&BrowserConnection::AutoConnect),
        );
        assert_eq!(authenticated, None);
    }

    #[test]
    #[allow(unsafe_code)]
    fn close_browser_for_connection_uses_expected_close_mode() {
        let _guard = env_lock().lock().unwrap();
        let log_dir = unique_test_dir("close_browser");
        let log_path = log_dir.join("agent-browser.log");
        let bin = fake_agent_browser_bin();
        unsafe {
            env::set_var("YOETZ_AGENT_BROWSER_BIN", &bin);
            env::set_var("LOG_PATH", &log_path);
        }

        let live_cases = [
            BrowserConnection::AutoConnect,
            BrowserConnection::Cdp {
                endpoint: "http://127.0.0.1:9222".to_string(),
            },
        ];
        for connection in live_cases {
            let _ = fs::remove_file(&log_path);
            close_browser_for_connection(&connection).unwrap();
            let logged = fs::read_to_string(&log_path).unwrap_or_default();
            assert!(
                logged.contains("--session yoetz-cdp close"),
                "expected live-attach session close, got `{logged}`"
            );
        }

        let managed_cases = [
            BrowserConnection::CookieState {
                state_file: PathBuf::from("/tmp/state.json"),
            },
            BrowserConnection::Profile {
                profile_dir: PathBuf::from("/tmp/profile"),
            },
        ];
        for connection in managed_cases {
            let _ = fs::remove_file(&log_path);
            close_browser_for_connection(&connection).unwrap();
            let logged = fs::read_to_string(&log_path).unwrap_or_default();
            assert!(
                logged.contains("close"),
                "expected managed daemon close, got `{logged}`"
            );
            assert!(
                !logged.contains("--session yoetz-cdp close"),
                "managed close should not use the live-attach session: `{logged}`"
            );
        }

        unsafe {
            env::remove_var("LOG_PATH");
        }
    }

    #[test]
    #[allow(unsafe_code)]
    fn sync_cookies_errors_on_invalid_json_output() {
        let _guard = env_lock().lock().unwrap();
        let dir = unique_test_dir("fake_node");
        let fake_node = command_path(&dir, "node");
        write_executable_script(
            &fake_node,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo \"v24.4.0\"\nelse\n  echo \"{not-json\"\nfi\n",
            "@echo off\r\nif \"%1\"==\"--version\" (\r\n  echo v24.4.0\r\n) else (\r\n  echo {not-json\r\n)\r\n",
        );

        let scripts_dir = dir.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        fs::write(
            scripts_dir.join("extract-cookies.mjs"),
            "console.log('unused');\n",
        )
        .unwrap();

        let original_path = env::var_os("PATH");
        let path_entries: Vec<PathBuf> = std::iter::once(dir.clone())
            .chain(
                original_path
                    .as_ref()
                    .into_iter()
                    .flat_map(|value| env::split_paths(value)),
            )
            .collect();
        let new_path = env::join_paths(path_entries).unwrap();

        unsafe {
            env::set_var("PATH", &new_path);
            env::set_var("YOETZ_SCRIPTS_DIR", &scripts_dir);
        }

        let profile_dir = dir.join("profile");
        let err = sync_cookies(&profile_dir).unwrap_err();
        assert!(err.to_string().contains("invalid JSON"));

        match original_path {
            Some(path) => unsafe { env::set_var("PATH", path) },
            None => unsafe { env::remove_var("PATH") },
        }
        unsafe {
            env::remove_var("YOETZ_SCRIPTS_DIR");
        }
    }

    #[test]
    fn parse_chatgpt_poll_args_defaults() {
        let options = parse_chatgpt_poll_args(None).unwrap();
        assert_eq!(
            options,
            ChatgptPollOptions {
                attempts: CHATGPT_POLL_ATTEMPTS_DEFAULT,
                interval_ms: CHATGPT_POLL_INTERVAL_MS_DEFAULT,
            }
        );
    }

    #[test]
    fn parse_chatgpt_poll_args_custom_values() {
        let args = vec![
            "--attempts".to_string(),
            "12".to_string(),
            "--interval-ms".to_string(),
            "9000".to_string(),
        ];
        let options = parse_chatgpt_poll_args(Some(&args)).unwrap();
        assert_eq!(
            options,
            ChatgptPollOptions {
                attempts: 12,
                interval_ms: 9000,
            }
        );
    }

    #[test]
    fn parse_chatgpt_poll_args_rejects_unknown_flag() {
        let args = vec!["--nope".to_string()];
        let err = parse_chatgpt_poll_args(Some(&args)).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[test]
    fn parse_chatgpt_dom_state_parses_eval_output() {
        let state = parse_chatgpt_dom_state("send=enabled|stop=0|copy=2").unwrap();
        assert_eq!(
            state,
            ChatgptDomState {
                send_state: ChatgptSendState::Enabled,
                has_stop_button: false,
                copy_button_count: 2,
            }
        );
    }

    #[test]
    fn parse_chatgpt_dom_state_rejects_quoted_output() {
        // agent-browser eval sometimes wraps output in double quotes;
        // the caller (inspect_chatgpt_dom_state) strips them before calling
        // parse_chatgpt_dom_state, but if unstripped quotes leak through the
        // parser should fail clearly rather than silently.
        let err = parse_chatgpt_dom_state(r#""send=enabled|stop=0|copy=1""#).unwrap_err();
        assert!(
            err.to_string().contains("send state")
                || err.to_string().contains("copy count")
                || err.to_string().contains("invalid"),
            "expected parse error for quoted output, got: {err}"
        );
    }

    #[test]
    fn detect_chatgpt_response_issue_finds_error_markers() {
        assert_eq!(
            detect_chatgpt_response_issue(r#"{"text":"Something went wrong"}"#),
            Some("something went wrong")
        );
        assert_eq!(
            detect_chatgpt_response_issue(r#"{"text":"all good"}"#),
            None
        );
    }

    #[test]
    fn chatgpt_response_complete_requires_enabled_send_and_progress() {
        let dom = ChatgptDomState {
            send_state: ChatgptSendState::Enabled,
            has_stop_button: false,
            copy_button_count: 0,
        };
        assert!(chatgpt_response_complete(true, dom));
        assert!(!chatgpt_response_complete(
            false,
            ChatgptDomState {
                copy_button_count: 0,
                ..dom
            }
        ));
        assert!(!chatgpt_response_complete(
            true,
            ChatgptDomState {
                send_state: ChatgptSendState::Disabled,
                ..dom
            }
        ));
    }

    #[test]
    fn recipe_yaml_rejects_unknown_keys() {
        let top_level_err = serde_yaml::from_str::<Recipe>(
            r#"
name: chatgpt
oops: true
steps:
  - action: open
    args: ["https://chatgpt.com/"]
"#,
        )
        .unwrap_err();
        assert!(top_level_err.to_string().contains("unknown field"));

        let step_err = serde_yaml::from_str::<Recipe>(
            r#"
name: chatgpt
steps:
  - action: open
    args: ["https://chatgpt.com/"]
    slep_ms: 1
"#,
        )
        .unwrap_err();
        assert!(step_err.to_string().contains("unknown field"));
    }

    #[test]
    fn recipe_step_parses_timeout_ms() {
        let recipe = serde_yaml::from_str::<Recipe>(
            r#"
name: test
steps:
  - action: wait
    args: ["--fn", "document.querySelector('#done')"]
    timeout_ms: 180000
"#,
        )
        .unwrap();
        assert_eq!(recipe.steps[0].timeout_ms, Some(180000));
    }

    #[test]
    fn recipe_step_with_sleep_and_action_prefers_sleep() {
        let recipe = serde_yaml::from_str::<Recipe>(
            r#"
name: noop
steps:
  - sleep_ms: 0
    action: open
    args: ["https://chatgpt.com/"]
"#,
        )
        .unwrap();
        let connection = BrowserConnection::AutoConnect;
        run_recipe_with_connection(
            recipe,
            recipe_context(),
            Some(&connection),
            OutputFormat::Text,
        )
        .unwrap();
    }

    #[test]
    fn is_localhost_endpoint_matches_common_local_addresses() {
        assert!(is_localhost_endpoint("http://127.0.0.1:9222"));
        assert!(is_localhost_endpoint("http://localhost:9222"));
        assert!(is_localhost_endpoint("http://LOCALHOST:9222"));
        assert!(is_localhost_endpoint("http://[::1]:9222"));
        assert!(!is_localhost_endpoint("http://192.168.1.5:9222"));
        assert!(!is_localhost_endpoint("ws://remote-host:9222"));
        // Must not false-positive on hostnames containing "localhost"
        assert!(!is_localhost_endpoint(
            "http://not-localhost.example.com:9222"
        ));
    }

    #[test]
    fn chrome136_cdp_warning_includes_endpoint_and_guidance() {
        let warning = chrome136_cdp_warning("http://127.0.0.1:9222");
        assert!(warning.contains("127.0.0.1:9222"));
        assert!(warning.contains("Chrome 136+"));
        assert!(warning.contains("chrome://inspect/#remote-debugging"));
        assert!(warning.contains("--user-data-dir"));
        assert!(warning.contains("Chrome for Testing"));
    }
}

use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::chatgpt_recipe::{self, AnyhowResultExt};
use crate::chatgpt_web;
use crate::chrome_devtools_mcp::client::{
    browser_id_from_ws_endpoint, discover_devtools_active_port_files,
    discover_local_chromium_processes, discover_running_chrome_targets, infer_email_hints,
    ChromiumProcessSummary, DevtoolsActivePortFile, RunningChromeTarget,
};
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
const CHATGPT_WAIT_UPLOAD_ACTION: &str = "chatgpt_wait_upload";
const CHATGPT_SELECT_MODEL_ACTION: &str = "chatgpt_select_model";
const CHATGPT_OPEN_ATTACHMENT_UI_ACTION: &str = "chatgpt_open_attachment_ui";
const CHATGPT_UPLOAD_BUNDLE_ACTION: &str = "chatgpt_upload_bundle";
const CHATGPT_SEND_ACTION: &str = "chatgpt_send";
const CHATGPT_POLL_ATTEMPTS_DEFAULT: usize = 60;
const CHATGPT_POLL_INTERVAL_MS_DEFAULT: u64 = 30_000;
const CHATGPT_POLL_TOTAL_TIMEOUT_MS_DEFAULT: u64 = 5_400_000;
const CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT: u64 = 10_000;
const CHATGPT_UPLOAD_POLL_ATTEMPTS_DEFAULT: usize = 60;
const CHATGPT_UPLOAD_POLL_INTERVAL_MS_DEFAULT: u64 = 2_000;
const CHATGPT_SEND_ENABLE_TIMEOUT_MS: u64 = 10_000;
const CHATGPT_SEND_ENABLE_POLL_INTERVAL_MS: u64 = 250;
const DAEMON_APPROVAL_GRACE_WINDOW: Duration = Duration::from_secs(30);
const LIVE_ATTACH_COMMAND_TIMEOUT_MS: u64 = 30_000;
const CDP_SESSION_NAME: &str = "yoetz-cdp";
// Keep the legacy filename for local compatibility; the lock only serializes
// attach attempts and does not represent persisted Chrome approval.
const CHROME_ATTACH_ATTEMPT_LOCK_FILENAME: &str = "chrome-approval.lock";
const BROWSER_TARGET_STATE_FILENAME: &str = "browser-target.json";
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum RecipeTransport {
    DevBrowser,
    AgentBrowser,
    ChromeDevtoolsMcp,
    ChromeExtensionNative,
    Manual,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: Option<String>,
    pub transports: Option<Vec<RecipeTransport>>,
    pub defaults: Option<BTreeMap<String, String>>,
    pub steps: Vec<RecipeStep>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeStep {
    pub action: Option<String>,
    pub args: Option<Vec<String>>,
    pub sleep_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
    /// JS expression evaluated in the page context. If it returns a truthy
    /// value, the step (action + sleep) is skipped. Useful for conditional
    /// steps like "only upload if the bundle is large".
    pub skip_if: Option<String>,
}

pub struct RecipeContext {
    pub bundle_path: Option<String>,
    pub bundle_text: Option<String>,
    pub profile_dir: Option<PathBuf>,
    pub profile_mode: BrowserProfileMode,
    pub fallback_used: bool,
    pub use_stealth: bool,
    pub headed: bool,
    pub vars: BTreeMap<String, String>,
    /// Target URL for captcha recovery (defaults to CHATGPT_URL).
    pub target_url: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserDefaults {
    pub profile: Option<String>,
    pub cdp: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct BrowserConfigFile {
    defaults: Option<BrowserConfigDefaults>,
}

#[derive(Debug, Deserialize, Default)]
struct BrowserConfigDefaults {
    browser_profile: Option<String>,
    browser_cdp: Option<String>,
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
    Cdp {
        endpoint: String,
        run_id: Option<String>,
    },
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedCdpTargetSource {
    Flag,
    Env,
    Config,
    Auto,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCdpTarget {
    pub endpoint: String,
    pub source: ResolvedCdpTargetSource,
    pub description: String,
    source_path: Option<PathBuf>,
    selected_target: Option<RunningChromeTarget>,
}

impl ResolvedCdpTarget {
    pub fn is_auto_discovered(&self) -> bool {
        matches!(self.source, ResolvedCdpTargetSource::Auto)
    }

    pub fn is_authoritative(&self) -> bool {
        matches!(self.source, ResolvedCdpTargetSource::Flag)
    }

    pub fn live_attach_target_alias(&self) -> String {
        if let Some(source_path) = &self.source_path {
            return format!("source-path:{}", source_path.display());
        }
        if let Some(browser_id) = browser_id_from_ws_endpoint(&self.endpoint) {
            return format!("browser-id:{browser_id}");
        }
        format!("endpoint:{}", self.endpoint)
    }

    pub fn selected_running_target(&self) -> Option<&RunningChromeTarget> {
        self.selected_target.as_ref()
    }

    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BrowserTargetState {
    last_source_path: Option<PathBuf>,
}

/// Cached agent-browser resolution. Probed once per process, reused for all calls.
static AGENT_BROWSER: OnceLock<Result<(String, Vec<String>), String>> = OnceLock::new();
static BROWSER_TARGET_STATE_WARNING_EMITTED: OnceLock<()> = OnceLock::new();
const AGENT_BROWSER_INSTALL_GUIDANCE: &str = concat!(
    "agent-browser not found in PATH. Install it explicitly using a pinned, vetted ",
    "binary/package, or set YOETZ_AGENT_BROWSER_BIN to the exact executable to run."
);

/// Returns true when the dev-browser backend is available locally.
pub fn use_dev_browser() -> bool {
    crate::dev_browser::has_any_backend()
}

pub fn recipe_transports(recipe: &Recipe, is_chatgpt: bool) -> Vec<RecipeTransport> {
    recipe.transports.clone().unwrap_or_else(|| {
        if is_chatgpt {
            // Chrome 147+ compat waterfall. chrome-devtools-mcp is primary
            // because it is the only tier that works against a running
            // logged-in Chrome 147 default profile (Playwright-based
            // dev-browser hangs on `Target.setAutoAttach`, agent-browser
            // inherits the same gating). dev-browser stays second-tier for
            // Chrome ≤ 146 and Chrome for Testing. agent-browser stays
            // third for cookie/profile managed flows. Manual is the final
            // escape hatch.
            vec![
                RecipeTransport::ChromeDevtoolsMcp,
                RecipeTransport::DevBrowser,
                RecipeTransport::AgentBrowser,
                RecipeTransport::Manual,
            ]
        } else {
            vec![RecipeTransport::AgentBrowser]
        }
    })
}

/// Prepend `chrome-extension-native` to the ChatGPT recipe transport list when
/// the Yoetz Chrome extension is installed and connected and the recipe author
/// did not pin their own transport order. The list is returned unchanged for
/// any other recipe, when the recipe pinned `transports:`, when the extension
/// is unhealthy, or when the list already contains `chrome-extension-native`.
pub fn maybe_prefer_extension_native_for_chatgpt(
    transports: Vec<RecipeTransport>,
    is_chatgpt: bool,
    recipe_transports_pinned: bool,
    extension_connected: bool,
) -> Vec<RecipeTransport> {
    if !is_chatgpt || recipe_transports_pinned || !extension_connected {
        return transports;
    }
    if transports.contains(&RecipeTransport::ChromeExtensionNative) {
        return transports;
    }
    let mut promoted = Vec::with_capacity(transports.len() + 1);
    promoted.push(RecipeTransport::ChromeExtensionNative);
    promoted.extend(transports);
    promoted
}

/// Returns (program, extra_prefix_args) for launching agent-browser.
/// Checks YOETZ_AGENT_BROWSER_BIN on every call, then falls back to a cached
/// PATH probe for the lifetime of the process.
fn resolve_agent_browser() -> Result<(String, Vec<String>)> {
    if let Ok(bin) = env::var("YOETZ_AGENT_BROWSER_BIN") {
        return Ok((bin, vec![]));
    }

    let cached = AGENT_BROWSER.get_or_init(detect_agent_browser_in_path);
    match cached {
        Ok(v) => Ok(v.clone()),
        Err(msg) => Err(anyhow!("{msg}")),
    }
}

fn detect_agent_browser_in_path() -> Result<(String, Vec<String>), String> {
    if Command::new("agent-browser")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        return Ok(("agent-browser".to_string(), vec![]));
    }
    Err(AGENT_BROWSER_INSTALL_GUIDANCE.to_string())
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

fn managed_connection(
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
        Some(BrowserConnection::Cdp { endpoint, run_id }) => {
            if !args
                .iter()
                .any(|a| a == "--session" || a.starts_with("--session="))
            {
                let session_name = run_id
                    .as_deref()
                    .map(live_cdp_session_name_for_run)
                    .unwrap_or_else(|| CDP_SESSION_NAME.to_string());
                args.insert(0, session_name);
                args.insert(0, "--session".to_string());
            }
            if !args.iter().any(|a| a == "--cdp" || a.starts_with("--cdp=")) {
                args.insert(0, endpoint.clone());
                args.insert(0, "--cdp".to_string());
            }
        }
        Some(BrowserConnection::AutoConnect) if !args.iter().any(|a| a == "--auto-connect") => {
            // For auto-connect, do NOT add --session. A managed session creates
            // an isolated context that can't see the real Chrome tabs/DOM.
            // Auto-connect should attach to the real browser directly.
            args.insert(0, "--auto-connect".to_string());
        }
        Some(BrowserConnection::CookieState { state_file })
            if !args
                .iter()
                .any(|a| a == "--state" || a.starts_with("--state=")) =>
        {
            args.insert(0, state_file.to_string_lossy().to_string());
            args.insert(0, "--state".to_string());
        }
        Some(BrowserConnection::Profile { profile_dir })
            if !args
                .iter()
                .any(|a| a == "--profile" || a.starts_with("--profile=")) =>
        {
            args.insert(0, profile_dir.to_string_lossy().to_string());
            args.insert(0, "--profile".to_string());
        }
        _ => {}
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
    let connection = managed_connection(profile_dir, profile_mode, use_stealth);
    run_agent_browser_with_connection(args, format, connection.as_ref(), use_stealth, headed)
}

pub fn run_recipe(recipe: Recipe, ctx: RecipeContext, format: OutputFormat) -> Result<Value> {
    let connection = managed_connection(
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
) -> Result<Value> {
    run_recipe_with_connection(recipe, ctx, Some(connection), format)
}

fn run_recipe_with_connection(
    recipe: Recipe,
    ctx: RecipeContext,
    connection: Option<&BrowserConnection>,
    format: OutputFormat,
) -> Result<Value> {
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
    let is_chatgpt_recipe = recipe
        .name
        .as_deref()
        .is_some_and(|name| name.eq_ignore_ascii_case("chatgpt"));
    let collect_step_events = wants_json || (wants_jsonl && is_chatgpt_recipe);
    let mut events: Vec<Value> = Vec::new();
    let mut headed = ctx.headed;
    let mut pending_chatgpt_send_baseline: Option<ChatgptResponseBaseline> = None;
    let mut chatgpt_stage = ChatgptRecipeStage::Idle;
    let mut chatgpt_focus_cache = ChatgptRunTabFocusCache::default();

    if let Some(connection) = connection {
        if connection.is_live_attach() {
            maybe_select_live_attach_profile_tab(connection, &ctx, headed)?;
        }
    }

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
        // Evaluate skip_if before anything else (including sleep).
        if let Some(expr) = &step.skip_if {
            if let Some(action) = step.action.as_deref() {
                focus_chatgpt_run_tab_before_recipe_action(
                    is_chatgpt_recipe,
                    action,
                    connection,
                    &ctx,
                    headed,
                    step.timeout_ms
                        .unwrap_or(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT),
                    Some(&mut chatgpt_focus_cache),
                )
                .map_err(|err| {
                    mark_chatgpt_error_after_side_effect(err, chatgpt_stage.terminal_phase(action))
                })
                .with_context(|| {
                    format!("recipe step {idx} ({action}) target focus before skip_if failed")
                })?;
            }
            let result = run_agent_browser_with_connection(
                vec!["eval".to_string(), expr.clone()],
                OutputFormat::Text,
                connection,
                ctx.use_stealth,
                headed,
            )
            .map_err(|err| {
                let phase = step
                    .action
                    .as_deref()
                    .and_then(|action| chatgpt_stage.terminal_phase(action));
                mark_chatgpt_error_after_side_effect(err, phase)
            })?;
            let trimmed = result.trim().trim_matches('"');
            if !trimmed.is_empty()
                && trimmed != "false"
                && trimmed != "0"
                && trimmed != "null"
                && trimmed != "undefined"
            {
                continue;
            }
        }

        if let Some(ms) = step.sleep_ms {
            thread::sleep(Duration::from_millis(ms));
        }

        let Some(action) = step.action.as_ref() else {
            if step.sleep_ms.is_some() {
                continue;
            }
            return Err(anyhow!("recipe step {idx} missing action"));
        };

        focus_chatgpt_run_tab_before_recipe_action(
            is_chatgpt_recipe,
            action,
            connection,
            &ctx,
            headed,
            step.timeout_ms
                .unwrap_or(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT),
            Some(&mut chatgpt_focus_cache),
        )
        .map_err(|err| {
            mark_chatgpt_error_after_side_effect(err, chatgpt_stage.terminal_phase(action))
        })
        .with_context(|| format!("recipe step {idx} ({action}) target focus failed"))?;

        if action == CHATGPT_WAIT_ACTION {
            // Interpolate recipe vars (e.g. {{wait_timeout_ms}}) in args before parsing.
            let interpolated_args: Option<Vec<String>> = step
                .args
                .as_ref()
                .map(|args| {
                    args.iter()
                        .map(|a| interpolate(a, &ctx, None))
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()
                .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::WaitResponse)
                .with_context(|| format!("recipe step {idx} ({action}) var interpolation"))?;
            let stdout = run_chatgpt_wait_response(
                interpolated_args.as_deref(),
                step.timeout_ms,
                connection,
                ctx.vars.get("run_id").map(String::as_str),
                pending_chatgpt_send_baseline.take(),
                ctx.use_stealth,
                headed,
            )
            .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::WaitResponse)
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
                }
                if collect_step_events {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
            continue;
        }

        if action == CHATGPT_SELECT_MODEL_ACTION {
            let stdout = run_chatgpt_select_model(&ctx, connection, ctx.use_stealth, headed)
                .map_err(|err| {
                    mark_chatgpt_error_after_side_effect(err, chatgpt_stage.terminal_phase(action))
                })
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
                }
                if collect_step_events {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
            continue;
        }

        if action == CHATGPT_OPEN_ATTACHMENT_UI_ACTION {
            let stdout = run_chatgpt_open_attachment_ui(connection, ctx.use_stealth, headed)
                .map_err(|err| {
                    mark_chatgpt_error_after_side_effect(err, chatgpt_stage.terminal_phase(action))
                })
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
                }
                if collect_step_events {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
            continue;
        }

        if action == CHATGPT_UPLOAD_BUNDLE_ACTION {
            chatgpt_stage.mark_upload_started();
            let stdout = run_chatgpt_upload_bundle(&ctx, connection, ctx.use_stealth, headed)
                .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Upload)
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
                }
                if collect_step_events {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
            continue;
        }

        if action == CHATGPT_SEND_ACTION {
            let stdout = run_chatgpt_send(connection, ctx.use_stealth, headed)
                .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Send)
                .with_context(|| format!("recipe step {idx} ({action}) failed"))?;
            pending_chatgpt_send_baseline = parse_chatgpt_send_baseline_from_stdout(&stdout);
            chatgpt_stage.mark_send_succeeded();

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
                }
                if collect_step_events {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
            continue;
        }

        if action == CHATGPT_WAIT_UPLOAD_ACTION {
            chatgpt_stage.mark_upload_started();
            let stdout = run_chatgpt_wait_upload(
                &ctx,
                step.args.as_deref(),
                connection,
                ctx.use_stealth,
                headed,
            )
            .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Upload)
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
                }
                if collect_step_events {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
            continue;
        }

        if is_chatgpt_recipe && action == "upload" {
            chatgpt_stage.mark_upload_started();
        }
        let action_terminal_phase = if is_chatgpt_recipe {
            chatgpt_stage.terminal_phase(action)
        } else {
            None
        };
        let commands = expand_step(action, step.args.as_deref(), &ctx)
            .map_err(|err| mark_chatgpt_error_after_side_effect(err, action_terminal_phase))
            .with_context(|| format!("recipe step {idx} ({action}) expand failed"))?;

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
                        &ctx.target_url,
                        headed,
                        snapshot.as_deref(),
                    )
                    .map_err(|err| {
                        mark_chatgpt_error_after_side_effect(err, action_terminal_phase)
                    })? {
                        headed = true;
                        run_agent_browser_with_connection_timeout(
                            args.clone(),
                            format,
                            connection,
                            ctx.use_stealth,
                            headed,
                            step.timeout_ms,
                        )
                        .map_err(|err| {
                            mark_chatgpt_error_after_side_effect(err, action_terminal_phase)
                        })?
                    } else {
                        return Err(mark_chatgpt_error_after_side_effect(
                            err.context(format!("recipe step {idx} ({action}) failed")),
                            action_terminal_phase,
                        ));
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
                }
                if collect_step_events {
                    events.push(event);
                }
            } else {
                print!("{stdout}");
            }
        }
    }

    let payload = if is_chatgpt_recipe {
        chatgpt_recipe_payload_from_steps(&events, ctx.fallback_used)
    } else {
        json!({
            "name": recipe.name,
            "steps": events,
        })
    };

    if wants_json {
        write_json(&payload)?;
        return Ok(payload);
    }

    if wants_jsonl && is_chatgpt_recipe {
        let event = json!({
            "type": "recipe_complete",
            "transport": "agent-browser",
            "backend": "agent-browser",
            "response": payload.get("response").cloned().unwrap_or(Value::Null),
            "model_used": payload.get("model_used").cloned().unwrap_or(Value::Null),
            "model_selection_status": payload
                .get("model_selection_status")
                .cloned()
                .unwrap_or(Value::Null),
            "warnings": payload
                .get("warnings")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
            "fallback_used": ctx.fallback_used,
            "delivery_mode": payload.get("delivery_mode").cloned().unwrap_or(Value::Null),
            "auto_paste_fallback": payload
                .get("auto_paste_fallback")
                .cloned()
                .unwrap_or(Value::Bool(false)),
        });
        write_jsonl_event(&event)?;
    }

    Ok(payload)
}

fn chatgpt_recipe_payload_from_steps(steps: &[Value], fallback_used: bool) -> Value {
    let mut response = Value::Null;
    let mut model_used = Value::Null;
    let mut model_selection_status = chatgpt_recipe::ChatgptModelSelectionStatus::Unavailable;
    let mut warnings: Vec<String> = Vec::new();

    for step in steps {
        let action = step
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let stdout = step.get("stdout").unwrap_or(&Value::Null);
        if action == CHATGPT_SELECT_MODEL_ACTION {
            if let Some(value) = stdout.get("model_used").cloned() {
                model_used = value;
            }
            if let Some(value) = stdout.get("model_selection_status").cloned() {
                if let Ok(status) = serde_json::from_value(value) {
                    model_selection_status = status;
                }
            }
        }
        if action == CHATGPT_WAIT_ACTION {
            if let Some(value) = stdout.get("response").cloned() {
                response = value;
            }
            if let Some(items) = stdout.get("warnings").and_then(Value::as_array) {
                warnings.extend(
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .map(str::to_owned),
                );
            }
        }
    }

    let output = chatgpt_recipe::ChatgptRecipeOutput {
        transport: "agent-browser".to_string(),
        backend: "agent-browser".to_string(),
        response: response.as_str().unwrap_or_default().to_string(),
        model_used: model_used.as_str().map(str::to_owned),
        model_selection_status,
        warnings,
        fallback_used,
        delivery_mode: chatgpt_recipe::ChatgptDeliveryMode::FileUpload,
        auto_paste_fallback: false,
    };
    let mut payload = output.to_value();
    if let Some(object) = payload.as_object_mut() {
        object.insert("steps".to_string(), json!(steps));
    }
    payload
}

fn focus_chatgpt_run_tab_before_recipe_action(
    is_chatgpt_recipe: bool,
    action: &str,
    connection: Option<&BrowserConnection>,
    ctx: &RecipeContext,
    headed: bool,
    timeout_ms: u64,
    focus_cache: Option<&mut ChatgptRunTabFocusCache>,
) -> Result<()> {
    if !is_chatgpt_recipe || action == "open" {
        return Ok(());
    }
    let Some(connection) = connection.filter(|connection| connection.is_live_attach()) else {
        return Ok(());
    };

    focus_chatgpt_run_tab_for_live_attach_cached(
        Some(connection),
        ctx.vars.get("run_id").map(String::as_str),
        ctx.use_stealth,
        headed,
        timeout_ms,
        focus_cache,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatgptRecipeStage {
    Idle,
    UploadStarted,
    SendSucceeded,
}

impl ChatgptRecipeStage {
    fn mark_upload_started(&mut self) {
        if *self == Self::Idle {
            *self = Self::UploadStarted;
        }
    }

    fn mark_send_succeeded(&mut self) {
        *self = Self::SendSucceeded;
    }

    fn terminal_phase(self, action: &str) -> Option<chatgpt_recipe::ChatgptTransportPhase> {
        if self == Self::SendSucceeded {
            return Some(chatgpt_recipe::ChatgptTransportPhase::WaitResponse);
        }

        match action {
            CHATGPT_WAIT_ACTION => Some(chatgpt_recipe::ChatgptTransportPhase::WaitResponse),
            CHATGPT_SEND_ACTION => Some(chatgpt_recipe::ChatgptTransportPhase::Send),
            CHATGPT_WAIT_UPLOAD_ACTION | CHATGPT_UPLOAD_BUNDLE_ACTION | "upload" => {
                Some(chatgpt_recipe::ChatgptTransportPhase::Upload)
            }
            _ if self == Self::UploadStarted => Some(chatgpt_recipe::ChatgptTransportPhase::Upload),
            _ => None,
        }
    }
}

fn mark_chatgpt_error_after_side_effect(
    err: anyhow::Error,
    phase: Option<chatgpt_recipe::ChatgptTransportPhase>,
) -> anyhow::Error {
    match phase {
        Some(phase) => chatgpt_recipe::mark_terminal_fallback_phase(err, phase),
        None => err,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatgptSendState {
    Enabled,
    Disabled,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChatgptDomState {
    send_state: ChatgptSendState,
    has_stop_button: bool,
    has_thinking_indicator: bool,
    /// Copy buttons scoped to the latest assistant message only.
    copy_button_count: usize,
    /// Number of assistant messages on the page.
    assistant_msg_count: usize,
    /// Character length of the latest assistant message (for stability detection).
    assistant_last_len: usize,
    /// Error text from scoped toast/alert containers (empty = no error).
    error: String,
}

/// Per-poll classification of ChatGPT response state.
///
/// The poll loop interprets these verdicts:
/// - `CopyButton`: copy control rendered on a *new* assistant message — the
///   strongest "streaming finished" signal ChatGPT emits. Complete immediately.
/// - `Idle`: composer is idle and at least some progress beyond baseline, but
///   no copy button yet. Caller must verify the message length stays unchanged
///   for a stable-idle window before declaring completion (fallback path).
/// - `Generating`: still streaming, thinking, or no progress yet. Reset any
///   in-flight idle window and keep polling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletionVerdict {
    CopyButton,
    Idle,
    Generating,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct AgentBrowserTab {
    index: usize,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
}

#[derive(Debug, Default, Deserialize)]
struct AgentBrowserTabListData {
    #[serde(default)]
    tabs: Vec<AgentBrowserTab>,
}

#[derive(Debug, Deserialize)]
struct AgentBrowserTabListEnvelope {
    #[serde(default)]
    data: AgentBrowserTabListData,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AutoConnectDoctorStatus {
    Reachable(Vec<AgentBrowserTab>),
    Unavailable(String),
    Skipped(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BrowserHelperProcessKind {
    DevBrowserDaemon,
    YoetzLiveCdpDaemon,
    ChromeDevtoolsMcp,
    ChromeDevtoolsMcpWatchdog,
}

impl BrowserHelperProcessKind {
    fn label(&self) -> &'static str {
        match self {
            Self::DevBrowserDaemon => "dev-browser daemon",
            Self::YoetzLiveCdpDaemon => "yoetz live-CDP daemon",
            Self::ChromeDevtoolsMcp => "chrome-devtools-mcp",
            Self::ChromeDevtoolsMcpWatchdog => "chrome-devtools-mcp watchdog",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BrowserHelperProcessSummary {
    pid: u32,
    kind: BrowserHelperProcessKind,
    command: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BrowserDoctorHelpers {
    agent_browser_default: DaemonState,
    agent_browser_default_pid: Option<u32>,
    live_attach_daemon: crate::live_attach::DaemonSummary,
    yoetz_live_cdp_processes: Vec<BrowserHelperProcessSummary>,
    dev_browser_processes: Vec<BrowserHelperProcessSummary>,
    external_mcp_processes: Vec<BrowserHelperProcessSummary>,
    recommended_actions: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChatgptPollOptions {
    attempts: usize,
    interval_ms: u64,
    timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChatgptResponseBaseline {
    assistant_msg_count: usize,
    assistant_last_len: usize,
}

fn run_chatgpt_wait_response(
    args: Option<&[String]>,
    timeout_ms: Option<u64>,
    connection: Option<&BrowserConnection>,
    run_id: Option<&str>,
    baseline: Option<ChatgptResponseBaseline>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let options = parse_chatgpt_poll_args(CHATGPT_WAIT_ACTION, args)?;
    let command_timeout_ms = timeout_ms.unwrap_or(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT);
    let started_at = Instant::now();
    let deadline = started_at + Duration::from_millis(options.timeout_ms);
    let mut focus_cache = ChatgptRunTabFocusCache::default();
    // Prefer the exact pre-send assistant counters returned by
    // `chatgpt_send`; otherwise fall back to a fresh DOM baseline.
    focus_chatgpt_run_tab_for_live_attach_cached(
        connection,
        run_id,
        use_stealth,
        headed,
        command_timeout_ms,
        Some(&mut focus_cache),
    )?;
    let baseline_dom = match baseline {
        Some(baseline) => baseline_dom_state(baseline),
        None => inspect_chatgpt_dom_state(connection, use_stealth, headed, command_timeout_ms)?,
    };
    let stable_idle_threshold_ms = chatgpt_stable_idle_threshold_ms(options.interval_ms);
    let mut last_dom = ChatgptDomState {
        send_state: ChatgptSendState::Missing,
        has_stop_button: false,
        has_thinking_indicator: false,
        copy_button_count: 0,
        assistant_msg_count: baseline_dom.assistant_msg_count,
        assistant_last_len: 0,
        error: String::new(),
    };
    // Stable-idle accounting: once we see an `Idle` verdict, anchor the
    // (msg_count, last_len) and start a real-time clock. The fallback only
    // fires if subsequent polls keep the same anchor for the threshold window.
    // Any growth or `Generating` verdict resets the anchor.
    let mut idle_since: Option<Instant> = None;
    let mut idle_anchor: Option<(usize, usize)> = None;
    let mut completed_polls = 0usize;
    let mut deadline_reached = false;

    for attempt in 1..=options.attempts {
        let now = Instant::now();
        if now >= deadline {
            deadline_reached = true;
            break;
        }
        let remaining_ms = deadline
            .saturating_duration_since(now)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(delay) = chatgpt_wait_probe_delay(attempt, options.interval_ms, remaining_ms) {
            thread::sleep(delay);
        }

        completed_polls = attempt;

        focus_chatgpt_run_tab_for_live_attach_cached(
            connection,
            run_id,
            use_stealth,
            headed,
            command_timeout_ms,
            Some(&mut focus_cache),
        )?;
        let dom = inspect_chatgpt_dom_state(connection, use_stealth, headed, command_timeout_ms)?;
        if !dom.error.is_empty() {
            return Err(anyhow!("ChatGPT error: {}", dom.error));
        }
        last_dom = dom.clone();

        match classify_chatgpt_completion(&dom, &baseline_dom) {
            CompletionVerdict::CopyButton => {
                let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                focus_chatgpt_run_tab_for_live_attach_cached(
                    connection,
                    run_id,
                    use_stealth,
                    headed,
                    command_timeout_ms,
                    Some(&mut focus_cache),
                )?;
                let response = read_latest_chatgpt_response(
                    connection,
                    use_stealth,
                    headed,
                    command_timeout_ms,
                )?;
                let payload = json!({
                    "status": "ok",
                    "response": response,
                    "warnings": Vec::<String>::new(),
                    "completion_reason": "copy_button",
                    "attempt": attempt,
                    "attempts": options.attempts,
                    "interval_ms": options.interval_ms,
                    "timeout_ms": options.timeout_ms,
                    "elapsed_ms": elapsed_ms,
                    "stable_for_ms": 0,
                    "stable_idle_threshold_ms": stable_idle_threshold_ms,
                    "send_state": chatgpt_send_state_str(dom.send_state),
                    "has_stop_button": dom.has_stop_button,
                    "has_thinking_indicator": dom.has_thinking_indicator,
                    "copy_button_count": dom.copy_button_count,
                    "assistant_msg_count": dom.assistant_msg_count,
                    "assistant_last_len": dom.assistant_last_len,
                });
                return Ok(payload.to_string());
            }
            CompletionVerdict::Idle => {
                let anchor = (dom.assistant_msg_count, dom.assistant_last_len);
                let stable_for_ms = match (idle_since, idle_anchor) {
                    (Some(since), Some(prev_anchor)) if prev_anchor == anchor => Instant::now()
                        .duration_since(since)
                        .as_millis()
                        .min(u128::from(u64::MAX))
                        as u64,
                    _ => {
                        idle_since = Some(Instant::now());
                        idle_anchor = Some(anchor);
                        0
                    }
                };
                if stable_for_ms >= stable_idle_threshold_ms {
                    let elapsed_ms =
                        started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                    focus_chatgpt_run_tab_for_live_attach_cached(
                        connection,
                        run_id,
                        use_stealth,
                        headed,
                        command_timeout_ms,
                        Some(&mut focus_cache),
                    )?;
                    let response = read_latest_chatgpt_response(
                        connection,
                        use_stealth,
                        headed,
                        command_timeout_ms,
                    )?;
                    let payload = json!({
                        "status": "ok",
                        "response": response,
                        "warnings": Vec::<String>::new(),
                        "completion_reason": "stable_idle_fallback",
                        "attempt": attempt,
                        "attempts": options.attempts,
                        "interval_ms": options.interval_ms,
                        "timeout_ms": options.timeout_ms,
                        "elapsed_ms": elapsed_ms,
                        "stable_for_ms": stable_for_ms,
                        "stable_idle_threshold_ms": stable_idle_threshold_ms,
                        "send_state": chatgpt_send_state_str(dom.send_state),
                        "has_stop_button": dom.has_stop_button,
                        "has_thinking_indicator": dom.has_thinking_indicator,
                        "copy_button_count": dom.copy_button_count,
                        "assistant_msg_count": dom.assistant_msg_count,
                        "assistant_last_len": dom.assistant_last_len,
                    });
                    return Ok(payload.to_string());
                }
            }
            CompletionVerdict::Generating => {
                idle_since = None;
                idle_anchor = None;
            }
        }
    }

    if Instant::now() >= deadline {
        deadline_reached = true;
    }
    let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let limit_reason = if deadline_reached {
        format!("deadline={}ms", options.timeout_ms)
    } else {
        format!("attempt_limit={}", options.attempts)
    };

    Err(anyhow!(
        "timed out waiting for ChatGPT response after {} polls (~{}s elapsed, {}). last_state: send={:?}, stop={}, thinking={}, copy={}, msgs={}, lastlen={}",
        completed_polls,
        elapsed_ms / 1000,
        limit_reason,
        last_dom.send_state,
        last_dom.has_stop_button,
        last_dom.has_thinking_indicator,
        last_dom.copy_button_count,
        last_dom.assistant_msg_count,
        last_dom.assistant_last_len
    ))
}

fn chatgpt_wait_probe_delay(
    attempt: usize,
    interval_ms: u64,
    remaining_ms: u64,
) -> Option<Duration> {
    if attempt == 1 {
        None
    } else {
        Some(Duration::from_millis(interval_ms.min(remaining_ms)))
    }
}

#[derive(Debug, Default)]
struct ChatgptRunTabFocusCache {
    run_id: Option<String>,
    tab_index: Option<usize>,
}

fn focus_chatgpt_run_tab_for_live_attach_cached(
    connection: Option<&BrowserConnection>,
    run_id: Option<&str>,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
    mut cache: Option<&mut ChatgptRunTabFocusCache>,
) -> Result<()> {
    let Some(connection) = connection.filter(|connection| connection.is_live_attach()) else {
        return Ok(());
    };
    let Some(run_id) = run_id.map(str::trim).filter(|run_id| !run_id.is_empty()) else {
        return Ok(());
    };
    let encoded_marker_param = chatgpt_web::chatgpt_run_url_marker(run_id);
    let raw_marker_param = format!("_yoetz={run_id}");
    let url_has_run_marker =
        |url: &str| url.contains(&encoded_marker_param) || url.contains(&raw_marker_param);
    let run_marker = format!("yoetz:{run_id}");

    if let Some(cache_ref) = cache.as_deref_mut() {
        if cache_ref.run_id.as_deref() == Some(run_id) {
            if let Some(index) = cache_ref.tab_index {
                if select_live_attach_tab(connection, index, use_stealth, headed, timeout_ms)
                    .is_ok()
                {
                    let identity = inspect_current_live_attach_tab_identity(
                        connection,
                        use_stealth,
                        headed,
                        timeout_ms,
                    )?;
                    if identity.window_name == run_marker || url_has_run_marker(&identity.url) {
                        return Ok(());
                    }
                }
                cache_ref.tab_index = None;
            }
        } else {
            cache_ref.run_id = Some(run_id.to_string());
            cache_ref.tab_index = None;
        }
    }

    let tabs = list_live_attach_tabs(connection, use_stealth, headed, Some(timeout_ms))
        .with_context(|| format!("list Chrome tabs while tracking yoetz run `{run_id}`"))?;
    if let Some(tab) = tabs
        .iter()
        .find(|tab| tab.active && url_has_run_marker(&tab.url))
    {
        if let Some(cache_ref) = cache.as_deref_mut() {
            cache_ref.run_id = Some(run_id.to_string());
            cache_ref.tab_index = Some(tab.index);
        }
        return Ok(());
    }
    if let Some(tab) = tabs.iter().find(|tab| url_has_run_marker(&tab.url)) {
        select_live_attach_tab(connection, tab.index, use_stealth, headed, timeout_ms)?;
        if let Some(cache_ref) = cache.as_deref_mut() {
            cache_ref.run_id = Some(run_id.to_string());
            cache_ref.tab_index = Some(tab.index);
        }
        return Ok(());
    }

    for tab in chatgpt_run_tab_candidates(&tabs) {
        select_live_attach_tab(connection, tab.index, use_stealth, headed, timeout_ms)?;
        let identity =
            inspect_current_live_attach_tab_identity(connection, use_stealth, headed, timeout_ms)?;
        if identity.window_name == run_marker || url_has_run_marker(&identity.url) {
            if let Some(cache_ref) = cache.as_deref_mut() {
                cache_ref.run_id = Some(run_id.to_string());
                cache_ref.tab_index = Some(tab.index);
            }
            return Ok(());
        }
    }

    Err(mark_chatgpt_attached_page_error(anyhow!(
        "yoetz-owned ChatGPT tab `{run_id}` was not visible in the attached Chrome session"
    )))
}

fn select_live_attach_tab(
    connection: &BrowserConnection,
    index: usize,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
) -> Result<()> {
    run_agent_browser_with_connection_timeout(
        vec!["tab".to_string(), index.to_string()],
        OutputFormat::Text,
        Some(connection),
        use_stealth,
        headed,
        Some(timeout_ms),
    )?;
    Ok(())
}

fn chatgpt_run_tab_candidates(tabs: &[AgentBrowserTab]) -> Vec<&AgentBrowserTab> {
    let mut candidates = tabs
        .iter()
        .filter(|tab| is_chatgpt_tab(tab))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|tab| (!tab.active, tab.index));
    candidates
}

fn is_chatgpt_tab(tab: &AgentBrowserTab) -> bool {
    let url = tab.url.to_ascii_lowercase();
    let title = tab.title.to_ascii_lowercase();
    url.contains("chatgpt.com") || title.contains("chatgpt")
}

#[derive(Debug, Deserialize)]
struct LiveAttachTabIdentity {
    #[serde(rename = "windowName", default)]
    window_name: String,
    #[serde(default)]
    url: String,
}

fn inspect_current_live_attach_tab_identity(
    connection: &BrowserConnection,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
) -> Result<LiveAttachTabIdentity> {
    let expression = chatgpt_web::wrap_function_source_for_json_eval(
        r#"
() => ({
  windowName: String(window.name || ""),
  url: String(location.href || ""),
})
"#,
    )?;
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["eval".to_string(), expression],
        OutputFormat::Text,
        Some(connection),
        use_stealth,
        headed,
        Some(timeout_ms),
    )?;
    let payload = parse_stdout_json(&stdout)
        .with_context(|| format!("parse ChatGPT run-tab identity result: {stdout}"))?;
    serde_json::from_value(payload)
        .context("agent-browser run-tab identity payload did not match the expected shape")
}

/// Short-burst polling for ChatGPT file upload completion.
/// Checks whether the upload spinner (`.animate-spin` SVG whose parent has
/// `display: none` when done) has disappeared inside the file tile.
fn parse_upload_poll_options(args: Option<&[String]>) -> Result<ChatgptPollOptions> {
    match args {
        Some(a) if !a.is_empty() => parse_chatgpt_poll_args(CHATGPT_WAIT_UPLOAD_ACTION, Some(a)),
        _ => Ok(ChatgptPollOptions {
            attempts: CHATGPT_UPLOAD_POLL_ATTEMPTS_DEFAULT,
            interval_ms: CHATGPT_UPLOAD_POLL_INTERVAL_MS_DEFAULT,
            timeout_ms: CHATGPT_UPLOAD_POLL_ATTEMPTS_DEFAULT as u64
                * CHATGPT_UPLOAD_POLL_INTERVAL_MS_DEFAULT,
        }),
    }
}

fn run_chatgpt_select_model(
    ctx: &RecipeContext,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let requested_model = ctx
        .vars
        .get("model")
        .map(String::as_str)
        .unwrap_or_default();
    let keep_current_model = chatgpt_web::should_keep_current_chatgpt_model(requested_model);
    let function = chatgpt_web::build_model_selection_function(requested_model);
    let expression = chatgpt_web::wrap_function_source_for_json_eval(&function)?;
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["eval".to_string(), expression],
        OutputFormat::Text,
        connection,
        use_stealth,
        headed,
        Some(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT),
    )?;
    let selection: Value = parse_stdout_json(&stdout)
        .with_context(|| format!("parse ChatGPT model selection result: {stdout}"))?;
    let status = selection
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let model_used = chatgpt_web::select_reported_chatgpt_model(&selection, requested_model);
    let model_selection_status =
        chatgpt_web::chatgpt_model_selection_status(&selection, requested_model);
    match status {
        "selected" | "already-selected" => {
            Ok(json!({
                "status": "ok",
                "model_used": model_used,
                "model_selection_status": model_selection_status,
            })
            .to_string())
        }
        "missing-selector" | "not-found" if keep_current_model => {
            Ok(json!({
                "status": "ok",
                "model_used": model_used,
                "model_selection_status": model_selection_status,
            })
            .to_string())
        }
        "missing-selector" => Err(anyhow!(
            "ChatGPT model selector button not found. url={:?}, title={:?}",
            selection.get("url").and_then(Value::as_str).unwrap_or(""),
            selection.get("title").and_then(Value::as_str).unwrap_or("")
        )),
        "not-found" => Err(anyhow!(
            "requested ChatGPT model `{}` was not available in the current session. Available options: {}",
            selection
                .get("requested")
                .and_then(Value::as_str)
                .unwrap_or(requested_model),
            selection
                .get("availableItems")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|items| !items.is_empty())
                .unwrap_or_else(|| "<unknown>".to_string())
        )),
        "selection-mismatch" => Err(anyhow!(
            "requested ChatGPT model `{}` was not actually selected. selected_label={:?}; target_testid={:?}; available_after={}",
            selection
                .get("requested")
                .and_then(Value::as_str)
                .unwrap_or(requested_model),
            selection.get("selectedLabel").and_then(Value::as_str),
            selection.get("targetTestId").and_then(Value::as_str),
            selection
                .get("availableItemsAfter")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        )),
        other => Err(anyhow!("unexpected ChatGPT model selection status `{other}`")),
    }
}

fn run_chatgpt_open_attachment_ui(
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let expression = chatgpt_web::wrap_function_source_for_json_eval(
        &chatgpt_web::build_open_attachment_ui_function(),
    )?;
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["eval".to_string(), expression],
        OutputFormat::Text,
        connection,
        use_stealth,
        headed,
        Some(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT),
    )?;
    let result: Value = parse_stdout_json(&stdout)
        .with_context(|| format!("parse ChatGPT attachment UI result: {stdout}"))?;
    match result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "opened" => Ok(json!({"status": "ok"}).to_string()),
        "not-found" => Err(anyhow!(
            "ChatGPT attachment button not found. url={:?}, title={:?}",
            result.get("url").and_then(Value::as_str).unwrap_or(""),
            result.get("title").and_then(Value::as_str).unwrap_or("")
        )),
        other => Err(anyhow!("unexpected ChatGPT attachment UI status `{other}`")),
    }
}

fn run_chatgpt_upload_bundle(
    ctx: &RecipeContext,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let bundle_path = ctx
        .bundle_path
        .as_deref()
        .context("ChatGPT upload requires `bundle_path` in the recipe context")?;
    let marker_selector = format!(
        "input[type='file'][title='{}']",
        chatgpt_web::COMPOSER_FILE_INPUT_MARKER
    );
    let scope_expression = build_chatgpt_upload_scope_with_nudges_expression()?;

    let scope_result = eval_chatgpt_upload_scope(
        &scope_expression,
        connection,
        use_stealth,
        headed,
        CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT,
    )?;
    if !chatgpt_upload_scope_is_marked(&scope_result) {
        return Err(anyhow!(
            "composer-scoped ChatGPT file input was not available for upload: {}",
            scope_result
        ));
    }

    run_agent_browser_with_connection_timeout(
        vec![
            "upload".to_string(),
            marker_selector.clone(),
            bundle_path.to_string(),
        ],
        OutputFormat::Text,
        connection,
        use_stealth,
        headed,
        Some(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT),
    )
    .with_context(|| format!("upload `{bundle_path}` through {marker_selector}"))?;

    Ok(json!({"status": "ok", "selector": marker_selector}).to_string())
}

fn build_chatgpt_upload_scope_with_nudges_expression() -> Result<String> {
    let scope_function_json =
        serde_json::to_string(&chatgpt_web::build_scope_composer_file_input_function())?;
    let open_function_json =
        serde_json::to_string(&chatgpt_web::build_open_attachment_ui_function())?;
    let menu_function_json =
        serde_json::to_string(&chatgpt_web::build_upload_menu_item_click_function())?;
    chatgpt_web::wrap_function_source_for_json_eval(&format!(
        r#"
async () => {{
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const run = (source) => eval("(" + source + ")")();
  let state = await run({scope_function_json});
  if (state?.status === "marked") return state;
  await run({open_function_json}).catch(() => null);
  await wait(300);
  state = await run({scope_function_json});
  if (state?.status === "marked") return state;
  await run({menu_function_json}).catch(() => null);
  await wait(300);
  return await run({scope_function_json});
}}
"#
    ))
}

fn eval_chatgpt_upload_scope(
    scope_expression: &str,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
) -> Result<Value> {
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["eval".to_string(), scope_expression.to_string()],
        OutputFormat::Text,
        connection,
        use_stealth,
        headed,
        Some(timeout_ms),
    )?;
    parse_stdout_json(&stdout)
        .with_context(|| format!("parse ChatGPT upload scope result: {stdout}"))
}

fn chatgpt_upload_scope_is_marked(result: &Value) -> bool {
    result.get("status").and_then(Value::as_str) == Some("marked")
}

fn run_chatgpt_wait_upload(
    ctx: &RecipeContext,
    args: Option<&[String]>,
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let options = parse_upload_poll_options(args)?;
    let started_at = Instant::now();
    let deadline = started_at + Duration::from_millis(options.timeout_ms);
    let bundle_path = ctx
        .bundle_path
        .as_deref()
        .context("ChatGPT upload waiter requires `bundle_path` in the recipe context")?;
    let file_name = Path::new(bundle_path)
        .file_name()
        .and_then(|value| value.to_str())
        .context("bundle path must end in a UTF-8 filename")?;
    let function = chatgpt_web::build_attachment_probe_function(file_name)?;
    let check_script = chatgpt_web::wrap_function_source_for_json_eval(&function)?;

    for attempt in 1..=options.attempts {
        if Instant::now() >= deadline {
            break;
        }
        let remaining_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        thread::sleep(Duration::from_millis(options.interval_ms.min(remaining_ms)));

        let stdout = run_agent_browser_with_connection_timeout(
            vec!["eval".to_string(), check_script.clone()],
            OutputFormat::Text,
            connection,
            use_stealth,
            headed,
            Some(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT),
        )?;
        let probe: Value = parse_stdout_json(&stdout)
            .with_context(|| format!("parse attachment upload probe: {stdout}"))?;
        let status = probe
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        match status {
            "done" => {
                let stable_ready_count = probe
                    .get("stableReadyCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if stable_ready_count >= chatgpt_web::CHATGPT_UPLOAD_STABLE_POLLS {
                    return Ok(json!({"status": "ok", "polls": attempt}).to_string());
                }
            }
            "failed" => {
                return Err(anyhow!(
                    "attachment upload for `{file_name}` failed: {}",
                    probe
                ));
            }
            "no_tile" if attempt > 5 => {
                return Err(anyhow!(
                    "attachment chip for `{file_name}` never appeared after {attempt} polls"
                ));
            }
            "no_match" if attempt > 5 => {
                return Err(anyhow!(
                    "attachment chip for `{file_name}` was never detected in the composer after {attempt} polls"
                ));
            }
            _ => {}
        }
    }

    let elapsed_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Err(anyhow!(
        "upload for `{file_name}` still processing after ~{}s (deadline={}ms)",
        elapsed_ms / 1000,
        options.timeout_ms,
    ))
}

fn run_chatgpt_send(
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
) -> Result<String> {
    let expression = chatgpt_web::wrap_function_source_for_json_eval(
        &chatgpt_web::build_send_button_click_function(),
    )?;
    let started_at = Instant::now();

    loop {
        let stdout = run_agent_browser_with_connection_timeout(
            vec!["eval".to_string(), expression.clone()],
            OutputFormat::Text,
            connection,
            use_stealth,
            headed,
            Some(CHATGPT_POLL_COMMAND_TIMEOUT_MS_DEFAULT),
        )?;
        let result: Value = parse_stdout_json(&stdout)
            .with_context(|| format!("parse ChatGPT send result: {stdout}"))?;
        match result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
        {
            "sent" => {
                let baseline = parse_chatgpt_send_baseline(&result)
                    .ok_or_else(|| anyhow!("missing assistant baseline in ChatGPT send payload"))?;
                return Ok(json!({
                    "status": "ok",
                    "assistantCountBeforeSend": baseline.assistant_msg_count,
                    "assistantLastLenBeforeSend": baseline.assistant_last_len,
                })
                .to_string());
            }
            "not-ready" => {
                let diagnostics = result.get("diagnostics").cloned().unwrap_or(Value::Null);
                if started_at.elapsed() >= Duration::from_millis(CHATGPT_SEND_ENABLE_TIMEOUT_MS) {
                    return Err(anyhow!(
                        "ChatGPT send button never became enabled after typing. {}",
                        diagnostics
                    ));
                }
                thread::sleep(Duration::from_millis(CHATGPT_SEND_ENABLE_POLL_INTERVAL_MS));
            }
            other => return Err(anyhow!("unexpected ChatGPT send status `{other}`")),
        }
    }
}

fn parse_chatgpt_send_baseline(result: &Value) -> Option<ChatgptResponseBaseline> {
    let assistant_msg_count = result
        .get("assistantCountBeforeSend")
        .and_then(Value::as_u64)?
        .try_into()
        .ok()?;
    let assistant_last_len = result
        .get("assistantLastLenBeforeSend")
        .and_then(Value::as_u64)?
        .try_into()
        .ok()?;
    Some(ChatgptResponseBaseline {
        assistant_msg_count,
        assistant_last_len,
    })
}

fn parse_chatgpt_send_baseline_from_stdout(stdout: &str) -> Option<ChatgptResponseBaseline> {
    let payload = parse_stdout_json(stdout)?;
    parse_chatgpt_send_baseline(&payload)
}

fn baseline_dom_state(baseline: ChatgptResponseBaseline) -> ChatgptDomState {
    ChatgptDomState {
        send_state: ChatgptSendState::Missing,
        has_stop_button: false,
        has_thinking_indicator: false,
        copy_button_count: 0,
        assistant_msg_count: baseline.assistant_msg_count,
        assistant_last_len: baseline.assistant_last_len,
        error: String::new(),
    }
}

fn inspect_chatgpt_dom_state(
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
) -> Result<ChatgptDomState> {
    let script = chatgpt_web::wrap_function_source_for_json_eval(
        &chatgpt_web::build_chatgpt_dom_probe_function(),
    )?;
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["eval".to_string(), script],
        OutputFormat::Text,
        connection,
        use_stealth,
        headed,
        Some(timeout_ms),
    )?;
    let raw = match parse_stdout_json(&stdout) {
        Some(Value::String(payload)) => payload,
        Some(other) => other.to_string(),
        None => stdout.trim().to_string(),
    };
    parse_chatgpt_dom_state(raw.trim())
}

fn read_latest_chatgpt_response(
    connection: Option<&BrowserConnection>,
    use_stealth: bool,
    headed: bool,
    timeout_ms: u64,
) -> Result<String> {
    let expression = chatgpt_web::wrap_function_source_for_json_eval(
        &chatgpt_web::build_latest_response_probe_function(),
    )?;
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["eval".to_string(), expression],
        OutputFormat::Text,
        connection,
        use_stealth,
        headed,
        Some(timeout_ms),
    )?;
    let payload: Value = parse_stdout_json(&stdout)
        .with_context(|| format!("parse ChatGPT latest response result: {stdout}"))?;
    Ok(payload
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

fn parse_chatgpt_poll_args(
    action_name: &str,
    args: Option<&[String]>,
) -> Result<ChatgptPollOptions> {
    let mut options = ChatgptPollOptions {
        attempts: CHATGPT_POLL_ATTEMPTS_DEFAULT,
        interval_ms: CHATGPT_POLL_INTERVAL_MS_DEFAULT,
        timeout_ms: CHATGPT_POLL_TOTAL_TIMEOUT_MS_DEFAULT,
    };
    let mut attempts_explicit = false;
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
                attempts_explicit = true;
            }
            "--interval-ms" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| anyhow!("--interval-ms requires a value"))?;
                options.interval_ms = raw
                    .parse()
                    .with_context(|| format!("invalid --interval-ms value `{raw}`"))?;
            }
            "--timeout-ms" => {
                let raw = iter
                    .next()
                    .ok_or_else(|| anyhow!("--timeout-ms requires a value"))?;
                options.timeout_ms = raw
                    .parse()
                    .with_context(|| format!("invalid --timeout-ms value `{raw}`"))?;
            }
            other => return Err(anyhow!("unsupported {action_name} arg `{other}`")),
        }
    }
    if options.interval_ms == 0 {
        return Err(anyhow!("--interval-ms must be greater than 0"));
    }
    if options.timeout_ms == 0 {
        return Err(anyhow!("--timeout-ms must be greater than 0"));
    }
    // Derive attempts from timeout/interval when not explicitly set,
    // so the attempt cap never silently truncates a long timeout.
    if !attempts_explicit {
        options.attempts = options.timeout_ms.div_ceil(options.interval_ms) as usize;
    }
    if options.attempts == 0 {
        return Err(anyhow!("--attempts must be greater than 0"));
    }
    Ok(options)
}

fn parse_chatgpt_dom_state(raw: &str) -> Result<ChatgptDomState> {
    let mut send_state = None;
    let mut has_stop_button = None;
    let mut has_thinking_indicator = None;
    let mut copy_button_count = None;
    let mut assistant_msg_count: usize = 0;
    let mut assistant_last_len: usize = 0;
    let mut error = String::new();

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
            "thinking" => {
                has_thinking_indicator = Some(match value {
                    "0" => false,
                    "1" => true,
                    other => return Err(anyhow!("invalid thinking flag `{other}`")),
                });
            }
            "copy" => {
                copy_button_count = Some(
                    value
                        .parse()
                        .with_context(|| format!("invalid copy count `{value}`"))?,
                );
            }
            "msgs" => {
                assistant_msg_count = value.parse().unwrap_or(0);
            }
            "lastlen" => {
                assistant_last_len = value.parse().unwrap_or(0);
            }
            "err" if !value.is_empty() => {
                error = value.to_string();
            }
            "err" => {}
            _ => {}
        }
    }

    Ok(ChatgptDomState {
        send_state: send_state.ok_or_else(|| anyhow!("missing send state"))?,
        has_stop_button: has_stop_button.ok_or_else(|| anyhow!("missing stop flag"))?,
        has_thinking_indicator: has_thinking_indicator.unwrap_or(false),
        copy_button_count: copy_button_count.ok_or_else(|| anyhow!("missing copy count"))?,
        assistant_msg_count,
        assistant_last_len,
        error,
    })
}

/// Classify the ChatGPT page state into a [`CompletionVerdict`].
///
/// Pure function over the current DOM snapshot and the pre-send baseline. All
/// stability/timing logic lives in the caller (`run_chatgpt_wait_response`),
/// because the stable-idle window must scale with the configurable poll
/// interval — that is policy state, not DOM state.
fn classify_chatgpt_completion(
    dom: &ChatgptDomState,
    baseline_dom: &ChatgptDomState,
) -> CompletionVerdict {
    // After response completes, ChatGPT may show the voice button instead of
    // the send button — so `send_state` is `Missing`, not `Enabled`. Both
    // states indicate the composer is idle (not generating).
    let composer_idle = matches!(
        dom.send_state,
        ChatgptSendState::Enabled | ChatgptSendState::Missing
    );
    if !composer_idle || dom.has_stop_button || dom.has_thinking_indicator {
        return CompletionVerdict::Generating;
    }
    let new_msg = dom.assistant_msg_count > baseline_dom.assistant_msg_count;
    // Strong gate: copy button only renders after a *specific* assistant
    // message finishes streaming. Scope to a new message (msg_count grew past
    // baseline) so we don't accept a stale copy button left over from a prior
    // turn in the same tab.
    if new_msg && dom.copy_button_count > 0 {
        return CompletionVerdict::CopyButton;
    }
    let new_msg_with_text = new_msg && dom.assistant_last_len > 0;
    let same_message_grew = dom.assistant_msg_count > 0
        && dom.assistant_msg_count == baseline_dom.assistant_msg_count
        && dom.assistant_last_len > baseline_dom.assistant_last_len;
    if new_msg_with_text || same_message_grew {
        CompletionVerdict::Idle
    } else {
        CompletionVerdict::Generating
    }
}

/// Stable-idle window required before the fallback completion path fires.
/// `max(floor, multiplier * interval_ms)` — guarantees ≥3 consecutive identical
/// idle polls regardless of how `wait_interval_ms` is configured, and ≥90s of
/// real time even when the interval is short.
fn chatgpt_stable_idle_threshold_ms(interval_ms: u64) -> u64 {
    chatgpt_web::stable_idle_threshold_ms(interval_ms)
}

fn chatgpt_send_state_str(state: ChatgptSendState) -> &'static str {
    match state {
        ChatgptSendState::Enabled => "enabled",
        ChatgptSendState::Disabled => "disabled",
        ChatgptSendState::Missing => "missing",
    }
}

pub fn load_browser_defaults_with_profile(profile: Option<&str>) -> Result<BrowserDefaults> {
    load_browser_defaults_from_paths(browser_config_paths(profile))
}

fn load_browser_defaults_from_paths(paths: Vec<(PathBuf, bool)>) -> Result<BrowserDefaults> {
    let mut defaults = BrowserDefaults::default();
    for (path, trusted) in paths {
        if !path.exists() {
            continue;
        }
        let file = load_browser_config_file(&path)?;
        let Some(file_defaults) = file.defaults else {
            continue;
        };
        if trusted {
            merge_browser_defaults(&mut defaults, file_defaults);
        } else {
            if file_defaults.browser_profile.is_some() {
                eprintln!(
                    "warning: ignoring defaults.browser_profile from untrusted config {}",
                    path.display()
                );
            }
            if file_defaults.browser_cdp.is_some() {
                eprintln!(
                    "warning: ignoring defaults.browser_cdp from untrusted config {}",
                    path.display()
                );
            }
        }
    }
    Ok(defaults)
}

fn browser_config_paths(profile: Option<&str>) -> Vec<(PathBuf, bool)> {
    let mut paths: Vec<(PathBuf, bool)> = Vec::new();

    if let Some(home) = home_dir() {
        paths.push((home.join(".yoetz/config.toml"), true));
        paths.push((home.join(".config/yoetz/config.toml"), true));
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        paths.push((PathBuf::from(xdg).join("yoetz/config.toml"), true));
    }
    paths.push((PathBuf::from("./yoetz.toml"), false));

    if let Ok(custom) = env::var("YOETZ_CONFIG_PATH") {
        paths.push((PathBuf::from(custom), true));
    }

    if let Some(name) = profile {
        if let Some(home) = home_dir() {
            paths.push((
                home.join(".yoetz/profiles").join(format!("{name}.toml")),
                true,
            ));
            paths.push((
                home.join(".config/yoetz/profiles")
                    .join(format!("{name}.toml")),
                true,
            ));
        }
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            paths.push((
                PathBuf::from(xdg)
                    .join("yoetz/profiles")
                    .join(format!("{name}.toml")),
                true,
            ));
        }
        paths.push((PathBuf::from(format!("./yoetz.{name}.toml")), false));
    }

    paths
}

fn load_browser_config_file(path: &Path) -> Result<BrowserConfigFile> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("parse config {}", path.display()))
}

fn merge_browser_defaults(target: &mut BrowserDefaults, other: BrowserConfigDefaults) {
    if other.browser_profile.is_some() {
        target.profile = other.browser_profile;
    }
    if other.browser_cdp.is_some() {
        target.cdp = other.browser_cdp;
    }
}

pub fn resolve_profile_dir(
    browser_defaults: &BrowserDefaults,
    override_profile: Option<&PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = override_profile {
        return expand_tilde(path);
    }
    if let Ok(path) = env::var("YOETZ_BROWSER_PROFILE") {
        return expand_tilde(Path::new(&path));
    }
    if let Some(path) = browser_defaults.profile.as_deref() {
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

pub fn close_live_attach_session() -> Result<()> {
    close_live_attach_session_name(CDP_SESSION_NAME)
}

fn close_live_attach_session_name(session_name: &str) -> Result<()> {
    let (bin, prefix_args) = resolve_agent_browser()?;
    let mut cmd = Command::new(bin);
    cmd.args(prefix_args);
    match cmd.args(["--session", session_name, "close"]).output() {
        Ok(output) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("warning: session close failed: {stderr}");
        }
        Err(e) => eprintln!("warning: session close error: {e}"),
        _ => {}
    }
    Ok(())
}

pub fn close_browser_for_connection(connection: &BrowserConnection) -> Result<()> {
    match connection {
        BrowserConnection::AutoConnect => return close_live_attach_session(),
        BrowserConnection::Cdp { run_id, .. } => {
            let session_name = run_id
                .as_deref()
                .map(live_cdp_session_name_for_run)
                .unwrap_or_else(|| CDP_SESSION_NAME.to_string());
            return close_live_attach_session_name(&session_name);
        }
        BrowserConnection::CookieState { .. } | BrowserConnection::Profile { .. } => {}
    }
    close_browser_daemon()
}

pub fn live_cdp_session_name_for_run(run_id: &str) -> String {
    let mut suffix = run_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    suffix.truncate(50);
    let suffix = suffix.trim_matches('-');
    if suffix.is_empty() {
        CDP_SESSION_NAME.to_string()
    } else {
        format!("{CDP_SESSION_NAME}-{suffix}")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonRecoveryAction {
    NoSocket,
    Healthy,
    AwaitingApproval,
    KilledStale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonState {
    NoSocket,
    Healthy,
    AwaitingApproval,
    Stale,
}

#[derive(Debug)]
pub struct AttachAttemptLock {
    _lock_file: File,
    waited: bool,
}

impl AttachAttemptLock {
    pub fn waited(&self) -> bool {
        self.waited
    }
}

fn yoetz_lock_path(filename: &str) -> PathBuf {
    if let Some(home) = home_dir() {
        return home.join(".yoetz").join(filename);
    }
    PathBuf::from(".yoetz").join(filename)
}

fn acquire_waitable_lock(lock_path: &Path, action: &str) -> Result<(File, bool)> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .with_context(|| format!("open {action} {}", lock_path.display()))?;

    let waited = match file.try_lock_exclusive() {
        Ok(()) => false,
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
            file.lock_exclusive()
                .with_context(|| format!("lock {action} {}", lock_path.display()))?;
            true
        }
        Err(err) => {
            return Err(err).with_context(|| format!("lock {action} {}", lock_path.display()));
        }
    };

    Ok((file, waited))
}

pub fn acquire_attach_attempt_lock() -> Result<AttachAttemptLock> {
    let lock_path = yoetz_lock_path(CHROME_ATTACH_ATTEMPT_LOCK_FILENAME);
    let (file, waited) = acquire_waitable_lock(&lock_path, "browser attach attempt lock")?;

    Ok(AttachAttemptLock {
        _lock_file: file,
        waited,
    })
}

pub fn is_chrome_approval_wait_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    message.contains("allow remote debugging")
        || message.contains("remote-debugging consent")
        || message.contains("remote debugging consent")
}

/// Detect the actionable "Chrome CDP unreachable" error produced by
/// `chrome_devtools_mcp::chatgpt::cdp_attach_hint`.
///
/// When tier 1 (chrome-devtools-mcp) already determined that Chrome is not
/// listening on CDP, the funnel should skip all remaining *live* transports
/// — dev-browser and agent-browser are the same CDP endpoint behind
/// different adapters, so they will fail for the same reason (dev-browser's
/// Playwright `connectOverCDP` in particular hangs instead of failing fast).
/// Stopping at tier 1 and jumping to manual gives the user an immediate,
/// actionable error instead of a long wait.
pub fn is_chrome_cdp_unreachable_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    message.contains("chrome://inspect")
        && (message.contains("could not reach chrome's cdp endpoint")
            || message.contains("ignores --remote-debugging-port")
            || looks_like_cdp_transport_failure(err))
}

pub fn is_chrome_create_target_block_error(err: &anyhow::Error) -> bool {
    crate::chrome_devtools_mcp::client::is_external_create_target_block_error(err)
}

const CHATGPT_ATTACHED_PAGE_ERROR_MARKER: &str = "chatgpt attached page failed after live attach";

pub fn mark_chatgpt_attached_page_error(err: anyhow::Error) -> anyhow::Error {
    err.context(CHATGPT_ATTACHED_PAGE_ERROR_MARKER)
}

pub fn is_chatgpt_auth_issue_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    message.contains("chatgpt login required")
        || message.contains("cloudflare challenge detected")
        || message.contains("captcha detected")
}

pub fn is_chatgpt_attached_page_error(err: &anyhow::Error) -> bool {
    if err
        .chain()
        .any(|cause| cause.to_string() == CHATGPT_ATTACHED_PAGE_ERROR_MARKER)
    {
        return true;
    }

    let message = format!("{err:#}").to_lowercase();
    (message.contains("requested chatgpt model") && message.contains("not actually selected"))
        || (message.contains("requested model") && message.contains("was not selected"))
        || (message.contains("requested chatgpt model")
            && message.contains("was not available in the current session"))
        || message.contains("chatgpt composer did not mount")
        || message.contains("did not finish loading the composer")
        || message.contains("could not find an enabled chatgpt send button")
        || message.contains("no attach/upload controls were detected")
        || (message.contains("could not attach `") && message.contains("to chatgpt"))
        || message.contains("attachment chip for `")
        || message.contains("chatgpt send button never became enabled after typing")
        || message.contains("chatgpt send click did not trigger a ui transition")
        || message.contains("chatgpt response timed out after")
}

pub fn is_chatgpt_profile_selector_visibility_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    message.contains("profile_email `")
        && (message.contains("live chrome browser context")
            || message.contains("live auto-connect tab list"))
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

fn default_daemon_paths() -> Option<(PathBuf, PathBuf)> {
    let ab_dir = home_dir()?.join(".agent-browser");
    Some((ab_dir.join("default.pid"), ab_dir.join("default.sock")))
}

#[cfg(unix)]
fn is_daemon_socket_healthy(sock_path: &Path) -> bool {
    use std::os::unix::net::UnixStream;

    if !sock_path.exists() {
        return false;
    }
    UnixStream::connect(sock_path).is_ok()
}

#[cfg(not(unix))]
fn is_daemon_socket_healthy(_sock_path: &Path) -> bool {
    false
}

pub fn is_daemon_healthy() -> bool {
    let Some((_, sock_path)) = default_daemon_paths() else {
        return false;
    };
    is_daemon_socket_healthy(&sock_path)
}

fn read_daemon_pid(pid_path: &Path) -> Option<u32> {
    fs::read_to_string(pid_path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn process_command_line(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if command.is_empty() {
        None
    } else {
        Some(command)
    }
}

#[cfg(not(unix))]
fn process_command_line(_pid: u32) -> Option<String> {
    None
}

fn process_looks_like_agent_browser(pid: u32) -> bool {
    process_command_line(pid).is_some_and(|command| command.contains("agent-browser"))
}

fn socket_is_recent(sock_path: &Path, grace_window: Duration) -> bool {
    let Ok(metadata) = fs::metadata(sock_path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    age <= grace_window
}

fn inspect_daemon_with_paths(
    pid_path: &Path,
    sock_path: &Path,
    grace_window: Duration,
) -> DaemonState {
    if !sock_path.exists() {
        return DaemonState::NoSocket;
    }
    if is_daemon_socket_healthy(sock_path) {
        return DaemonState::Healthy;
    }

    if let Some(pid) = read_daemon_pid(pid_path) {
        if process_is_alive(pid)
            && process_looks_like_agent_browser(pid)
            && socket_is_recent(sock_path, grace_window)
        {
            return DaemonState::AwaitingApproval;
        }
    }

    DaemonState::Stale
}

pub fn inspect_default_daemon() -> DaemonState {
    let Some((pid_path, sock_path)) = default_daemon_paths() else {
        return DaemonState::NoSocket;
    };
    inspect_daemon_with_paths(&pid_path, &sock_path, DAEMON_APPROVAL_GRACE_WINDOW)
}

fn default_daemon_pid() -> Option<u32> {
    let (pid_path, _) = default_daemon_paths()?;
    read_daemon_pid(&pid_path)
}

fn render_daemon_state(state: DaemonState) -> &'static str {
    match state {
        DaemonState::NoSocket => "not running",
        DaemonState::Healthy => "healthy",
        DaemonState::AwaitingApproval => "awaiting approval",
        DaemonState::Stale => "stale",
    }
}

fn render_live_attach_daemon_health(health: crate::live_attach::DaemonHealth) -> &'static str {
    match health {
        crate::live_attach::DaemonHealth::NotRunning => "not running",
        crate::live_attach::DaemonHealth::Healthy => "healthy",
        crate::live_attach::DaemonHealth::Busy => "busy",
        crate::live_attach::DaemonHealth::Stale => "stale",
    }
}

fn inspect_browser_helpers() -> BrowserDoctorHelpers {
    let helper_processes = discover_browser_helper_processes();
    let dev_browser_processes = helper_processes
        .iter()
        .filter(|process| matches!(process.kind, BrowserHelperProcessKind::DevBrowserDaemon))
        .cloned()
        .collect::<Vec<_>>();
    let yoetz_live_cdp_processes = helper_processes
        .iter()
        .filter(|process| matches!(process.kind, BrowserHelperProcessKind::YoetzLiveCdpDaemon))
        .cloned()
        .collect::<Vec<_>>();
    let external_mcp_processes = helper_processes
        .iter()
        .filter(|process| {
            matches!(
                process.kind,
                BrowserHelperProcessKind::ChromeDevtoolsMcp
                    | BrowserHelperProcessKind::ChromeDevtoolsMcpWatchdog
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let agent_browser_default = inspect_default_daemon();
    let agent_browser_default_pid = default_daemon_pid();
    let live_attach_daemon = crate::live_attach::inspect_daemon_sync();

    BrowserDoctorHelpers {
        agent_browser_default,
        agent_browser_default_pid,
        live_attach_daemon: live_attach_daemon.clone(),
        recommended_actions: browser_doctor_recommended_actions(
            agent_browser_default,
            &live_attach_daemon,
            &yoetz_live_cdp_processes,
            &dev_browser_processes,
            &external_mcp_processes,
        ),
        yoetz_live_cdp_processes,
        dev_browser_processes,
        external_mcp_processes,
    }
}

fn browser_doctor_recommended_actions(
    agent_browser_default: DaemonState,
    live_attach_daemon: &crate::live_attach::DaemonSummary,
    yoetz_live_cdp_processes: &[BrowserHelperProcessSummary],
    dev_browser_processes: &[BrowserHelperProcessSummary],
    external_mcp_processes: &[BrowserHelperProcessSummary],
) -> Vec<String> {
    let mut actions = Vec::new();

    if matches!(
        live_attach_daemon.health,
        crate::live_attach::DaemonHealth::Stale
    ) {
        actions.push(
            "The yoetz live-attach daemon looks stale. Run `yoetz browser reset` before the next attach/check/recipe so the primary CDP owner restarts cleanly.".to_string(),
        );
    }
    if matches!(agent_browser_default, DaemonState::Stale) {
        actions.push(
            "Run `yoetz browser reset` before `yoetz browser attach`, `yoetz browser check`, or `yoetz browser recipe` to clear stale yoetz-owned helpers.".to_string(),
        );
    }
    if matches!(agent_browser_default, DaemonState::AwaitingApproval) {
        actions.push(
            "Agent-browser may still be waiting for Chrome's \"Allow remote debugging?\" dialog. Approve it in Chrome if this attach was intentional; otherwise run `yoetz browser reset`.".to_string(),
        );
    }
    if !dev_browser_processes.is_empty() {
        actions.push(
            "A dev-browser daemon is still running. If you want a clean yoetz-owned attach retry, run `yoetz browser reset` first.".to_string(),
        );
    }
    if !yoetz_live_cdp_processes.is_empty() {
        actions.push(
            "A yoetz live-CDP daemon is still running. If you want a clean bundled dev-browser attach retry, run `yoetz browser reset` first.".to_string(),
        );
    }
    if !external_mcp_processes.is_empty() {
        let pids = external_mcp_processes
            .iter()
            .map(|process| process.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        actions.push(format!(
            "External chrome-devtools-mcp processes are still running (pid {}). If they are not actively in use, close the owning tool/process first; `yoetz browser reset` will not stop them.",
            pids
        ));
    }
    if actions.is_empty() {
        actions.push("None required.".to_string());
    }

    actions
}

#[cfg(unix)]
fn discover_browser_helper_processes() -> Vec<BrowserHelperProcessSummary> {
    let Ok(output) = Command::new("ps")
        .args(["axww", "-o", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_browser_helper_process_line)
        .collect()
}

#[cfg(not(unix))]
fn discover_browser_helper_processes() -> Vec<BrowserHelperProcessSummary> {
    Vec::new()
}

fn parse_browser_helper_process_line(line: &str) -> Option<BrowserHelperProcessSummary> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_whitespace = trimmed.find(char::is_whitespace)?;
    let pid = trimmed[..first_whitespace].trim().parse::<u32>().ok()?;
    let command = trimmed[first_whitespace..].trim().to_string();
    let kind = if command.contains(".dev-browser/daemon.mjs") {
        BrowserHelperProcessKind::DevBrowserDaemon
    } else if command.contains(".yoetz/live-cdp-daemon.mjs")
        || command.contains(".yoetz\\live-cdp-daemon.mjs")
    {
        BrowserHelperProcessKind::YoetzLiveCdpDaemon
    } else if command.contains("chrome-devtools-mcp")
        && command.contains("telemetry/watchdog/main.js")
    {
        BrowserHelperProcessKind::ChromeDevtoolsMcpWatchdog
    } else if command.contains("chrome-devtools-mcp") {
        BrowserHelperProcessKind::ChromeDevtoolsMcp
    } else {
        return None;
    };

    Some(BrowserHelperProcessSummary { pid, kind, command })
}

fn force_kill_stale_daemon_with_paths(
    pid_path: &Path,
    sock_path: &Path,
    grace_window: Duration,
) -> DaemonRecoveryAction {
    match inspect_daemon_with_paths(pid_path, sock_path, grace_window) {
        DaemonState::NoSocket => return DaemonRecoveryAction::NoSocket,
        DaemonState::Healthy => return DaemonRecoveryAction::Healthy,
        DaemonState::AwaitingApproval => return DaemonRecoveryAction::AwaitingApproval,
        DaemonState::Stale => {}
    }

    if let Some(pid) = read_daemon_pid(pid_path) {
        if process_is_alive(pid) {
            if process_looks_like_agent_browser(pid) {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
            } else {
                eprintln!(
                    "warning: refusing to SIGKILL pid {} from {} because it does not look like agent-browser",
                    pid,
                    pid_path.display()
                );
            }
        }
    }

    let _ = fs::remove_file(pid_path);
    let _ = fs::remove_file(sock_path);
    // Brief pause for OS to reclaim resources
    thread::sleep(Duration::from_millis(500));
    DaemonRecoveryAction::KilledStale
}

/// Force-kill a stale agent-browser daemon by reading its PID file and sending
/// SIGKILL. Used when the daemon socket is unresponsive and `agent-browser close`
/// would hang. Only targets the "default" session.
pub fn force_kill_stale_daemon() -> DaemonRecoveryAction {
    let Some((pid_path, sock_path)) = default_daemon_paths() else {
        return DaemonRecoveryAction::NoSocket;
    };
    let action =
        force_kill_stale_daemon_with_paths(&pid_path, &sock_path, DAEMON_APPROVAL_GRACE_WINDOW);
    if matches!(action, DaemonRecoveryAction::AwaitingApproval) {
        eprintln!(
            "warning: existing agent-browser daemon looks recent and may be waiting for Chrome's \"Allow remote debugging?\" dialog. Approve it in Chrome, then retry."
        );
    }
    action
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
    // Check YOETZ_SCRIPTS_DIR env var (works for scripts)
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

/// Resolve a recipe path or bare recipe name.
///
/// Bare names such as `chatgpt` are resolved from installed recipe locations so
/// a caller's working tree cannot accidentally shadow built-ins with a same-name
/// directory.
pub fn resolve_recipe(path: &Path) -> Result<PathBuf> {
    if is_bare_recipe_name(path) {
        return resolve_recipe_name(path);
    }

    if path.exists() {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!(
            "recipe path {} is not a file; pass a recipe file path or a built-in recipe name like `chatgpt`",
            path.display()
        );
    }

    resolve_recipe_name(path)
}

fn resolve_recipe_name(path: &Path) -> Result<PathBuf> {
    let name = path.to_string_lossy();
    let filename = if name.ends_with(".yaml") || name.ends_with(".yml") {
        name.to_string()
    } else {
        format!("{name}.yaml")
    };

    find_data_file("recipes", &filename)
}

fn is_bare_recipe_name(path: &Path) -> bool {
    path.extension().is_none()
        && matches!(
            path.components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
}

/// Resolve CDP endpoint from flag → env → config (first non-empty wins).
pub fn resolve_cdp_endpoint(
    cdp_override: Option<&str>,
    browser_defaults: &BrowserDefaults,
) -> Option<String> {
    cdp_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| env::var("YOETZ_BROWSER_CDP").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| browser_defaults.cdp.clone())
        .filter(|value| !value.is_empty())
}

#[allow(dead_code)]
pub fn resolve_cdp_target(
    cdp_override: Option<&str>,
    browser_defaults: &BrowserDefaults,
) -> Result<Option<ResolvedCdpTarget>> {
    resolve_cdp_target_with_selector(cdp_override, None, browser_defaults, true)
}

#[allow(dead_code)]
pub fn resolve_cdp_target_with_implicit(
    cdp_override: Option<&str>,
    browser_defaults: &BrowserDefaults,
    allow_implicit: bool,
) -> Result<Option<ResolvedCdpTarget>> {
    resolve_cdp_target_with_selector(cdp_override, None, browser_defaults, allow_implicit)
}

pub fn resolve_cdp_target_with_selector(
    cdp_override: Option<&str>,
    browser_id: Option<&str>,
    browser_defaults: &BrowserDefaults,
    allow_implicit: bool,
) -> Result<Option<ResolvedCdpTarget>> {
    let browser_id = browser_id.map(str::trim).filter(|value| !value.is_empty());
    if cdp_override
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
        && browser_id.is_some()
    {
        bail!("pass either `--cdp` or `--browser-id`, not both");
    }
    if let Some(endpoint) = cdp_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    {
        return Ok(Some(ResolvedCdpTarget {
            description: format!("using explicit --cdp target `{endpoint}`"),
            endpoint,
            source: ResolvedCdpTargetSource::Flag,
            source_path: None,
            selected_target: None,
        }));
    }

    if let Some(browser_id) = browser_id {
        return Ok(Some(resolve_explicit_browser_id_target(browser_id)?));
    }

    if !allow_implicit {
        return Ok(None);
    }

    if let Some(endpoint) = env::var("YOETZ_BROWSER_CDP")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(ResolvedCdpTarget {
            description: format!("using YOETZ_BROWSER_CDP target `{endpoint}`"),
            endpoint,
            source: ResolvedCdpTargetSource::Env,
            source_path: None,
            selected_target: None,
        }));
    }

    if let Some(endpoint) = browser_defaults
        .cdp
        .clone()
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(ResolvedCdpTarget {
            description: format!("using defaults.browser_cdp target `{endpoint}`"),
            endpoint,
            source: ResolvedCdpTargetSource::Config,
            source_path: None,
            selected_target: None,
        }));
    }

    let mut targets = discover_running_chrome_targets();
    let mut sticky_source = match load_browser_target_state() {
        Ok(state) => state.last_source_path,
        Err(err) => {
            warn_invalid_browser_target_state(&err);
            let _ = clear_browser_target_state();
            None
        }
    };
    if sticky_source
        .as_ref()
        .is_some_and(|path| !targets.iter().any(|target| target.source_path == *path))
    {
        let _ = clear_browser_target_state();
        sticky_source = None;
    }
    let Some(target) =
        select_preferred_running_chrome_target(&mut targets, sticky_source.as_deref())
    else {
        return Ok(None);
    };
    let description = if sticky_source
        .as_deref()
        .is_some_and(|sticky| sticky == target.source_path.as_path())
    {
        format!(
            "reusing last successful Chrome target: {}",
            target.summary()
        )
    } else {
        format!("auto-selected running Chrome target: {}", target.summary())
    };
    Ok(Some(ResolvedCdpTarget {
        endpoint: target.ws_endpoint.clone(),
        source: ResolvedCdpTargetSource::Auto,
        description,
        source_path: Some(target.source_path.clone()),
        selected_target: Some(target),
    }))
}

fn resolve_explicit_browser_id_target(browser_id: &str) -> Result<ResolvedCdpTarget> {
    let discovered_targets = discover_running_chrome_targets();
    if let Some(target) = discovered_targets.iter().find(|target| {
        browser_id_from_ws_endpoint(&target.ws_endpoint).as_deref() == Some(browser_id)
    }) {
        return Ok(ResolvedCdpTarget {
            endpoint: target.ws_endpoint.clone(),
            source: ResolvedCdpTargetSource::Flag,
            description: format!(
                "using explicit --browser-id target `{browser_id}`: {}",
                target.summary()
            ),
            source_path: Some(target.source_path.clone()),
            selected_target: Some(target.clone()),
        });
    }

    let active_port_files = discover_devtools_active_port_files();
    if let Some(file) = find_active_port_file_by_browser_id(&active_port_files, browser_id) {
        let endpoint = file
            .ws_endpoint
            .clone()
            .context("active port file matched browser_id without websocket endpoint")?;
        return Ok(ResolvedCdpTarget {
            endpoint,
            source: ResolvedCdpTargetSource::Flag,
            description: format!(
                "using explicit --browser-id target `{browser_id}` from {}",
                file.path.display()
            ),
            source_path: Some(file.path.clone()),
            selected_target: None,
        });
    }

    if let Some(file) = find_stale_active_port_file_by_browser_id(&active_port_files, browser_id) {
        bail!(
            "browser_id `{browser_id}` matched {}, but that DevToolsActivePort file is stale/unreachable. Wait for a healthy Chrome DevTools target to appear in `yoetz browser doctor`, or use an explicit `--cdp` endpoint.",
            file.path.display()
        );
    }

    bail!(
        "browser_id `{browser_id}` did not match any local Chrome DevTools browser endpoint. Run `yoetz browser doctor` to list available browser IDs."
    )
}

fn find_active_port_file_by_browser_id<'a>(
    files: &'a [DevtoolsActivePortFile],
    browser_id: &str,
) -> Option<&'a DevtoolsActivePortFile> {
    files.iter().find(|file| {
        file.healthy
            && file
                .ws_endpoint
                .as_deref()
                .and_then(browser_id_from_ws_endpoint)
                .as_deref()
                == Some(browser_id)
    })
}

fn find_stale_active_port_file_by_browser_id<'a>(
    files: &'a [DevtoolsActivePortFile],
    browser_id: &str,
) -> Option<&'a DevtoolsActivePortFile> {
    files.iter().find(|file| {
        !file.healthy
            && file
                .ws_endpoint
                .as_deref()
                .and_then(browser_id_from_ws_endpoint)
                .as_deref()
                == Some(browser_id)
    })
}

pub fn auto_discovered_cdp_target_warning(target: &ResolvedCdpTarget) -> Option<String> {
    let targets = discover_running_chrome_targets();
    let processes = discover_local_chromium_processes();
    auto_discovered_cdp_target_warning_with_discovery(target, &targets, &processes)
}

fn auto_discovered_cdp_target_warning_with_discovery(
    target: &ResolvedCdpTarget,
    discovered_targets: &[RunningChromeTarget],
    local_processes: &[ChromiumProcessSummary],
) -> Option<String> {
    let selected = target.selected_running_target()?;
    if selected.has_chatgpt_tab() {
        return None;
    }

    let mut warning = if selected.page_samples.is_empty() {
        "auto-selected browser has no open ChatGPT tabs. yoetz will open ChatGPT in this browser/profile, which may not be the account you expect.".to_string()
    } else {
        format!(
            "auto-selected browser has no open ChatGPT tabs. Current sample tabs: {}. yoetz will open ChatGPT in this browser/profile, which may not be the account you expect.",
            selected.page_samples.join(", ")
        )
    };

    let alternative_targets = discovered_targets
        .iter()
        .filter(|candidate| {
            candidate.has_chatgpt_tab()
                && candidate.source_path != selected.source_path
                && candidate.ws_endpoint != selected.ws_endpoint
        })
        .take(2)
        .map(RunningChromeTarget::summary)
        .collect::<Vec<_>>();
    if !alternative_targets.is_empty() {
        warning.push_str(&format!(
            " Another discovered debug target already has ChatGPT tabs: {}.",
            alternative_targets.join(" | ")
        ));
    }

    let undebugged = local_processes
        .iter()
        .filter(|process| !process.has_remote_debugging)
        .take(3)
        .map(|process| format!("{} (pid {})", process.browser_name, process.pid))
        .collect::<Vec<_>>();
    if !undebugged.is_empty() {
        warning.push_str(&format!(
            " Other local Chromium processes without remote debugging: {}.",
            undebugged.join(", ")
        ));
        warning.push_str(
            " If the expected window is one of them, Chrome 136+ may be ignoring remote debugging on the default profile.",
        );
    }

    warning.push_str(" Run `yoetz browser doctor` to compare local Chromium targets.");

    Some(warning)
}

pub fn browser_doctor_report(live_probe: bool) -> String {
    let targets = discover_running_chrome_targets();
    let devtools_files = discover_devtools_active_port_files();
    let processes = discover_local_chromium_processes();
    let auto_connect = if live_probe {
        probe_auto_connect_tabs_for_doctor()
    } else {
        AutoConnectDoctorStatus::Skipped(
            "skipped by default; use `yoetz browser doctor --live` to probe the auto-connect helper"
                .to_string(),
        )
    };
    let helpers = inspect_browser_helpers();
    browser_doctor_report_with_discovery(
        &targets,
        &devtools_files,
        &processes,
        &auto_connect,
        &helpers,
    )
}

fn browser_doctor_report_with_discovery(
    targets: &[RunningChromeTarget],
    devtools_files: &[crate::chrome_devtools_mcp::client::DevtoolsActivePortFile],
    processes: &[ChromiumProcessSummary],
    auto_connect: &AutoConnectDoctorStatus,
    helpers: &BrowserDoctorHelpers,
) -> String {
    let mut lines = Vec::new();

    lines.push("Agent-browser auto-connect view:".to_string());
    match auto_connect {
        AutoConnectDoctorStatus::Reachable(tabs) => {
            let chatgpt_tabs = tabs
                .iter()
                .filter(|tab| {
                    let url = tab.url.to_ascii_lowercase();
                    url.contains("chatgpt.com") || url.contains("chat.openai.com")
                })
                .count();
            lines.push(format!(
                "  - reachable (tabs: {}, chatgpt tabs: {})",
                tabs.len(),
                chatgpt_tabs
            ));
            lines.push(
                "    note: this is the tab set exposed by the auto-connect helper; it may differ from the frontmost Chrome window/profile."
                    .to_string(),
            );
            if chatgpt_tabs == 0 {
                lines.push(
                    "    note: if a visible ChatGPT tab is missing here, auto-connect is attached to a different Chrome browsing context than the one you are looking at."
                        .to_string(),
                );
            }
            let inferred_emails = summarize_auto_connect_profile_emails(tabs);
            if !inferred_emails.is_empty() {
                lines.push("    inferred profile emails:".to_string());
                for (email, sample_tabs) in &inferred_emails {
                    let sample_suffix = if sample_tabs.is_empty() {
                        String::new()
                    } else {
                        format!("; sample tabs: {}", sample_tabs.join(", "))
                    };
                    lines.push(format!("      - {email}{sample_suffix}"));
                }
                if inferred_emails.len() > 1 {
                    lines.push(
                        "    note: multiple profile emails are visible; use `--var profile_email=<email>` to pin the ChatGPT recipe to one Chrome browser context."
                            .to_string(),
                    );
                }
            }
            for tab in tabs.iter().take(6) {
                let marker = if tab.active { "*" } else { "-" };
                let title = if tab.title.trim().is_empty() {
                    "<untitled>"
                } else {
                    tab.title.trim()
                };
                lines.push(format!(
                    "    {marker} tab {}: {} ({})",
                    tab.index, title, tab.url
                ));
            }
        }
        AutoConnectDoctorStatus::Unavailable(err) => {
            lines.push(format!("  - unavailable: {err}"));
        }
        AutoConnectDoctorStatus::Skipped(reason) => {
            lines.push(format!("  - skipped: {reason}"));
        }
    }

    lines.push("Running Chrome targets:".to_string());
    if targets.is_empty() {
        lines.push("  - none discovered".to_string());
        if matches!(auto_connect, AutoConnectDoctorStatus::Reachable(_)) {
            lines.push(
                "    note: auto-connect is reachable even though no raw DevToolsActivePort target was discovered."
                    .to_string(),
            );
        }
    } else {
        for target in targets {
            lines.push(format!("  - {}", target.summary()));
            if let Some(browser_id) = browser_id_from_ws_endpoint(&target.ws_endpoint) {
                lines.push(format!(
                    "    browser_id: {browser_id} (use `--browser-id {browser_id}`)"
                ));
            }
            lines.push(format!("    ws: {}", target.ws_endpoint));
        }
    }

    lines.push("DevToolsActivePort files:".to_string());
    if devtools_files.is_empty() {
        lines.push("  - none found".to_string());
    } else {
        for file in devtools_files {
            let modified = file
                .modified_at
                .and_then(|timestamp| timestamp.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let endpoint = file
                .ws_endpoint
                .clone()
                .unwrap_or_else(|| "unparseable".to_string());
            let health = if file.healthy {
                "healthy"
            } else {
                "stale/unreachable"
            };
            lines.push(format!(
                "  - {} [{health}] mtime-unix={modified} ws={endpoint}",
                file.path.display()
            ));
            if let Some(browser_id) = browser_id_from_ws_endpoint(&endpoint) {
                if file.healthy {
                    lines.push(format!(
                        "    browser_id: {browser_id} (use `--browser-id {browser_id}`)"
                    ));
                } else {
                    lines.push(format!(
                        "    browser_id: {browser_id} (stale/unusable until this DevTools target becomes healthy)"
                    ));
                }
            }
        }
    }

    lines.push("Local Chromium browser processes:".to_string());
    if processes.is_empty() {
        lines.push("  - none found".to_string());
    } else {
        for process in processes {
            let debug = if process.has_remote_debugging {
                "debuggable"
            } else {
                "no-remote-debugging"
            };
            let user_data_dir = process
                .user_data_dir
                .as_deref()
                .map(|value| format!(" user-data-dir={value}"))
                .unwrap_or_default();
            lines.push(format!(
                "  - {} pid={} [{debug}]{}",
                process.browser_name, process.pid, user_data_dir
            ));
            lines.push(format!("    cmd: {}", process.command));
        }
    }

    lines.push("Browser helpers:".to_string());
    lines.push(format!(
        "  - agent-browser default daemon: {}{}",
        render_daemon_state(helpers.agent_browser_default),
        helpers
            .agent_browser_default_pid
            .map(|pid| format!(" (pid {pid})"))
            .unwrap_or_default()
    ));
    lines.push("    note: only the default agent-browser session is inspected.".to_string());
    lines.push(format!(
        "  - yoetz live-attach daemon: {}{}",
        render_live_attach_daemon_health(helpers.live_attach_daemon.health),
        helpers
            .live_attach_daemon
            .pid
            .map(|pid| format!(
                " (pid {pid}, endpoints {}, aliases {}, poisoned {})",
                helpers.live_attach_daemon.endpoint_session_count,
                helpers.live_attach_daemon.target_alias_count,
                helpers.live_attach_daemon.poisoned_count
            ))
            .unwrap_or_default()
    ));
    lines.push(
        "    note: this is the primary chrome-devtools-mcp owner for yoetz attach/check/chatgpt recipe flows."
            .to_string(),
    );
    if matches!(
        helpers.live_attach_daemon.health,
        crate::live_attach::DaemonHealth::Busy
    ) {
        lines.push(
            "    note: status ping timed out while the owner was busy; this is expected during long-running attach/check/recipe work."
                .to_string(),
        );
    }

    if helpers.dev_browser_processes.is_empty() {
        lines.push("  - dev-browser daemon: not running".to_string());
    } else {
        lines.push(format!(
            "  - dev-browser daemon: running ({} process{})",
            helpers.dev_browser_processes.len(),
            if helpers.dev_browser_processes.len() == 1 {
                ""
            } else {
                "es"
            }
        ));
        for process in helpers.dev_browser_processes.iter().take(3) {
            lines.push(format!("    - pid {}: {}", process.pid, process.command));
        }
        if helpers.dev_browser_processes.len() > 3 {
            lines.push(format!(
                "    - … and {} more",
                helpers.dev_browser_processes.len() - 3
            ));
        }
    }

    if helpers.yoetz_live_cdp_processes.is_empty() {
        lines.push("  - yoetz live-CDP daemon: not running".to_string());
    } else {
        lines.push(format!(
            "  - yoetz live-CDP daemon: running ({} process{})",
            helpers.yoetz_live_cdp_processes.len(),
            if helpers.yoetz_live_cdp_processes.len() == 1 {
                ""
            } else {
                "es"
            }
        ));
        for process in helpers.yoetz_live_cdp_processes.iter().take(3) {
            lines.push(format!("    - pid {}: {}", process.pid, process.command));
        }
        if helpers.yoetz_live_cdp_processes.len() > 3 {
            lines.push(format!(
                "    - … and {} more",
                helpers.yoetz_live_cdp_processes.len() - 3
            ));
        }
    }

    if helpers.external_mcp_processes.is_empty() {
        lines.push("  - external cdp clients: none detected".to_string());
    } else {
        lines.push(format!(
            "  - external cdp clients: {} detected",
            helpers.external_mcp_processes.len()
        ));
        for process in &helpers.external_mcp_processes {
            lines.push(format!(
                "    - {} (pid {})",
                process.kind.label(),
                process.pid
            ));
        }
    }

    lines.push("Recommended actions:".to_string());
    for action in &helpers.recommended_actions {
        lines.push(format!("  - {action}"));
    }

    lines.join("\n")
}

fn probe_auto_connect_tabs_for_doctor() -> AutoConnectDoctorStatus {
    let connection = BrowserConnection::AutoConnect;
    let stdout = match run_agent_browser_with_connection_timeout(
        vec!["tab".to_string(), "list".to_string(), "--json".to_string()],
        OutputFormat::Json,
        Some(&connection),
        /* use_stealth */ false,
        /* headed */ false,
        Some(10_000),
    ) {
        Ok(stdout) => stdout,
        Err(err) => return AutoConnectDoctorStatus::Unavailable(format!("{err:#}")),
    };

    let parsed: AgentBrowserTabListEnvelope = match serde_json::from_str(stdout.trim()) {
        Ok(parsed) => parsed,
        Err(err) => {
            return AutoConnectDoctorStatus::Unavailable(format!(
                "invalid auto-connect tab list JSON: {err}"
            ));
        }
    };

    AutoConnectDoctorStatus::Reachable(parsed.data.tabs)
}

fn summarize_auto_connect_profile_emails(tabs: &[AgentBrowserTab]) -> Vec<(String, Vec<String>)> {
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for tab in tabs {
        let sample = if tab.title.trim().is_empty() {
            tab.url.trim().to_string()
        } else {
            tab.title.trim().to_string()
        };
        for email in infer_email_hints(&tab.title, &tab.url) {
            let samples = grouped.entry(email).or_default();
            if !sample.is_empty()
                && samples.len() < 3
                && !samples.iter().any(|existing| existing == &sample)
            {
                samples.push(sample.clone());
            }
        }
    }
    grouped.into_iter().collect()
}

pub fn remember_cdp_target(target: &ResolvedCdpTarget) -> Result<()> {
    let Some(source_path) = target.source_path.clone() else {
        return Ok(());
    };
    save_browser_target_state(&BrowserTargetState {
        last_source_path: Some(source_path),
    })
}

pub fn forget_cdp_target(target: &ResolvedCdpTarget) -> Result<()> {
    if target.source_path.is_none() {
        return Ok(());
    }
    clear_browser_target_state()
}

fn browser_target_state_path() -> PathBuf {
    if let Some(path) = env::var("YOETZ_BROWSER_TARGET_PATH")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(path);
    }
    if let Some(home) = home_dir() {
        return home.join(".yoetz").join(BROWSER_TARGET_STATE_FILENAME);
    }
    PathBuf::from(".yoetz").join(BROWSER_TARGET_STATE_FILENAME)
}

fn load_browser_target_state() -> Result<BrowserTargetState> {
    let path = browser_target_state_path();
    if !path.exists() {
        return Ok(BrowserTargetState::default());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("read browser target {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("parse browser target {}", path.display()))
}

fn save_browser_target_state(state: &BrowserTargetState) -> Result<()> {
    let path = browser_target_state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(state)?;
    let tmp_path = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp_path, data).with_context(|| format!("write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))
}

fn select_preferred_running_chrome_target(
    targets: &mut [RunningChromeTarget],
    sticky_source: Option<&Path>,
) -> Option<RunningChromeTarget> {
    targets.sort_by(|left, right| compare_running_chrome_targets(left, right, sticky_source));
    targets.first().cloned()
}

fn compare_running_chrome_targets(
    left: &RunningChromeTarget,
    right: &RunningChromeTarget,
    sticky_source: Option<&Path>,
) -> std::cmp::Ordering {
    let sticky_left = sticky_source.is_some_and(|path| path == left.source_path.as_path());
    let sticky_right = sticky_source.is_some_and(|path| path == right.source_path.as_path());
    right
        .has_chatgpt_tab()
        .cmp(&left.has_chatgpt_tab())
        .then_with(|| right.chatgpt_tab_count.cmp(&left.chatgpt_tab_count))
        .then_with(|| sticky_right.cmp(&sticky_left))
        .then_with(|| {
            browser_family_priority(&right.browser_name)
                .cmp(&browser_family_priority(&left.browser_name))
        })
        .then_with(|| {
            modified_sort_key(right.modified_at).cmp(&modified_sort_key(left.modified_at))
        })
        .then_with(|| left.source_path.cmp(&right.source_path))
}

fn browser_family_priority(browser_name: &str) -> u8 {
    let name = browser_name.to_ascii_lowercase();
    if name.contains("chrome beta") {
        3
    } else if name.contains("chrome canary") {
        2
    } else if name.contains("chrome") {
        4
    } else if name.contains("chromium") {
        1
    } else {
        0
    }
}

fn modified_sort_key(modified_at: Option<SystemTime>) -> u128 {
    modified_at
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
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
    browser_defaults: &BrowserDefaults,
    cdp_override: Option<&str>,
    profile_dir: &Path,
    target_url: &str,
) -> Result<BrowserConnection> {
    if let Some(endpoint) = resolve_cdp_endpoint(cdp_override, browser_defaults) {
        match try_cdp_attach(&endpoint, target_url) {
            Ok(()) => {
                return Ok(BrowserConnection::Cdp {
                    endpoint,
                    run_id: None,
                })
            }
            Err(err) if should_stop_live_attach_fallback(&err) => {
                return Err(err);
            }
            Err(_) => {}
        }
    }

    match try_auto_connect(target_url) {
        Ok(()) => return Ok(BrowserConnection::AutoConnect),
        Err(err) if should_stop_live_attach_fallback(&err) => {
            return Err(err);
        }
        Err(_) => {}
    }

    resolve_browser_connection_fallback(profile_dir, /* headed */ false, target_url)
}

pub fn try_cdp_attach(endpoint: &str, target_url: &str) -> Result<()> {
    let connection = BrowserConnection::Cdp {
        endpoint: endpoint.to_string(),
        run_id: None,
    };
    verify_auth_cdp(target_url, &connection).map_err(|e| {
        if allow_dialog_error(&e) {
            e
        } else if should_attach_chrome136_warning(endpoint, &e) {
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

fn should_attach_chrome136_warning(endpoint: &str, err: &anyhow::Error) -> bool {
    is_localhost_endpoint(endpoint) && looks_like_cdp_transport_failure(err)
}

fn should_stop_live_attach_fallback(err: &anyhow::Error) -> bool {
    is_chrome_approval_wait_error(err) || is_chrome_create_target_block_error(err)
}

fn looks_like_cdp_transport_failure(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    message.contains("could not reach chrome's cdp endpoint")
        || (message.contains("requesting `http")
            && message.contains("/json/version")
            && (message.contains("failed") || message.contains("connection refused")))
        || (message.contains("connectovercdp")
            && (message.contains("econnrefused")
                || message.contains("connection refused")
                || message.contains("browser.getversion")))
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
    if is_daemon_healthy() {
        // Daemon is running, but still verify auth state on the target page
        // to avoid false positives where the daemon is healthy but the
        // browser session is not authenticated.
        return verify_auth_cdp(target_url, &connection);
    }
    verify_auth_cdp(target_url, &connection)
}

fn clear_browser_target_state() -> Result<()> {
    let path = browser_target_state_path();
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn warn_invalid_browser_target_state(err: &anyhow::Error) {
    BROWSER_TARGET_STATE_WARNING_EMITTED.get_or_init(|| {
        eprintln!(
            "warning: failed to load browser target state ({err}); clearing sticky target state"
        );
    });
}

pub fn verify_auth_cdp(target_url: &str, connection: &BrowserConnection) -> Result<()> {
    if !connection.is_live_attach() {
        return Err(anyhow!(
            "verify_auth_cdp requires a live browser connection"
        ));
    }
    check_auth_with_connection_timeout(
        connection,
        /* headed */ false,
        target_url,
        Some(LIVE_ATTACH_COMMAND_TIMEOUT_MS),
    )
    .map_err(rewrite_live_attach_timeout)
}

pub fn resolve_auth(profile_dir: &Path, headed: bool) -> Result<BrowserConnection> {
    resolve_browser_connection_fallback(profile_dir, headed, CHATGPT_URL)
}

pub fn resolve_auth_mode(profile_dir: &Path, headed: bool) -> Result<BrowserProfileMode> {
    match resolve_auth(profile_dir, headed)? {
        BrowserConnection::CookieState { .. } => Ok(BrowserProfileMode::PreferState),
        BrowserConnection::Profile { .. } => Ok(BrowserProfileMode::ProfileOnly),
        BrowserConnection::Cdp { .. } | BrowserConnection::AutoConnect => Err(anyhow!(
            "managed auth mode cannot map a live browser connection"
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
    let command_timeout_ms = connection
        .is_live_attach()
        .then_some(LIVE_ATTACH_COMMAND_TIMEOUT_MS);
    check_auth_with_connection_timeout(connection, headed, target_url, command_timeout_ms)
}

fn check_auth_with_connection_timeout(
    connection: &BrowserConnection,
    headed: bool,
    target_url: &str,
    command_timeout_ms: Option<u64>,
) -> Result<()> {
    if !connection.is_live_attach() {
        let _ = close_browser_for_connection(connection);
    }
    let mut current_headed = headed;
    let use_stealth = !connection.is_live_attach();
    let verification_tab = if connection.is_live_attach() {
        open_live_attach_verification_tab(
            connection,
            use_stealth,
            current_headed,
            target_url,
            command_timeout_ms,
        )?
    } else {
        let _ = run_agent_browser_with_connection_timeout(
            vec!["open".to_string(), target_url.to_string()],
            OutputFormat::Text,
            Some(connection),
            use_stealth,
            current_headed,
            command_timeout_ms,
        )?;
        None
    };
    let deadline = Instant::now() + Duration::from_millis(auth_check_timeout_ms(connection));
    let mut last_issue: Option<&'static str>;
    loop {
        let snapshot = run_agent_browser_with_connection_timeout(
            vec![
                "snapshot".to_string(),
                "-c".to_string(),
                "--json".to_string(),
            ],
            OutputFormat::Json,
            Some(connection),
            use_stealth,
            current_headed,
            command_timeout_ms,
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
            close_live_attach_verification_tab(
                connection,
                use_stealth,
                current_headed,
                verification_tab.as_ref(),
                command_timeout_ms,
            );
            return Ok(());
        }

        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(AUTH_CHECK_POLL_MS));
    }

    close_live_attach_verification_tab(
        connection,
        use_stealth,
        current_headed,
        verification_tab.as_ref(),
        command_timeout_ms,
    );

    if let Some(issue) = last_issue {
        return Err(anyhow!("{issue}"));
    }
    Err(anyhow!(
        "auth check timed out without confirming authentication. \
         The page may still be loading. Try again or run `yoetz browser login`."
    ))
}

/// URL query parameter yoetz stamps onto auth-probe tabs so the cleanup path
/// can identify the probe tab unambiguously without reading tab ordering.
/// (Review finding #6: stop closing probe tabs by index.)
pub const YOETZ_PROBE_MARKER_PARAM: &str = "_yoetz_probe";

/// Opaque handle returned by [`open_live_attach_verification_tab`] so the
/// cleanup path can verify it is closing the exact probe tab it opened.
#[derive(Debug, Clone)]
pub struct LiveAttachProbeHandle {
    /// Marker value embedded in the probe tab URL as
    /// `?_yoetz_probe=<marker>` or `&_yoetz_probe=<marker>`.
    marker: String,
    /// Index the probe tab occupied on creation — kept only as a hint for
    /// logging; close still re-resolves by marker before touching Chrome.
    last_known_index: Option<usize>,
}

fn generate_probe_marker() -> String {
    format!(
        "yoetz-probe-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

pub(crate) fn mark_probe_url(target_url: &str, marker: &str) -> String {
    let trimmed = target_url.trim();
    if trimmed.is_empty() {
        return format!("about:blank?{YOETZ_PROBE_MARKER_PARAM}={marker}");
    }
    let separator = if trimmed.split('#').next().unwrap_or(trimmed).contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{trimmed}{separator}{YOETZ_PROBE_MARKER_PARAM}={marker}")
}

fn find_probe_tab(tabs: &[AgentBrowserTab], marker: &str) -> Option<usize> {
    if marker.is_empty() {
        return None;
    }
    let needle = format!("{YOETZ_PROBE_MARKER_PARAM}={marker}");
    tabs.iter()
        .find(|tab| tab.url.contains(&needle))
        .map(|tab| tab.index)
}

fn open_live_attach_verification_tab(
    connection: &BrowserConnection,
    use_stealth: bool,
    headed: bool,
    target_url: &str,
    command_timeout_ms: Option<u64>,
) -> Result<Option<LiveAttachProbeHandle>> {
    let marker = generate_probe_marker();
    let marked_url = mark_probe_url(target_url, &marker);
    let _ = run_agent_browser_with_connection_timeout(
        vec!["tab".to_string(), "new".to_string(), marked_url.clone()],
        OutputFormat::Text,
        Some(connection),
        use_stealth,
        headed,
        command_timeout_ms,
    )?;
    // Surface tab-list failures as warnings (don't swallow silently, but also
    // don't fail the whole auth check — the close path will re-try the list
    // and either resolve the probe by marker or warn again). This is the
    // review-finding-#6 requirement: stop the silent `.ok()` pattern.
    let last_known_index = match list_live_attach_tabs(
        connection,
        use_stealth,
        headed,
        command_timeout_ms,
    ) {
        Ok(tabs) => find_probe_tab(&tabs, &marker),
        Err(err) => {
            eprintln!(
                    "warn: listing Chrome tabs after opening the yoetz auth probe failed ({err:#}); close will re-resolve by marker `{marker}`"
                );
            None
        }
    };
    Ok(Some(LiveAttachProbeHandle {
        marker,
        last_known_index,
    }))
}

fn close_live_attach_verification_tab(
    connection: &BrowserConnection,
    use_stealth: bool,
    headed: bool,
    verification_tab: Option<&LiveAttachProbeHandle>,
    command_timeout_ms: Option<u64>,
) {
    if !connection.is_live_attach() {
        return;
    }
    let Some(handle) = verification_tab else {
        return;
    };
    // Always re-resolve the probe tab by its marker before closing so tab
    // churn between open and close cannot redirect the close to an unrelated
    // user tab (review finding #6).
    let tabs = match list_live_attach_tabs(connection, use_stealth, headed, command_timeout_ms) {
        Ok(tabs) => tabs,
        Err(err) => {
            eprintln!(
                "warn: could not re-list Chrome tabs before closing yoetz auth probe ({err:#}); leaving the probe tab open to avoid closing the wrong one"
            );
            return;
        }
    };
    let Some(index) = find_probe_tab(&tabs, &handle.marker) else {
        if let Some(hint) = handle.last_known_index {
            eprintln!(
                "warn: yoetz auth probe tab (marker `{}`, last known at index {hint}) was not found; it may have already been closed by the user",
                handle.marker
            );
        }
        return;
    };
    if run_agent_browser_with_connection_timeout(
        vec!["tab".to_string(), index.to_string()],
        OutputFormat::Text,
        Some(connection),
        use_stealth,
        headed,
        command_timeout_ms,
    )
    .is_err()
    {
        return;
    }
    let _ = run_agent_browser_with_connection_timeout(
        vec!["tab".to_string(), "close".to_string()],
        OutputFormat::Text,
        Some(connection),
        use_stealth,
        headed,
        command_timeout_ms,
    );
}

fn list_live_attach_tabs(
    connection: &BrowserConnection,
    use_stealth: bool,
    headed: bool,
    command_timeout_ms: Option<u64>,
) -> Result<Vec<AgentBrowserTab>> {
    let stdout = run_agent_browser_with_connection_timeout(
        vec!["tab".to_string(), "list".to_string(), "--json".to_string()],
        OutputFormat::Text,
        Some(connection),
        use_stealth,
        headed,
        command_timeout_ms,
    )?;
    let parsed: AgentBrowserTabListEnvelope =
        serde_json::from_str(stdout.trim()).with_context(|| {
            format!(
                "agent-browser returned invalid tab list JSON (first 200 chars): {}",
                &stdout[..stdout.len().min(200)]
            )
        })?;
    Ok(parsed.data.tabs)
}

fn maybe_select_live_attach_profile_tab(
    connection: &BrowserConnection,
    ctx: &RecipeContext,
    headed: bool,
) -> Result<()> {
    if ctx
        .vars
        .get("browser_context_id")
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }
    let Some(requested_email) = ctx
        .vars
        .get("profile_email")
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let tabs = list_live_attach_tabs(
        connection,
        ctx.use_stealth,
        headed,
        Some(LIVE_ATTACH_COMMAND_TIMEOUT_MS),
    )?;
    let selected = select_live_attach_profile_tab(&tabs, &requested_email)?;
    run_agent_browser_with_connection_timeout(
        vec!["tab".to_string(), selected.index.to_string()],
        OutputFormat::Text,
        Some(connection),
        ctx.use_stealth,
        headed,
        Some(LIVE_ATTACH_COMMAND_TIMEOUT_MS),
    )?;
    Ok(())
}

fn select_live_attach_profile_tab<'a>(
    tabs: &'a [AgentBrowserTab],
    requested_email: &str,
) -> Result<&'a AgentBrowserTab> {
    let matches = tabs
        .iter()
        .filter(|tab| {
            infer_email_hints(&tab.title, &tab.url)
                .into_iter()
                .any(|email| email == requested_email)
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        let visible = summarize_auto_connect_profile_emails(tabs)
            .into_iter()
            .map(|(email, _)| email)
            .collect::<Vec<_>>();
        if visible.is_empty() {
            bail!(
                "profile_email `{requested_email}` was not visible in the live auto-connect tab list"
            );
        }
        bail!(
            "profile_email `{requested_email}` was not visible in the live auto-connect tab list. Visible emails: {}",
            visible.join(", ")
        );
    }

    if matches.len() > 1 {
        bail!(
            "profile_email `{requested_email}` matched multiple live auto-connect tabs; refine the target before falling back to agent-browser"
        );
    }

    Ok(matches[0])
}

fn looks_like_timeout(err: &anyhow::Error) -> bool {
    let message = err.to_string().to_lowercase();
    message.contains("timed out") || message.contains("timeout")
}

fn allow_dialog_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    message.contains("allow remote debugging")
        || message.contains("remote-debugging consent")
        || message.contains("remote debugging consent")
}

fn live_attach_approval_wait_error(timeout_ms: u64) -> anyhow::Error {
    anyhow!(
        "live browser attach timed out ({}s). Chrome may be showing an \"Allow remote debugging?\" dialog — please click Allow in Chrome, then retry.",
        timeout_ms / 1000
    )
}

fn rewrite_live_attach_timeout(err: anyhow::Error) -> anyhow::Error {
    if !looks_like_timeout(&err) {
        return err;
    }
    live_attach_approval_wait_error(LIVE_ATTACH_COMMAND_TIMEOUT_MS)
}

fn auth_check_timeout_ms(connection: &BrowserConnection) -> u64 {
    if connection.is_live_attach() {
        LIVE_ATTACH_AUTH_CHECK_TIMEOUT_MS
    } else {
        AUTH_CHECK_TIMEOUT_MS
    }
}

/// Positive confirmation that the page is authenticated (ChatGPT loaded successfully).
/// Requires auth-specific markers (composer, sidebar controls) rather than branding
/// strings like "chatgpt" that also appear on logged-out pages.
fn looks_authenticated(snapshot: &str) -> bool {
    chatgpt_web::looks_authenticated_text(snapshot)
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
    chatgpt_web::is_challenge_text(snapshot)
}

fn detect_auth_issue_for_connection(
    snapshot: &str,
    connection: Option<&BrowserConnection>,
) -> Option<&'static str> {
    chatgpt_web::detect_auth_issue_text(
        snapshot,
        connection.is_some_and(BrowserConnection::is_live_attach),
    )
}

fn parse_stdout_json(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parsed: Value = serde_json::from_str(trimmed).ok()?;
    for _ in 0..4 {
        let Value::String(inner) = &parsed else {
            break;
        };
        let inner = inner.trim();
        // Unwrap only when the payload is itself another JSON container or
        // quoted JSON string; keep plain string scalars like "1" as strings.
        if !matches!(
            inner.as_bytes().first().copied(),
            Some(b'{') | Some(b'[') | Some(b'"')
        ) {
            break;
        }
        let Ok(next) = serde_json::from_str(inner) else {
            break;
        };
        parsed = next;
    }
    Some(parsed)
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

/// JSON-encode a string for safe embedding in JS source (e.g. `"hello \"world\""`).
/// `serde_json::to_string` on `&str` is infallible.
fn json_string_literal(s: &str) -> String {
    serde_json::to_string(s).unwrap()
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
        out = out.replace("{{bundle_path|json}}", &json_string_literal(path));
        out = out.replace("{{bundle_path}}", path);
    }
    for (key, value) in &ctx.vars {
        let json_needle = format!("{{{{{key}|json}}}}");
        out = out.replace(&json_needle, &json_string_literal(value));
        let needle = format!("{{{{{key}}}}}");
        out = out.replace(&needle, value);
    }
    if let Some(text) = bundle_text {
        out = out.replace("{{bundle_text|json}}", &json_string_literal(text));
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
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock as TestOnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: TestOnceLock<Mutex<()>> = TestOnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_env() -> MutexGuard<'static, ()> {
        env_lock().lock().unwrap_or_else(|e| e.into_inner())
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        #[allow(unsafe_code)]
        fn set<K>(key: &'static str, value: K) -> Self
        where
            K: AsRef<std::ffi::OsStr>,
        {
            let original = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            match &self.original {
                Some(value) => unsafe { env::set_var(self.key, value) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    struct CurrentDirGuard {
        original: PathBuf,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let original = env::current_dir().unwrap();
            env::set_current_dir(path).unwrap();
            Self { original }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            env::set_current_dir(&self.original).unwrap();
        }
    }

    fn no_live_attach_daemon() -> crate::live_attach::DaemonSummary {
        crate::live_attach::DaemonSummary {
            health: crate::live_attach::DaemonHealth::NotRunning,
            pid: None,
            session_count: 0,
            endpoint_session_count: 0,
            target_alias_count: 0,
            poisoned_count: 0,
        }
    }

    #[test]
    fn mark_probe_url_appends_param_without_existing_query() {
        let marked = mark_probe_url("https://chatgpt.com/", "run-abc");
        assert_eq!(marked, "https://chatgpt.com/?_yoetz_probe=run-abc");
    }

    #[test]
    fn mark_probe_url_appends_param_with_existing_query() {
        let marked = mark_probe_url("https://chatgpt.com/?foo=bar", "run-abc");
        assert_eq!(marked, "https://chatgpt.com/?foo=bar&_yoetz_probe=run-abc");
    }

    #[test]
    fn mark_probe_url_ignores_fragment_when_deciding_separator() {
        let marked = mark_probe_url("https://chatgpt.com/#chat", "run-abc");
        assert_eq!(marked, "https://chatgpt.com/#chat?_yoetz_probe=run-abc");
    }

    #[test]
    fn find_probe_tab_identifies_marker_ignoring_index_order() {
        let tabs = vec![
            AgentBrowserTab {
                index: 2,
                active: true,
                title: "Inbox".to_string(),
                url: "https://mail.google.com/".to_string(),
            },
            AgentBrowserTab {
                index: 7,
                active: false,
                title: "ChatGPT probe".to_string(),
                url: "https://chatgpt.com/?_yoetz_probe=run-abc".to_string(),
            },
            AgentBrowserTab {
                index: 0,
                active: false,
                title: "ChatGPT real".to_string(),
                url: "https://chatgpt.com/c/old".to_string(),
            },
        ];
        assert_eq!(find_probe_tab(&tabs, "run-abc"), Some(7));
        // A different marker must never resolve to a user-owned ChatGPT tab
        // (review finding #6: identity must be certain before closing).
        assert_eq!(find_probe_tab(&tabs, "other-run"), None);
        assert_eq!(find_probe_tab(&tabs, ""), None);
    }

    #[test]
    fn find_probe_tab_survives_index_churn_between_open_and_close() {
        // The probe tab's index changed from 3 at open to 1 at close; only
        // the URL marker is stable, so closing by stored index would destroy
        // an unrelated user tab.
        let tabs_before_close = vec![
            AgentBrowserTab {
                index: 1,
                active: false,
                title: "ChatGPT probe".to_string(),
                url: "https://chatgpt.com/?_yoetz_probe=run-xyz".to_string(),
            },
            AgentBrowserTab {
                index: 3,
                active: true,
                title: "Important Doc".to_string(),
                url: "https://docs.google.com/".to_string(),
            },
        ];
        assert_eq!(find_probe_tab(&tabs_before_close, "run-xyz"), Some(1));
    }

    fn recipe_context() -> RecipeContext {
        RecipeContext {
            bundle_path: Some("/tmp/bundle.md".to_string()),
            bundle_text: Some("hello world".to_string()),
            profile_dir: None,
            profile_mode: BrowserProfileMode::ProfileOnly,
            fallback_used: false,
            use_stealth: true,
            headed: false,
            target_url: CHATGPT_URL.to_string(),
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

    #[cfg(unix)]
    fn unique_unix_socket_dir(label: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let unique_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from("/tmp").join(format!(
            "ytz_{label}_{:x}_{:x}_{:x}",
            std::process::id(),
            nanos,
            unique_id
        ));
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

    fn fake_agent_browser_auth_bin() -> PathBuf {
        static BIN: TestOnceLock<PathBuf> = TestOnceLock::new();
        BIN.get_or_init(|| {
            let dir = unique_test_dir("fake_agent_browser_auth");
            let bin = command_path(&dir, "fake-agent-browser-auth");
            write_executable_script(
                &bin,
                "#!/bin/sh\nprintf 'timeout=%s args=%s\\n' \"$AGENT_BROWSER_DEFAULT_TIMEOUT\" \"$*\" >> \"$LOG_PATH\"\ncase \"$*\" in\n  *snapshot*) printf '{\"text\":\"ChatGPT - New chat\"}' ;;\nesac\n",
                "@echo off\r\necho timeout=%AGENT_BROWSER_DEFAULT_TIMEOUT% args=%*>> \"%LOG_PATH%\"\r\necho %* | findstr /c:\"snapshot\" >nul\r\nif %errorlevel%==0 echo {\"text\":\"ChatGPT - New chat\"}\r\n",
            );
            bin
        })
        .clone()
    }

    fn fake_agent_browser_auth_with_tab_list_bin() -> PathBuf {
        static BIN: TestOnceLock<PathBuf> = TestOnceLock::new();
        BIN.get_or_init(|| {
            let dir = unique_test_dir("fake_agent_browser_auth_tabs");
            let bin = command_path(&dir, "fake-agent-browser-auth-tabs");
            // The fake captures the URL passed to `tab new` so the subsequent
            // `tab list` response can echo it back — including the
            // `?_yoetz_probe=` marker yoetz now stamps on probe tabs
            // (review finding #6). Without this, the new probe tab's URL
            // would be static and the marker-based cleanup would refuse to
            // close it.
            write_executable_script(
                &bin,
                "#!/bin/sh\nprintf 'timeout=%s args=%s\\n' \"$AGENT_BROWSER_DEFAULT_TIMEOUT\" \"$*\" >> \"$LOG_PATH\"\ncase \"$*\" in\n  *\"tab new \"*)\n    probe_url=$(printf '%s' \"$*\" | sed -e 's/.*tab new //' -e 's/ .*//')\n    if [ -n \"$probe_url\" ]; then\n      printf '%s' \"$probe_url\" > \"${TAB_STATE_PATH}.url\"\n    fi\n    ;;\n  *\"tab list --json\"*)\n    count=0\n    if [ -f \"$TAB_STATE_PATH\" ]; then\n      count=$(cat \"$TAB_STATE_PATH\")\n    fi\n    if [ \"$count\" = \"0\" ]; then\n      printf '1' > \"$TAB_STATE_PATH\"\n      printf '{\"success\":true,\"data\":{\"tabs\":[{\"active\":false,\"index\":0,\"title\":\"Docs\",\"url\":\"https://docs.example.com/\"},{\"active\":true,\"index\":2,\"title\":\"Workspace\",\"url\":\"https://app.example.com/\"}]}}'\n    else\n      probe_url=\"https://chatgpt.com/\"\n      if [ -f \"${TAB_STATE_PATH}.url\" ]; then\n        probe_url=$(cat \"${TAB_STATE_PATH}.url\")\n      fi\n      printf '{\"success\":true,\"data\":{\"tabs\":[{\"active\":false,\"index\":0,\"title\":\"Docs\",\"url\":\"https://docs.example.com/\"},{\"active\":false,\"index\":2,\"title\":\"Workspace\",\"url\":\"https://app.example.com/\"},{\"active\":true,\"index\":5,\"title\":\"ChatGPT\",\"url\":\"%s\"}]}}' \"$probe_url\"\n    fi\n    ;;\n  *snapshot*) printf '{\"text\":\"ChatGPT - New chat\"}' ;;\nesac\n",
                "@echo off\r\necho timeout=%AGENT_BROWSER_DEFAULT_TIMEOUT% args=%*>> \"%LOG_PATH%\"\r\necho %* | findstr /c:\"tab new \" >nul\r\nif %errorlevel%==0 (\r\n  for /f \"tokens=* delims= \" %%A in (\"%*\") do set args=%%A\r\n  for /f \"tokens=3\" %%U in (\"%args%\") do (> \"%TAB_STATE_PATH%.url\" echo %%U)\r\n)\r\necho %* | findstr /c:\"tab list --json\" >nul\r\nif %errorlevel%==0 (\r\n  if not exist \"%TAB_STATE_PATH%\" (\r\n    > \"%TAB_STATE_PATH%\" echo 1\r\n    echo {\"success\":true,\"data\":{\"tabs\":[{\"active\":false,\"index\":0,\"title\":\"Docs\",\"url\":\"https://docs.example.com/\"},{\"active\":true,\"index\":2,\"title\":\"Workspace\",\"url\":\"https://app.example.com/\"}]}}\r\n  ) else (\r\n    set probe_url=https://chatgpt.com/\r\n    if exist \"%TAB_STATE_PATH%.url\" (\r\n      set /p probe_url=<\"%TAB_STATE_PATH%.url\"\r\n    )\r\n    echo {\"success\":true,\"data\":{\"tabs\":[{\"active\":false,\"index\":0,\"title\":\"Docs\",\"url\":\"https://docs.example.com/\"},{\"active\":false,\"index\":2,\"title\":\"Workspace\",\"url\":\"https://app.example.com/\"},{\"active\":true,\"index\":5,\"title\":\"ChatGPT\",\"url\":\"%probe_url%\"}]}}\r\n  )\r\n)\r\necho %* | findstr /c:\"snapshot\" >nul\r\nif %errorlevel%==0 echo {\"text\":\"ChatGPT - New chat\"}\r\n",
            );
            bin
        })
        .clone()
    }

    fn fake_agent_browser_timeout_bin() -> PathBuf {
        static BIN: TestOnceLock<PathBuf> = TestOnceLock::new();
        BIN.get_or_init(|| {
            let dir = unique_test_dir("fake_agent_browser_timeout");
            let bin = command_path(&dir, "fake-agent-browser-timeout");
            write_executable_script(
                &bin,
                "#!/bin/sh\nprintf 'timed out waiting for Chrome approval\\n' >&2\nexit 1\n",
                "@echo off\r\necho timed out waiting for Chrome approval 1>&2\r\nexit /b 1\r\n",
            );
            bin
        })
        .clone()
    }

    fn fake_agent_browser_focus_bin() -> PathBuf {
        static BIN: TestOnceLock<PathBuf> = TestOnceLock::new();
        BIN.get_or_init(|| {
            let dir = unique_test_dir("fake_agent_browser_focus");
            let bin = command_path(&dir, "fake-agent-browser-focus");
            write_executable_script(
                &bin,
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$LOG_PATH"
case "$*" in
  *"tab list --json"*)
    printf '{"success":true,"data":{"tabs":[{"active":true,"index":1,"title":"Docs","url":"https://docs.example.com/"},{"active":false,"index":4,"title":"ChatGPT","url":"https://chatgpt.com/?_yoetz=run-focus"}]}}'
    ;;
  *" eval "*)
    case "$*" in
      *"windowName"*)
        printf '{"windowName":"yoetz:run-focus","url":"https://chatgpt.com/?_yoetz=run-focus"}'
        exit 0
        ;;
    esac
    count=0
    if [ -f "$EVAL_COUNT_PATH" ]; then count=$(cat "$EVAL_COUNT_PATH"); fi
    count=$((count + 1))
    printf '%s' "$count" > "$EVAL_COUNT_PATH"
    if [ "$count" = "1" ]; then
      printf '{"status":"already-selected","currentLabel":"GPT-5 Pro"}'
    elif [ "$count" = "2" ]; then
      printf '{"status":"marked"}'
    else
      printf '{"status":"sent","assistantCountBeforeSend":0,"assistantLastLenBeforeSend":0}'
    fi
    ;;
esac
"#,
                r#"@echo off
setlocal enabledelayedexpansion
echo %*>> "%LOG_PATH%"
echo %* | findstr /c:"tab list --json" >nul
if %errorlevel%==0 (
  echo {"success":true,"data":{"tabs":[{"active":true,"index":1,"title":"Docs","url":"https://docs.example.com/"},{"active":false,"index":4,"title":"ChatGPT","url":"https://chatgpt.com/?_yoetz=run-focus"}]}}
  exit /b 0
)
echo %* | findstr /c:" eval " >nul
if %errorlevel%==0 (
  echo %* | findstr /c:"windowName" >nul
  if !errorlevel!==0 (
    echo {"windowName":"yoetz:run-focus","url":"https://chatgpt.com/?_yoetz=run-focus"}
    exit /b 0
  )
  set count=0
  if exist "%EVAL_COUNT_PATH%" (
    set /p count=<"%EVAL_COUNT_PATH%"
  )
  set /a count=count+1
  > "%EVAL_COUNT_PATH%" echo !count!
  if "!count!"=="1" (
    echo {"status":"already-selected","currentLabel":"GPT-5 Pro"}
  ) else if "!count!"=="2" (
    echo {"status":"marked"}
  ) else (
    echo {"status":"sent","assistantCountBeforeSend":0,"assistantLastLenBeforeSend":0}
  )
)
"#,
            );
            bin
        })
        .clone()
    }

    #[test]
    fn detect_agent_browser_in_path_does_not_fall_back_to_npx() {
        let _guard = lock_env();
        let original_path = env::var_os("PATH");
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(
                "PATH",
                std::env::temp_dir().join("yoetz-missing-agent-browser-path"),
            );
        }

        let result = detect_agent_browser_in_path();

        #[allow(unsafe_code)]
        unsafe {
            match original_path {
                Some(value) => env::set_var("PATH", value),
                None => env::remove_var("PATH"),
            }
        }

        let err = result.unwrap_err();
        assert!(err.contains("agent-browser not found in PATH"));
        assert!(!err.contains("npx"));
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
            run_id: None,
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
        let _guard = lock_env();
        let _cdp_env = EnvVarGuard::set("YOETZ_BROWSER_CDP", "http://127.0.0.1:9000");
        let browser_defaults = BrowserDefaults {
            cdp: Some("http://127.0.0.1:9222".to_string()),
            ..Default::default()
        };

        let from_flag = resolve_cdp_endpoint(Some("http://127.0.0.1:9333"), &browser_defaults);
        assert_eq!(from_flag.as_deref(), Some("http://127.0.0.1:9333"));

        let from_env = resolve_cdp_endpoint(None, &browser_defaults);
        assert_eq!(from_env.as_deref(), Some("http://127.0.0.1:9000"));

        unsafe {
            env::remove_var("YOETZ_BROWSER_CDP");
        }
        let from_config = resolve_cdp_endpoint(None, &browser_defaults);
        assert_eq!(from_config.as_deref(), Some("http://127.0.0.1:9222"));
    }

    #[test]
    fn load_browser_defaults_from_paths_uses_only_trusted_files() {
        let trusted_dir = unique_test_dir("browser_defaults_trusted");
        let trusted_path = trusted_dir.join("config.toml");
        fs::write(
            &trusted_path,
            r#"
[defaults]
browser_profile = "/safe/profile"
browser_cdp = "http://127.0.0.1:9222"
"#,
        )
        .unwrap();

        let untrusted_dir = unique_test_dir("browser_defaults_untrusted");
        let untrusted_path = untrusted_dir.join("yoetz.toml");
        fs::write(
            &untrusted_path,
            r#"
[defaults]
browser_profile = "/tmp/evil-profile"
browser_cdp = "http://evil.example.com:9222"
"#,
        )
        .unwrap();

        let defaults =
            load_browser_defaults_from_paths(vec![(untrusted_path, false), (trusted_path, true)])
                .unwrap();

        assert_eq!(defaults.profile.as_deref(), Some("/safe/profile"));
        assert_eq!(defaults.cdp.as_deref(), Some("http://127.0.0.1:9222"));
    }

    #[test]
    #[allow(unsafe_code)]
    fn resolve_cdp_target_can_suppress_implicit_live_targets() {
        let _guard = lock_env();
        let _cdp_env = EnvVarGuard::set("YOETZ_BROWSER_CDP", "http://127.0.0.1:9000");
        let browser_defaults = BrowserDefaults {
            cdp: Some("http://127.0.0.1:9222".to_string()),
            ..Default::default()
        };

        let suppressed = resolve_cdp_target_with_implicit(None, &browser_defaults, false).unwrap();
        assert!(suppressed.is_none());

        let explicit = resolve_cdp_target_with_implicit(
            Some("http://127.0.0.1:9333"),
            &browser_defaults,
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(explicit.endpoint, "http://127.0.0.1:9333");
        assert_eq!(explicit.source, ResolvedCdpTargetSource::Flag);
    }

    #[test]
    fn only_explicit_cdp_targets_are_authoritative() {
        let explicit = ResolvedCdpTarget {
            endpoint: "ws://127.0.0.1:9222/devtools/browser/flag".to_string(),
            source: ResolvedCdpTargetSource::Flag,
            description: "explicit".to_string(),
            source_path: None,
            selected_target: None,
        };
        let configured = ResolvedCdpTarget {
            endpoint: "ws://127.0.0.1:9222/devtools/browser/config".to_string(),
            source: ResolvedCdpTargetSource::Config,
            description: "config".to_string(),
            source_path: None,
            selected_target: None,
        };
        let automatic = ResolvedCdpTarget {
            endpoint: "ws://127.0.0.1:9222/devtools/browser/auto".to_string(),
            source: ResolvedCdpTargetSource::Auto,
            description: "auto".to_string(),
            source_path: Some(PathBuf::from("/tmp/DevToolsActivePort")),
            selected_target: None,
        };

        assert!(explicit.is_authoritative());
        assert!(!configured.is_authoritative());
        assert!(!automatic.is_authoritative());
    }

    #[test]
    fn select_preferred_running_chrome_target_prefers_chatgpt_before_sticky() {
        let sticky_path = PathBuf::from("/tmp/chrome-sticky/DevToolsActivePort");
        let older = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let newer = SystemTime::UNIX_EPOCH + Duration::from_secs(20);
        let mut targets = vec![
            RunningChromeTarget {
                ws_endpoint: "ws://127.0.0.1:9333/devtools/browser/sticky".to_string(),
                source_path: sticky_path.clone(),
                browser_name: "Brave".to_string(),
                chatgpt_tab_count: 0,
                page_target_count: 2,
                page_samples: vec!["Inbox".to_string(), "Calendar".to_string()],
                modified_at: Some(older),
            },
            RunningChromeTarget {
                ws_endpoint: "ws://127.0.0.1:9222/devtools/browser/chatgpt".to_string(),
                source_path: PathBuf::from("/tmp/chrome-default/DevToolsActivePort"),
                browser_name: "Chrome".to_string(),
                chatgpt_tab_count: 2,
                page_target_count: 3,
                page_samples: vec!["ChatGPT - New chat".to_string()],
                modified_at: Some(newer),
            },
        ];

        let sticky_choice =
            select_preferred_running_chrome_target(&mut targets, Some(sticky_path.as_path()))
                .unwrap();
        assert_eq!(
            sticky_choice.ws_endpoint,
            "ws://127.0.0.1:9222/devtools/browser/chatgpt"
        );

        let chatgpt_choice = select_preferred_running_chrome_target(&mut targets, None).unwrap();
        assert_eq!(
            chatgpt_choice.ws_endpoint,
            "ws://127.0.0.1:9222/devtools/browser/chatgpt"
        );
    }

    #[test]
    fn remember_cdp_target_persists_auto_selected_source_path() {
        let _guard = lock_env();
        let dir = unique_test_dir("browser_target_state");
        let state_path = dir.join("browser-target.json");
        let _state_env = EnvVarGuard::set("YOETZ_BROWSER_TARGET_PATH", &state_path);

        let target = ResolvedCdpTarget {
            endpoint: "ws://127.0.0.1:9222/devtools/browser/test".to_string(),
            source: ResolvedCdpTargetSource::Auto,
            description: "auto-selected running Chrome target".to_string(),
            source_path: Some(PathBuf::from("/tmp/chrome-default/DevToolsActivePort")),
            selected_target: None,
        };

        remember_cdp_target(&target).unwrap();
        let loaded = load_browser_target_state().unwrap();
        assert_eq!(
            loaded.last_source_path,
            Some(PathBuf::from("/tmp/chrome-default/DevToolsActivePort"))
        );
    }

    #[test]
    fn auto_discovered_cdp_target_warning_omits_healthy_chatgpt_target() {
        let selected = RunningChromeTarget {
            ws_endpoint: "ws://127.0.0.1:9222/devtools/browser/chatgpt".to_string(),
            source_path: PathBuf::from("/tmp/chrome-default/DevToolsActivePort"),
            browser_name: "Chrome".to_string(),
            chatgpt_tab_count: 1,
            page_target_count: 1,
            page_samples: vec!["ChatGPT - New chat".to_string()],
            modified_at: Some(SystemTime::UNIX_EPOCH),
        };
        let target = ResolvedCdpTarget {
            endpoint: selected.ws_endpoint.clone(),
            source: ResolvedCdpTargetSource::Auto,
            description: "auto-selected".to_string(),
            source_path: Some(selected.source_path.clone()),
            selected_target: Some(selected.clone()),
        };

        let warning = auto_discovered_cdp_target_warning_with_discovery(&target, &[selected], &[]);
        assert!(warning.is_none());
    }

    #[test]
    fn auto_discovered_cdp_target_warning_mentions_alternative_target_and_profile_hint() {
        let selected = RunningChromeTarget {
            ws_endpoint: "ws://127.0.0.1:9222/devtools/browser/work".to_string(),
            source_path: PathBuf::from("/tmp/chrome-work/DevToolsActivePort"),
            browser_name: "Chrome".to_string(),
            chatgpt_tab_count: 0,
            page_target_count: 2,
            page_samples: vec!["Inbox".to_string(), "Calendar".to_string()],
            modified_at: Some(SystemTime::UNIX_EPOCH),
        };
        let alternative = RunningChromeTarget {
            ws_endpoint: "ws://127.0.0.1:9333/devtools/browser/personal".to_string(),
            source_path: PathBuf::from("/tmp/chrome-personal/DevToolsActivePort"),
            browser_name: "Chrome".to_string(),
            chatgpt_tab_count: 2,
            page_target_count: 3,
            page_samples: vec!["ChatGPT - New chat".to_string()],
            modified_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        };
        let target = ResolvedCdpTarget {
            endpoint: selected.ws_endpoint.clone(),
            source: ResolvedCdpTargetSource::Auto,
            description: "auto-selected".to_string(),
            source_path: Some(selected.source_path.clone()),
            selected_target: Some(selected.clone()),
        };
        let processes = [ChromiumProcessSummary {
            pid: 2706,
            browser_name: "Chrome".to_string(),
            command: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string(),
            has_remote_debugging: false,
            user_data_dir: None,
        }];

        let warning = auto_discovered_cdp_target_warning_with_discovery(
            &target,
            &[selected, alternative.clone()],
            &processes,
        )
        .unwrap();
        assert!(warning.contains("Current sample tabs: Inbox, Calendar"));
        assert!(warning.contains("Another discovered debug target already has ChatGPT tabs"));
        assert!(warning.contains(&alternative.summary()));
        assert!(warning.contains("Chrome 136+ may be ignoring remote debugging"));
        assert!(warning.contains("yoetz browser doctor"));
    }

    #[test]
    fn browser_doctor_report_renders_empty_sections() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[],
            &[],
            &AutoConnectDoctorStatus::Unavailable("probe failed".to_string()),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::NoSocket,
                agent_browser_default_pid: None,
                live_attach_daemon: no_live_attach_daemon(),
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains("Agent-browser auto-connect view:"));
        assert!(report.contains("probe failed"));
        assert!(report.contains("Running Chrome targets:"));
        assert!(report.contains("  - none discovered"));
        assert!(report.contains("DevToolsActivePort files:"));
        assert!(report.contains("  - none found"));
        assert!(report.contains("Local Chromium browser processes:"));
        assert!(report.contains("Browser helpers:"));
        assert!(report.contains("agent-browser default daemon: not running"));
        assert!(report.contains("dev-browser daemon: not running"));
        assert!(report.contains("yoetz live-CDP daemon: not running"));
        assert!(report.contains("external cdp clients: none detected"));
        assert!(report.contains("Recommended actions:"));
        assert!(report.contains("None required."));
    }

    #[test]
    fn browser_doctor_report_can_skip_live_auto_connect_probe() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[],
            &[],
            &AutoConnectDoctorStatus::Skipped(
                "skipped by default; use `yoetz browser doctor --live` to probe the auto-connect helper"
                    .to_string(),
            ),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::NoSocket,
                agent_browser_default_pid: None,
                live_attach_daemon: no_live_attach_daemon(),
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains("Agent-browser auto-connect view:"));
        assert!(report.contains("skipped by default"));
        assert!(report.contains("yoetz browser doctor --live"));
    }

    #[test]
    fn browser_doctor_report_includes_auto_connect_tabs() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[],
            &[],
            &AutoConnectDoctorStatus::Reachable(vec![
                AgentBrowserTab {
                    index: 3,
                    active: true,
                    title: "ChatGPT".to_string(),
                    url: "https://chatgpt.com/".to_string(),
                },
                AgentBrowserTab {
                    index: 1,
                    active: false,
                    title: "Inbox".to_string(),
                    url: "https://mail.google.com/".to_string(),
                },
            ]),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::NoSocket,
                agent_browser_default_pid: None,
                live_attach_daemon: no_live_attach_daemon(),
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains("reachable (tabs: 2, chatgpt tabs: 1)"));
        assert!(report.contains("tab set exposed by the auto-connect helper"));
        assert!(report.contains("* tab 3: ChatGPT (https://chatgpt.com/)"));
        assert!(report.contains("- tab 1: Inbox (https://mail.google.com/)"));
    }

    #[test]
    fn browser_doctor_report_warns_when_auto_connect_and_raw_targets_disagree() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[],
            &[],
            &AutoConnectDoctorStatus::Reachable(vec![AgentBrowserTab {
                index: 9,
                active: true,
                title: "<untitled>".to_string(),
                url: "about:blank".to_string(),
            }]),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::NoSocket,
                agent_browser_default_pid: None,
                live_attach_daemon: no_live_attach_daemon(),
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains("chatgpt tabs: 0"));
        assert!(report.contains("different Chrome browsing context"));
        assert!(report.contains("no raw DevToolsActivePort target was discovered"));
    }

    #[test]
    fn browser_doctor_report_surfaces_inferred_profile_emails() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[],
            &[],
            &AutoConnectDoctorStatus::Reachable(vec![
                AgentBrowserTab {
                    index: 0,
                    active: false,
                    title: "Inbox (43,617) - personal@example.com - Gmail".to_string(),
                    url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
                },
                AgentBrowserTab {
                    index: 1,
                    active: false,
                    title: "Inbox (6,096) - work@example.com - Work Mail".to_string(),
                    url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
                },
            ]),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::NoSocket,
                agent_browser_default_pid: None,
                live_attach_daemon: no_live_attach_daemon(),
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains("inferred profile emails"));
        assert!(report.contains("personal@example.com"));
        assert!(report.contains("work@example.com"));
        assert!(report.contains("use `--var profile_email=<email>`"));
    }

    #[test]
    fn browser_doctor_report_surfaces_browser_ids() {
        let report = browser_doctor_report_with_discovery(
            &[RunningChromeTarget {
                ws_endpoint: "ws://127.0.0.1:9222/devtools/browser/browser-work".to_string(),
                source_path: PathBuf::from("/tmp/chrome-work/DevToolsActivePort"),
                browser_name: "Chrome".to_string(),
                chatgpt_tab_count: 1,
                page_target_count: 2,
                page_samples: vec!["ChatGPT".to_string()],
                modified_at: Some(SystemTime::UNIX_EPOCH),
            }],
            &[DevtoolsActivePortFile {
                path: PathBuf::from("/tmp/chrome-work/DevToolsActivePort"),
                ws_endpoint: Some("ws://127.0.0.1:9222/devtools/browser/browser-work".to_string()),
                modified_at: Some(SystemTime::UNIX_EPOCH),
                healthy: true,
            }],
            &[],
            &AutoConnectDoctorStatus::Unavailable("probe failed".to_string()),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::NoSocket,
                agent_browser_default_pid: None,
                live_attach_daemon: no_live_attach_daemon(),
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains("browser_id: browser-work"));
        assert!(report.contains("--browser-id browser-work"));
    }

    #[test]
    fn browser_doctor_report_marks_stale_browser_ids_unusable() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[DevtoolsActivePortFile {
                path: PathBuf::from("/tmp/chrome-stale/DevToolsActivePort"),
                ws_endpoint: Some("ws://127.0.0.1:9222/devtools/browser/browser-stale".to_string()),
                modified_at: Some(SystemTime::UNIX_EPOCH),
                healthy: false,
            }],
            &[],
            &AutoConnectDoctorStatus::Skipped("skip".to_string()),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::NoSocket,
                agent_browser_default_pid: None,
                live_attach_daemon: no_live_attach_daemon(),
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains("browser_id: browser-stale"));
        assert!(report.contains("stale/unusable"));
        assert!(!report.contains("use `--browser-id browser-stale`"));
    }

    #[test]
    fn browser_doctor_report_surfaces_helper_recommendations() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[],
            &[],
            &AutoConnectDoctorStatus::Unavailable("probe failed".to_string()),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::Stale,
                agent_browser_default_pid: Some(1234),
                live_attach_daemon: crate::live_attach::DaemonSummary {
                    health: crate::live_attach::DaemonHealth::Stale,
                    pid: Some(4444),
                    session_count: 0,
                    endpoint_session_count: 0,
                    target_alias_count: 0,
                    poisoned_count: 0,
                },
                yoetz_live_cdp_processes: vec![BrowserHelperProcessSummary {
                    pid: 2223,
                    kind: BrowserHelperProcessKind::YoetzLiveCdpDaemon,
                    command: "node /Users/test/.yoetz/live-cdp-daemon.mjs".to_string(),
                }],
                dev_browser_processes: vec![BrowserHelperProcessSummary {
                    pid: 2222,
                    kind: BrowserHelperProcessKind::DevBrowserDaemon,
                    command: "node /Users/test/.dev-browser/daemon.mjs".to_string(),
                }],
                external_mcp_processes: vec![
                    BrowserHelperProcessSummary {
                        pid: 3333,
                        kind: BrowserHelperProcessKind::ChromeDevtoolsMcp,
                        command: "chrome-devtools-mcp".to_string(),
                    },
                    BrowserHelperProcessSummary {
                        pid: 3334,
                        kind: BrowserHelperProcessKind::ChromeDevtoolsMcpWatchdog,
                        command: "node .../telemetry/watchdog/main.js".to_string(),
                    },
                ],
                recommended_actions: vec![
                    "Run `yoetz browser reset` before `yoetz browser attach`, `yoetz browser check`, or `yoetz browser recipe` to clear stale yoetz-owned helpers.".to_string(),
                    "A yoetz live-CDP daemon is still running. If you want a clean bundled dev-browser attach retry, run `yoetz browser reset` first.".to_string(),
                    "External chrome-devtools-mcp processes are still running (pid 3333, 3334). If they are not actively in use, close the owning tool/process first; `yoetz browser reset` will not stop them.".to_string(),
                ],
            },
        );
        assert!(report.contains("agent-browser default daemon: stale (pid 1234)"));
        assert!(report.contains("dev-browser daemon: running (1 process)"));
        assert!(report.contains("yoetz live-CDP daemon: running (1 process)"));
        assert!(report.contains("external cdp clients: 2 detected"));
        assert!(report.contains("chrome-devtools-mcp (pid 3333)"));
        assert!(report.contains("chrome-devtools-mcp watchdog (pid 3334)"));
        assert!(report.contains("Run `yoetz browser reset`"));
        assert!(report.contains("will not stop them"));
    }

    #[test]
    fn browser_doctor_report_marks_busy_live_attach_daemon_without_reset_guidance() {
        let report = browser_doctor_report_with_discovery(
            &[],
            &[],
            &[],
            &AutoConnectDoctorStatus::Unavailable("probe failed".to_string()),
            &BrowserDoctorHelpers {
                agent_browser_default: DaemonState::Healthy,
                agent_browser_default_pid: Some(1234),
                live_attach_daemon: crate::live_attach::DaemonSummary {
                    health: crate::live_attach::DaemonHealth::Busy,
                    pid: Some(4444),
                    session_count: 0,
                    endpoint_session_count: 0,
                    target_alias_count: 0,
                    poisoned_count: 0,
                },
                yoetz_live_cdp_processes: vec![],
                dev_browser_processes: vec![],
                external_mcp_processes: vec![],
                recommended_actions: vec!["None required.".to_string()],
            },
        );
        assert!(report.contains(
            "yoetz live-attach daemon: busy (pid 4444, endpoints 0, aliases 0, poisoned 0)"
        ));
        assert!(report.contains("status ping timed out while the owner was busy"));
        assert!(!report.contains("looks stale"));
    }

    #[test]
    fn parse_browser_helper_process_line_classifies_helpers() {
        let dev =
            parse_browser_helper_process_line("10060 node /Users/test/.dev-browser/daemon.mjs")
                .expect("dev-browser helper");
        assert_eq!(dev.pid, 10060);
        assert_eq!(dev.kind, BrowserHelperProcessKind::DevBrowserDaemon);

        let yoetz_live_cdp =
            parse_browser_helper_process_line("10061 node /Users/test/.yoetz/live-cdp-daemon.mjs")
                .expect("yoetz live-CDP helper");
        assert_eq!(
            yoetz_live_cdp.kind,
            BrowserHelperProcessKind::YoetzLiveCdpDaemon
        );

        let mcp = parse_browser_helper_process_line(
            "19608 npm exec chrome-devtools-mcp@latest --autoConnect --slim",
        )
        .expect("mcp helper");
        assert_eq!(mcp.kind, BrowserHelperProcessKind::ChromeDevtoolsMcp);

        let watchdog = parse_browser_helper_process_line(
            "19863 node /path/node_modules/chrome-devtools-mcp/build/src/telemetry/watchdog/main.js --parent-pid=19841",
        )
        .expect("watchdog helper");
        assert_eq!(
            watchdog.kind,
            BrowserHelperProcessKind::ChromeDevtoolsMcpWatchdog
        );

        assert!(parse_browser_helper_process_line(
            "65418 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        )
        .is_none());
    }

    #[test]
    fn find_active_port_file_by_browser_id_ignores_stale_entries() {
        let files = vec![
            DevtoolsActivePortFile {
                path: PathBuf::from("/tmp/stale"),
                ws_endpoint: Some("ws://127.0.0.1:9222/devtools/browser/browser-work".to_string()),
                modified_at: None,
                healthy: false,
            },
            DevtoolsActivePortFile {
                path: PathBuf::from("/tmp/healthy"),
                ws_endpoint: Some("ws://127.0.0.1:9223/devtools/browser/browser-work".to_string()),
                modified_at: None,
                healthy: true,
            },
        ];

        assert_eq!(
            find_active_port_file_by_browser_id(&files, "browser-work")
                .map(|file| file.path.as_path()),
            Some(Path::new("/tmp/healthy"))
        );
        assert_eq!(
            find_stale_active_port_file_by_browser_id(&files, "browser-work")
                .map(|file| file.path.as_path()),
            Some(Path::new("/tmp/stale"))
        );
    }

    #[test]
    fn select_live_attach_profile_tab_prefers_active_matching_tab_for_fresh() {
        let tabs = vec![
            AgentBrowserTab {
                index: 1,
                active: true,
                title: "Inbox (43,617) - personal@example.com - Gmail".to_string(),
                url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
            },
            AgentBrowserTab {
                index: 4,
                active: false,
                title: "Personal Pro OK".to_string(),
                url: "https://chatgpt.com/c/abc".to_string(),
            },
        ];

        let selected = select_live_attach_profile_tab(&tabs, "personal@example.com").unwrap();
        assert_eq!(selected.index, 1);
    }

    #[test]
    fn select_live_attach_profile_tab_errors_when_email_not_visible() {
        let tabs = vec![AgentBrowserTab {
            index: 2,
            active: true,
            title: "Inbox (6,096) - work@example.com - Work Mail".to_string(),
            url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
        }];

        let err = select_live_attach_profile_tab(&tabs, "personal@example.com").unwrap_err();
        assert!(err
            .to_string()
            .contains("profile_email `personal@example.com` was not visible"));
    }

    #[test]
    fn select_live_attach_profile_tab_errors_when_email_is_ambiguous() {
        let tabs = vec![
            AgentBrowserTab {
                index: 1,
                active: true,
                title: "Inbox (43,617) - personal@example.com - Gmail".to_string(),
                url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
            },
            AgentBrowserTab {
                index: 2,
                active: false,
                title: "Drafts - personal@example.com - Gmail".to_string(),
                url: "https://mail.google.com/mail/u/1/#drafts".to_string(),
            },
        ];

        let err = select_live_attach_profile_tab(&tabs, "personal@example.com").unwrap_err();
        assert!(err
            .to_string()
            .contains("matched multiple live auto-connect tabs"));
    }

    #[test]
    fn is_chatgpt_tab_matches_url_or_title() {
        assert!(is_chatgpt_tab(&AgentBrowserTab {
            index: 1,
            active: false,
            title: "Workspace".to_string(),
            url: "https://chatgpt.com/c/abc".to_string(),
        }));
        assert!(is_chatgpt_tab(&AgentBrowserTab {
            index: 2,
            active: false,
            title: "ChatGPT".to_string(),
            url: "https://example.com/".to_string(),
        }));
        assert!(!is_chatgpt_tab(&AgentBrowserTab {
            index: 3,
            active: true,
            title: "Workspace".to_string(),
            url: "https://example.com/".to_string(),
        }));
    }

    #[test]
    fn chatgpt_run_tab_candidates_prioritize_active_chatgpt_tabs() {
        let tabs = vec![
            AgentBrowserTab {
                index: 9,
                active: true,
                title: "Workspace".to_string(),
                url: "https://example.com/".to_string(),
            },
            AgentBrowserTab {
                index: 7,
                active: false,
                title: "ChatGPT".to_string(),
                url: "https://chatgpt.com/c/older".to_string(),
            },
            AgentBrowserTab {
                index: 5,
                active: true,
                title: "ChatGPT".to_string(),
                url: "https://chatgpt.com/c/current".to_string(),
            },
        ];

        let indices = chatgpt_run_tab_candidates(&tabs)
            .into_iter()
            .map(|tab| tab.index)
            .collect::<Vec<_>>();
        assert_eq!(indices, vec![5, 7]);
    }

    #[test]
    fn forget_cdp_target_clears_persisted_auto_selected_source_path() {
        let _guard = lock_env();
        let dir = unique_test_dir("browser_target_state_forget");
        let state_path = dir.join("browser-target.json");
        let _state_env = EnvVarGuard::set("YOETZ_BROWSER_TARGET_PATH", &state_path);

        let target = ResolvedCdpTarget {
            endpoint: "ws://127.0.0.1:9222/devtools/browser/test".to_string(),
            source: ResolvedCdpTargetSource::Auto,
            description: "auto-selected running Chrome target".to_string(),
            source_path: Some(PathBuf::from("/tmp/chrome-default/DevToolsActivePort")),
            selected_target: None,
        };

        remember_cdp_target(&target).unwrap();
        forget_cdp_target(&target).unwrap();
        let loaded = load_browser_target_state().unwrap();
        assert_eq!(loaded.last_source_path, None);
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
                run_id: None,
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
    fn build_agent_browser_args_uses_run_scoped_cdp_session_name() {
        let args = build_agent_browser_args(
            vec!["snapshot".to_string()],
            OutputFormat::Json,
            Some(&BrowserConnection::Cdp {
                endpoint: "http://127.0.0.1:9222".to_string(),
                run_id: Some("run:abc/123".to_string()),
            }),
            /* use_stealth */ true,
            /* headed */ true,
        );

        assert!(args.iter().any(|arg| arg == "yoetz-cdp-run-abc-123"));
        assert!(!args.iter().any(|arg| arg == CDP_SESSION_NAME));
    }

    #[test]
    fn live_cdp_session_name_leaves_agent_browser_length_headroom() {
        let name = live_cdp_session_name_for_run(&"a".repeat(200));
        assert_eq!(name.len(), "yoetz-cdp-".len() + 50);
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
    #[allow(unsafe_code)]
    fn live_attach_chatgpt_steps_focus_run_tab_before_built_in_and_generic_actions() {
        fn assert_action_was_focused(lines: &[&str], needle: &str) {
            let action_idx = lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("expected `{needle}` in log:\n{}", lines.join("\n")));
            assert!(
                lines[..action_idx]
                    .iter()
                    .rev()
                    .any(|line| line.contains(" tab 4")),
                "expected tab 4 selection before `{needle}` in log:\n{}",
                lines.join("\n")
            );
        }

        fn run_case(label: &str, connection: BrowserConnection, expected_connection_arg: &str) {
            let log_dir = unique_test_dir(label);
            let log_path = log_dir.join("agent-browser.log");
            let eval_count_path = log_dir.join("eval-count");
            let bin = fake_agent_browser_focus_bin();
            let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);
            let _log_env = EnvVarGuard::set("LOG_PATH", &log_path);
            let _eval_env = EnvVarGuard::set("EVAL_COUNT_PATH", &eval_count_path);
            let mut ctx = recipe_context();
            ctx.use_stealth = false;
            ctx.vars.insert("model".to_string(), "auto".to_string());
            ctx.vars.insert("prompt".to_string(), "Review".to_string());
            ctx.vars
                .insert("run_id".to_string(), "run-focus".to_string());
            let recipe = serde_yaml_ng::from_str::<Recipe>(
                r##"
name: chatgpt
steps:
  - action: open
    args: ["https://chatgpt.com/?_yoetz={{run_id}}"]
  - action: chatgpt_select_model
  - action: chatgpt_upload_bundle
  - action: type
    args: ["#prompt-textarea", "{{prompt}}"]
  - action: chatgpt_send
"##,
            )
            .unwrap();

            run_recipe_with_connection(recipe, ctx, Some(&connection), OutputFormat::Text).unwrap();

            let logged = fs::read_to_string(&log_path).unwrap_or_default();
            assert!(
                logged.contains(expected_connection_arg),
                "expected `{expected_connection_arg}` in log:\n{logged}"
            );
            let lines = logged.lines().collect::<Vec<_>>();
            let eval_indices = lines
                .iter()
                .enumerate()
                .filter_map(|(idx, line)| {
                    (line.contains(" eval ") && !line.contains("windowName")).then_some(idx)
                })
                .collect::<Vec<_>>();
            assert!(
                eval_indices.len() >= 3,
                "expected model-selection, upload-scope, and send evals:\n{logged}"
            );
            for idx in eval_indices {
                assert!(
                    lines[..idx]
                        .iter()
                        .rev()
                        .any(|line| line.contains(" tab 4")),
                    "expected run-tab focus before eval line `{}`:\n{logged}",
                    lines[idx]
                );
            }
            assert_action_was_focused(
                &lines,
                " upload input[type='file'][title='yoetz-upload-target'] /tmp/bundle.md",
            );
            assert_action_was_focused(&lines, " type #prompt-textarea Review");
        }

        let _guard = lock_env();
        run_case(
            "focus_auto_connect",
            BrowserConnection::AutoConnect,
            "--auto-connect",
        );
        run_case(
            "focus_cdp",
            BrowserConnection::Cdp {
                endpoint: "http://127.0.0.1:9222".to_string(),
                run_id: Some("run-focus".to_string()),
            },
            "--cdp http://127.0.0.1:9222 --session yoetz-cdp-run-focus",
        );
    }

    #[test]
    fn looks_authenticated_detects_chatgpt() {
        // Auth-specific markers (composer, sidebar controls).
        assert!(looks_authenticated(r#"{"text": "New chat"}"#));
        assert!(looks_authenticated(r#"{"text": "Send a message"}"#));
        assert!(looks_authenticated(
            r#"{"ref": "send-button", "text": "Ready"}"#
        ));
        assert!(looks_authenticated(
            r#"{"ref": "prompt-textarea", "text": ""}"#
        ));
        assert!(looks_authenticated(
            r#"{"ref": "model-switcher-dropdown-button", "text": "ChatGPT"}"#
        ));
        assert!(looks_authenticated(
            r#"{"ref": "composer-plus-btn", "text": ""}"#
        ));
        // Branding-only strings should NOT match (logged-out page can have these).
        assert!(!looks_authenticated(r#"{"text": "ChatGPT"}"#));
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
                run_id: None,
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
    fn is_challenge_page_ignores_review_content_inside_authenticated_chat() {
        let snapshot = r#"{
            "url":"https://chatgpt.com/",
            "text":"The review inspects Cloudflare security check handling and the phrase verify you are human.",
            "ref":"prompt-textarea"
        }"#;
        assert!(!is_challenge_page(snapshot));
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
                run_id: None,
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
    fn detect_auth_issue_ignores_challenge_phrases_in_authenticated_thread() {
        let snapshot = r#"{
            "url":"https://chatgpt.com/",
            "text":"I am reviewing code that literally contains verify you are human and security check markers.",
            "ref":"prompt-textarea"
        }"#;
        let issue =
            detect_auth_issue_for_connection(snapshot, Some(&BrowserConnection::AutoConnect));
        assert_eq!(issue, None);
    }

    #[test]
    #[allow(unsafe_code)]
    fn close_browser_for_connection_uses_expected_close_mode() {
        let _guard = lock_env();
        let log_dir = unique_test_dir("close_browser");
        let log_path = log_dir.join("agent-browser.log");
        let bin = fake_agent_browser_bin();
        let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);
        let _log_env = EnvVarGuard::set("LOG_PATH", &log_path);

        let live_cases = [
            BrowserConnection::AutoConnect,
            BrowserConnection::Cdp {
                endpoint: "http://127.0.0.1:9222".to_string(),
                run_id: None,
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
    }

    #[test]
    #[allow(unsafe_code)]
    fn check_auth_with_connection_skips_close_for_live_attach() {
        let _guard = lock_env();
        let log_dir = unique_test_dir("check_auth_live_attach");
        let log_path = log_dir.join("agent-browser.log");
        let tab_state_path = log_dir.join("tab-state");
        let bin = fake_agent_browser_auth_with_tab_list_bin();
        let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);
        let _log_env = EnvVarGuard::set("LOG_PATH", &log_path);
        let _tab_state_env = EnvVarGuard::set("TAB_STATE_PATH", &tab_state_path);

        check_auth_with_connection(&BrowserConnection::AutoConnect, false, CHATGPT_URL).unwrap();

        let logged = fs::read_to_string(&log_path).unwrap_or_default();
        let select_pos = logged
            .find("--auto-connect tab 5")
            .expect("expected verification tab selection before close");
        let close_pos = logged
            .find("--auto-connect tab close")
            .expect("expected verification tab close");
        assert!(
            logged.contains("--auto-connect tab new"),
            "expected live-attach tab open, got `{logged}`"
        );
        assert!(
            logged.contains("--auto-connect tab list --json"),
            "expected live-attach tab discovery, got `{logged}`"
        );
        assert!(
            logged.contains("--auto-connect snapshot -c --json"),
            "expected live-attach snapshot, got `{logged}`"
        );
        assert!(
            logged.contains("timeout=30000"),
            "expected bounded live-attach timeout in logged commands, got `{logged}`"
        );
        assert!(
            select_pos < close_pos,
            "live-attach auth check should select the verification tab before closing it: `{logged}`"
        );
        assert!(
            logged.contains("--auto-connect tab close"),
            "live-attach auth check should close the verification tab: `{logged}`"
        );
        assert!(
            !logged.contains(" close\n") || logged.contains("tab close"),
            "live-attach auth check should not close the daemon/session: `{logged}`"
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_socket_health_detects_responsive_listener() {
        use std::os::unix::net::UnixListener;

        let dir = unique_unix_socket_dir("daemon_health_ok");
        let sock_path = dir.join("default.sock");
        let _listener = UnixListener::bind(&sock_path).unwrap();
        assert!(is_daemon_socket_healthy(&sock_path));
    }

    #[cfg(unix)]
    #[test]
    fn daemon_socket_health_rejects_stale_socket() {
        use std::os::unix::net::UnixListener;

        let dir = unique_unix_socket_dir("daemon_health_stale");
        let sock_path = dir.join("default.sock");
        {
            let _listener = UnixListener::bind(&sock_path).unwrap();
        }
        // Allow the kernel to fully release the socket after the listener drops.
        thread::sleep(Duration::from_millis(50));
        assert!(!is_daemon_socket_healthy(&sock_path));
    }

    #[cfg(unix)]
    #[test]
    fn force_kill_stale_daemon_preserves_recent_live_process() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let dir = unique_unix_socket_dir("daemon_grace");
        let pid_path = dir.join("default.pid");
        let sock_path = dir.join("default.sock");
        {
            let _listener = UnixListener::bind(&sock_path).unwrap();
        }

        let script_path = dir.join("agent-browser");
        fs::write(&script_path, "#!/bin/sh\nsleep 30\n").unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&script_path, perms).unwrap();

        let mut child = Command::new("/bin/sh").arg(&script_path).spawn().unwrap();
        fs::write(&pid_path, child.id().to_string()).unwrap();

        let action =
            force_kill_stale_daemon_with_paths(&pid_path, &sock_path, Duration::from_secs(30));
        assert_eq!(action, DaemonRecoveryAction::AwaitingApproval);
        assert!(process_is_alive(child.id()));

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn force_kill_stale_daemon_refuses_unverified_pid() {
        use std::os::unix::net::UnixListener;

        let dir = unique_unix_socket_dir("daemon_refuse_unverified");
        let pid_path = dir.join("default.pid");
        let sock_path = dir.join("default.sock");
        {
            let _listener = UnixListener::bind(&sock_path).unwrap();
        }
        thread::sleep(Duration::from_millis(50));

        let mut child = Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap();
        fs::write(&pid_path, child.id().to_string()).unwrap();

        let action =
            force_kill_stale_daemon_with_paths(&pid_path, &sock_path, Duration::from_secs(0));
        assert_eq!(action, DaemonRecoveryAction::KilledStale);
        assert!(process_is_alive(child.id()));
        assert!(!pid_path.exists());
        assert!(!sock_path.exists());

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    #[allow(unsafe_code)]
    fn try_auto_connect_verifies_auth_even_with_healthy_daemon() {
        use std::os::unix::net::UnixListener;

        let _guard = lock_env();
        let home_dir = unique_unix_socket_dir("auto_connect_home");
        let ab_dir = home_dir.join(".agent-browser");
        fs::create_dir_all(&ab_dir).unwrap();
        let sock_path = ab_dir.join("default.sock");
        let _listener = UnixListener::bind(&sock_path).unwrap();

        let log_dir = unique_test_dir("auto_connect_log");
        let log_path = log_dir.join("agent-browser.log");
        let bin = fake_agent_browser_auth_bin();
        let _home_env = EnvVarGuard::set("HOME", &home_dir);
        let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);
        let _log_env = EnvVarGuard::set("LOG_PATH", &log_path);

        try_auto_connect(CHATGPT_URL).unwrap();

        let logged = fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            logged.contains("--auto-connect tab new"),
            "healthy daemon should still verify the target page, got `{logged}`"
        );
        assert!(
            logged.contains("--auto-connect snapshot -c --json"),
            "healthy daemon should still verify auth state, got `{logged}`"
        );
    }

    #[test]
    #[allow(unsafe_code)]
    fn try_auto_connect_surfaces_allow_dialog_timeout() {
        let _guard = lock_env();
        let bin = fake_agent_browser_timeout_bin();
        let home_dir = unique_test_dir("auto_connect_timeout_home");
        let _home_env = EnvVarGuard::set("HOME", &home_dir);
        let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);

        let err = try_auto_connect(CHATGPT_URL).unwrap_err();
        assert!(allow_dialog_error(&err), "unexpected error: {err}");
    }

    #[test]
    #[allow(unsafe_code)]
    fn try_cdp_attach_surfaces_allow_dialog_timeout() {
        let _guard = lock_env();
        let bin = fake_agent_browser_timeout_bin();
        let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);

        let err = try_cdp_attach("http://127.0.0.1:9222", CHATGPT_URL).unwrap_err();
        assert!(allow_dialog_error(&err), "unexpected error: {err}");
        assert!(is_chrome_approval_wait_error(&err));
    }

    #[test]
    fn should_attach_chrome136_warning_only_for_transport_failures() {
        let auth_issue = anyhow!(
            "chatgpt login required in the attached Chrome session. Log in there and try again."
        );
        let connect_failure =
            anyhow!("requesting `http://127.0.0.1:9222/json/version` failed: connection refused");

        assert!(!should_attach_chrome136_warning(
            "http://127.0.0.1:9222",
            &auth_issue
        ));
        assert!(should_attach_chrome136_warning(
            "http://127.0.0.1:9222",
            &connect_failure
        ));
    }

    #[test]
    fn is_chrome_approval_wait_error_rejects_non_approval_timeouts() {
        let err = anyhow!("ChatGPT response timed out after 900000ms");
        assert!(!is_chrome_approval_wait_error(&err));
    }

    #[test]
    fn is_chrome_approval_wait_error_matches_live_cdp_consent_phrase() {
        let err = anyhow!(
            "Timed out after 5000ms initializing live CDP browser during Target.getTargets. \
             Chrome may be waiting for remote-debugging consent or a target may be unresponsive."
        );
        assert!(is_chrome_approval_wait_error(&err));
        assert!(allow_dialog_error(&err));
    }

    #[test]
    fn is_chrome_approval_wait_error_matches_consent_space_variant() {
        let err = anyhow!("Chrome is waiting for remote debugging consent");
        assert!(is_chrome_approval_wait_error(&err));
        assert!(allow_dialog_error(&err));
    }

    #[test]
    fn allow_dialog_error_is_case_insensitive_and_reads_full_chain() {
        let err = anyhow!("waiting for Allow Remote Debugging consent dialog")
            .context("connecting to Chrome");
        assert!(
            allow_dialog_error(&err),
            "mixed-case dialog phrase in chain should still match"
        );

        let not_approval = anyhow!("connection refused");
        assert!(!allow_dialog_error(&not_approval));
    }

    #[test]
    fn is_chrome_cdp_unreachable_error_detects_wrapped_hint() {
        let err =
            anyhow!("requesting `http://127.0.0.1:9222/json/version` failed: connection refused")
                .context(
                    "chrome-devtools-mcp could not reach Chrome's CDP endpoint. \
                 Chrome 136+ ignores --remote-debugging-port on the default profile — \
                 either enable chrome://inspect/#remote-debugging (Chrome 144+) and retry, \
                 or pass --cdp=ws://127.0.0.1:PORT after launching Chrome with a non-default \
                 --user-data-dir, or use Chrome for Testing",
                );
        assert!(is_chrome_cdp_unreachable_error(&err));
    }

    #[test]
    fn is_chrome_cdp_unreachable_error_rejects_unrelated_errors() {
        let err = anyhow!("ChatGPT response timed out after 900000ms");
        assert!(!is_chrome_cdp_unreachable_error(&err));
    }

    #[test]
    fn is_chrome_cdp_unreachable_error_requires_a_real_cdp_failure_shape() {
        let err = anyhow!("read the docs at chrome://inspect/#remote-debugging before retrying");
        assert!(!is_chrome_cdp_unreachable_error(&err));
    }

    #[test]
    fn is_chatgpt_auth_issue_error_detects_login_and_challenge_errors() {
        let login = anyhow!(
            "chatgpt login required in the attached Chrome session. Log in there and try again."
        );
        let challenge = anyhow!(
            "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again."
        );
        let captcha = anyhow!(
            "captcha detected in the attached Chrome session, but stdin is not interactive."
        );
        let other = anyhow!("browserType.connectOverCDP: failed to list pages");

        assert!(is_chatgpt_auth_issue_error(&login));
        assert!(is_chatgpt_auth_issue_error(&challenge));
        assert!(is_chatgpt_auth_issue_error(&captcha));
        assert!(!is_chatgpt_auth_issue_error(&other));
    }

    #[test]
    fn is_chatgpt_attached_page_error_detects_marker_and_model_mismatch() {
        let tagged = mark_chatgpt_attached_page_error(anyhow!("composer interaction failed"));
        let mismatch = anyhow!(
            "requested ChatGPT model `pro` was not actually selected. Current page: url=https://chatgpt.com/, title=\"ChatGPT\""
        );
        let other = anyhow!("browserType.connectOverCDP: failed to list pages");

        assert!(is_chatgpt_attached_page_error(&tagged));
        assert!(is_chatgpt_attached_page_error(&mismatch));
        assert!(!is_chatgpt_attached_page_error(&other));
    }

    #[test]
    fn is_chatgpt_profile_selector_visibility_error_detects_context_mismatch() {
        let err = anyhow!(
            "profile_email `work@example.com` was not visible in the live auto-connect tab list. Visible emails: personal@example.com"
        );
        let other = anyhow!("requested ChatGPT model `pro` was not actually selected");

        assert!(is_chatgpt_profile_selector_visibility_error(&err));
        assert!(!is_chatgpt_profile_selector_visibility_error(&other));
    }

    #[test]
    fn should_stop_live_attach_fallback_on_approval_wait_and_create_target_block() {
        let approval = anyhow!(
            "live browser attach timed out (30s). Chrome may be showing an \"Allow remote debugging?\" dialog — please click Allow in Chrome, then retry."
        );
        let create_target_block = anyhow!(
            "creating a new Chrome page for `about:blank` failed: Chrome's default-profile CDP endpoint likely rejected external `Target.createTarget` while opening `about:blank`. Chrome 146+/147 can allow attach/read operations but close the session on new-tab creation for untrusted clients. First, open chrome://inspect/#remote-debugging, refresh Discover network targets (or Open dedicated DevTools for Node), and retry. If Chrome still closes the session, launch Chrome with `--remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug` and pass `--cdp`, or use Chrome for Testing. Unable to make method calls because underlying connection is closed"
        );
        let auth_issue = anyhow!(
            "chatgpt login required in the attached Chrome session. Log in there and try again."
        );

        assert!(should_stop_live_attach_fallback(&approval));
        assert!(should_stop_live_attach_fallback(&create_target_block));
        assert!(!should_stop_live_attach_fallback(&auth_issue));
    }

    #[test]
    #[allow(unsafe_code)]
    fn sync_cookies_errors_on_invalid_json_output() {
        let _guard = lock_env();
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

        let _path_env = EnvVarGuard::set("PATH", &new_path);
        let _scripts_env = EnvVarGuard::set("YOETZ_SCRIPTS_DIR", &scripts_dir);

        let profile_dir = dir.join("profile");
        let err = sync_cookies(&profile_dir).unwrap_err();
        assert!(err.to_string().contains("invalid JSON"));
    }

    #[test]
    fn parse_chatgpt_poll_args_defaults() {
        let options = parse_chatgpt_poll_args(CHATGPT_WAIT_ACTION, None).unwrap();
        assert_eq!(options.interval_ms, CHATGPT_POLL_INTERVAL_MS_DEFAULT);
        assert_eq!(options.timeout_ms, CHATGPT_POLL_TOTAL_TIMEOUT_MS_DEFAULT);
        // attempts derived from ceil(timeout / interval) when not explicit.
        let expected_attempts = CHATGPT_POLL_TOTAL_TIMEOUT_MS_DEFAULT
            .div_ceil(CHATGPT_POLL_INTERVAL_MS_DEFAULT) as usize;
        assert_eq!(options.attempts, expected_attempts);
    }

    #[test]
    fn parse_chatgpt_poll_args_derives_attempts_from_timeout() {
        let args = vec![
            "--timeout-ms".to_string(),
            "600000".to_string(),
            "--interval-ms".to_string(),
            "10000".to_string(),
        ];
        let options = parse_chatgpt_poll_args(CHATGPT_WAIT_ACTION, Some(&args)).unwrap();
        // ceil(600000 / 10000) = 60
        assert_eq!(options.attempts, 60);
    }

    #[test]
    fn parse_chatgpt_poll_args_explicit_attempts_not_overridden() {
        let args = vec![
            "--attempts".to_string(),
            "5".to_string(),
            "--timeout-ms".to_string(),
            "600000".to_string(),
            "--interval-ms".to_string(),
            "10000".to_string(),
        ];
        let options = parse_chatgpt_poll_args(CHATGPT_WAIT_ACTION, Some(&args)).unwrap();
        // Explicit --attempts should be preserved, not derived.
        assert_eq!(options.attempts, 5);
    }

    #[test]
    fn parse_chatgpt_poll_args_custom_values() {
        let args = vec![
            "--attempts".to_string(),
            "12".to_string(),
            "--interval-ms".to_string(),
            "9000".to_string(),
            "--timeout-ms".to_string(),
            "42000".to_string(),
        ];
        let options = parse_chatgpt_poll_args(CHATGPT_WAIT_ACTION, Some(&args)).unwrap();
        assert_eq!(
            options,
            ChatgptPollOptions {
                attempts: 12,
                interval_ms: 9000,
                timeout_ms: 42000,
            }
        );
    }

    #[test]
    fn parse_chatgpt_poll_args_rejects_unknown_flag() {
        let args = vec!["--nope".to_string()];
        let err = parse_chatgpt_poll_args(CHATGPT_WAIT_ACTION, Some(&args)).unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[test]
    fn parse_upload_poll_options_labels_unknown_args_as_upload_wait() {
        let args = vec!["--nope".to_string()];
        let err = parse_upload_poll_options(Some(&args)).unwrap_err();
        assert!(err.to_string().contains(CHATGPT_WAIT_UPLOAD_ACTION));
        assert!(!err.to_string().contains(CHATGPT_WAIT_ACTION));
    }

    #[test]
    fn parse_chatgpt_poll_args_rejects_zero_timeout() {
        let args = vec!["--timeout-ms".to_string(), "0".to_string()];
        let err = parse_chatgpt_poll_args(CHATGPT_WAIT_ACTION, Some(&args)).unwrap_err();
        assert!(err.to_string().contains("--timeout-ms"));
    }

    #[test]
    fn chatgpt_recipe_payload_extracts_response_model_and_warnings() {
        let steps = vec![
            json!({
                "type": "browser_step",
                "action": CHATGPT_SELECT_MODEL_ACTION,
                "stdout": {
                    "status": "ok",
                    "model_used": "gpt-5-4-pro",
                    "model_selection_status": "selected"
                }
            }),
            json!({
                "type": "browser_step",
                "action": CHATGPT_WAIT_ACTION,
                "stdout": {
                    "status": "ok",
                    "response": "final answer",
                    "warnings": ["used paste fallback"]
                }
            }),
        ];

        let payload = chatgpt_recipe_payload_from_steps(&steps, true);
        assert_eq!(payload["transport"], "agent-browser");
        assert_eq!(payload["backend"], "agent-browser");
        assert_eq!(payload["response"], "final answer");
        assert_eq!(payload["model_used"], "gpt-5-4-pro");
        assert_eq!(payload["model_selection_status"], "selected");
        assert_eq!(payload["fallback_used"], true);
        assert_eq!(payload["warnings"], json!(["used paste fallback"]));
        assert_eq!(payload["delivery_mode"], "file_upload");
        assert_eq!(payload["auto_paste_fallback"], false);
        assert_eq!(payload["steps"], json!(steps));

        let primary_payload = chatgpt_recipe_payload_from_steps(&steps, false);
        assert_eq!(primary_payload["fallback_used"], false);
    }

    fn dom(send: ChatgptSendState, stop: bool, thinking: bool, copy: usize) -> ChatgptDomState {
        ChatgptDomState {
            send_state: send,
            has_stop_button: stop,
            has_thinking_indicator: thinking,
            copy_button_count: copy,
            assistant_msg_count: 1,
            assistant_last_len: 100,
            error: String::new(),
        }
    }

    fn baseline_dom() -> ChatgptDomState {
        ChatgptDomState {
            send_state: ChatgptSendState::Missing,
            has_stop_button: false,
            has_thinking_indicator: false,
            copy_button_count: 0,
            assistant_msg_count: 0,
            assistant_last_len: 0,
            error: String::new(),
        }
    }

    #[test]
    fn parse_chatgpt_dom_state_parses_eval_output() {
        let state =
            parse_chatgpt_dom_state("send=enabled|stop=0|thinking=0|copy=2|msgs=1|lastlen=50|err=")
                .unwrap();
        assert_eq!(state.send_state, ChatgptSendState::Enabled);
        assert!(!state.has_stop_button);
        assert!(!state.has_thinking_indicator);
        assert_eq!(state.copy_button_count, 2);
        assert_eq!(state.assistant_msg_count, 1);
        assert_eq!(state.assistant_last_len, 50);
        assert!(state.error.is_empty());
    }

    #[test]
    fn parse_chatgpt_dom_state_parses_thinking_indicator() {
        let state =
            parse_chatgpt_dom_state("send=disabled|stop=1|thinking=1|copy=0|msgs=0|lastlen=0|err=")
                .unwrap();
        assert!(state.has_thinking_indicator);
        assert!(state.has_stop_button);
    }

    #[test]
    fn parse_chatgpt_dom_state_parses_error_field() {
        let state = parse_chatgpt_dom_state(
            "send=enabled|stop=0|thinking=0|copy=0|msgs=0|lastlen=0|err=network error",
        )
        .unwrap();
        assert_eq!(state.error, "network error");
    }

    #[test]
    fn parse_chatgpt_dom_state_defaults_thinking_when_absent() {
        let state = parse_chatgpt_dom_state("send=enabled|stop=0|copy=2").unwrap();
        assert!(!state.has_thinking_indicator);
    }

    #[test]
    fn parse_chatgpt_dom_state_rejects_quoted_output() {
        let err = parse_chatgpt_dom_state(r#""send=enabled|stop=0|copy=1""#).unwrap_err();
        assert!(
            err.to_string().contains("send state")
                || err.to_string().contains("copy count")
                || err.to_string().contains("invalid"),
            "expected parse error for quoted output, got: {err}"
        );
    }

    #[test]
    fn parse_stdout_json_unwraps_stringified_json_payloads() {
        let parsed =
            parse_stdout_json(r#""{\"status\":\"already-selected\",\"modelUsed\":\"Pro\"}""#)
                .expect("stringified json payload");
        assert_eq!(parsed["status"], "already-selected");
        assert_eq!(parsed["modelUsed"], "Pro");
    }

    #[test]
    fn parse_stdout_json_unwraps_nested_json_strings() {
        let payload = "send=missing|stop=0|thinking=0|copy=1|msgs=1|lastlen=5|err=";
        let nested = serde_json::to_string(&serde_json::to_string(payload).unwrap()).unwrap();
        let parsed = parse_stdout_json(&nested).expect("nested json string");
        assert_eq!(parsed, Value::String(payload.into()));
    }

    #[test]
    fn parse_stdout_json_keeps_plain_string_scalars_as_strings() {
        let parsed = parse_stdout_json(r#""1""#).expect("string scalar");
        assert_eq!(parsed, Value::String("1".into()));
    }

    #[test]
    fn parse_chatgpt_send_baseline_from_stdout_extracts_counts() {
        let baseline = parse_chatgpt_send_baseline_from_stdout(
            r#"{"status":"ok","assistantCountBeforeSend":1,"assistantLastLenBeforeSend":42}"#,
        )
        .expect("baseline");
        assert_eq!(
            baseline,
            ChatgptResponseBaseline {
                assistant_msg_count: 1,
                assistant_last_len: 42,
            }
        );
    }

    #[test]
    fn chatgpt_wait_probe_delay_is_immediate_on_first_attempt() {
        assert_eq!(chatgpt_wait_probe_delay(1, 30_000, 30_000), None);
        assert_eq!(
            chatgpt_wait_probe_delay(2, 30_000, 30_000),
            Some(Duration::from_millis(30_000))
        );
        assert_eq!(
            chatgpt_wait_probe_delay(3, 30_000, 5_000),
            Some(Duration::from_millis(5_000))
        );
    }

    #[test]
    fn classify_chatgpt_completion_distinguishes_generating_idle_copybutton() {
        let bl = baseline_dom();

        // No copy button + new message + non-empty text → Idle (caller must
        // verify length stability across the threshold window before declaring
        // completion). Critically, this is NOT immediate completion — the
        // preamble-capture bug came from treating this case as instantly done.
        let idle_no_copy = dom(ChatgptSendState::Enabled, false, false, 0);
        assert_eq!(
            classify_chatgpt_completion(&idle_no_copy, &bl),
            CompletionVerdict::Idle
        );

        // No new message (same count as baseline) → Generating.
        let no_new = ChatgptDomState {
            assistant_msg_count: 0,
            ..idle_no_copy.clone()
        };
        assert_eq!(
            classify_chatgpt_completion(&no_new, &bl),
            CompletionVerdict::Generating
        );

        // Empty response (new msg but lastlen=0, no copy button) → Generating.
        let empty = ChatgptDomState {
            assistant_last_len: 0,
            ..idle_no_copy.clone()
        };
        assert_eq!(
            classify_chatgpt_completion(&empty, &bl),
            CompletionVerdict::Generating
        );

        // Disabled send (composer locked while sending) → Generating.
        let disabled = dom(ChatgptSendState::Disabled, false, false, 0);
        assert_eq!(
            classify_chatgpt_completion(&disabled, &bl),
            CompletionVerdict::Generating
        );

        // Missing send (voice button replaces send post-completion) → Idle.
        let missing = dom(ChatgptSendState::Missing, false, false, 0);
        assert_eq!(
            classify_chatgpt_completion(&missing, &bl),
            CompletionVerdict::Idle
        );

        // Stop button present → Generating regardless of other signals.
        let generating = dom(ChatgptSendState::Enabled, true, false, 0);
        assert_eq!(
            classify_chatgpt_completion(&generating, &bl),
            CompletionVerdict::Generating
        );

        // Thinking indicator → Generating.
        let thinking = dom(ChatgptSendState::Enabled, false, true, 0);
        assert_eq!(
            classify_chatgpt_completion(&thinking, &bl),
            CompletionVerdict::Generating
        );

        // Strong gate: copy button on a NEW assistant message → CopyButton.
        let copy_on_new = ChatgptDomState {
            copy_button_count: 1,
            ..idle_no_copy.clone()
        };
        assert_eq!(
            classify_chatgpt_completion(&copy_on_new, &bl),
            CompletionVerdict::CopyButton
        );
    }

    #[test]
    fn classify_chatgpt_completion_scopes_copy_button_to_new_message() {
        // Tab is being reused: baseline already has an assistant message with
        // its own copy button. Without msg_count growth, that stale copy
        // button must NOT be treated as completion of the new turn.
        let baseline = ChatgptDomState {
            send_state: ChatgptSendState::Missing,
            has_stop_button: false,
            has_thinking_indicator: false,
            copy_button_count: 1,
            assistant_msg_count: 1,
            assistant_last_len: 80,
            error: String::new(),
        };
        let stale_latest = ChatgptDomState {
            copy_button_count: 1,
            assistant_msg_count: 1,
            assistant_last_len: 80,
            ..dom(ChatgptSendState::Enabled, false, false, 0)
        };
        let new_latest = ChatgptDomState {
            copy_button_count: 1,
            assistant_msg_count: 2,
            assistant_last_len: 0,
            ..dom(ChatgptSendState::Enabled, false, false, 0)
        };

        // Same msg_count as baseline + same length → no progress at all.
        assert_eq!(
            classify_chatgpt_completion(&stale_latest, &baseline),
            CompletionVerdict::Generating
        );
        // Genuinely new message with copy button → CopyButton even with
        // assistant_last_len=0 (the copy button itself proves completion).
        assert_eq!(
            classify_chatgpt_completion(&new_latest, &baseline),
            CompletionVerdict::CopyButton
        );
    }

    #[test]
    fn classify_chatgpt_completion_handles_same_message_growth() {
        // Reused thread: baseline includes a non-empty assistant message
        // (e.g., a previous turn). The new turn has not yet incremented
        // msg_count but the latest message has grown — that is still progress
        // and should classify as Idle (caller verifies stability).
        let baseline = ChatgptDomState {
            send_state: ChatgptSendState::Missing,
            has_stop_button: false,
            has_thinking_indicator: false,
            copy_button_count: 1,
            assistant_msg_count: 1,
            assistant_last_len: 80,
            error: String::new(),
        };
        let grew = ChatgptDomState {
            copy_button_count: 0,
            assistant_msg_count: 1,
            assistant_last_len: 200,
            ..dom(ChatgptSendState::Enabled, false, false, 0)
        };
        assert_eq!(
            classify_chatgpt_completion(&grew, &baseline),
            CompletionVerdict::Idle
        );
    }

    #[test]
    fn chatgpt_stable_idle_threshold_floors_and_scales() {
        // Very short interval → floor at 90s.
        assert_eq!(chatgpt_stable_idle_threshold_ms(1_000), 90_000);
        assert_eq!(chatgpt_stable_idle_threshold_ms(10_000), 90_000);
        // Default 30s interval → still 90s (== 3 × 30s == floor).
        assert_eq!(chatgpt_stable_idle_threshold_ms(30_000), 90_000);
        // Long interval → scales up (3 × interval).
        assert_eq!(chatgpt_stable_idle_threshold_ms(60_000), 180_000);
        assert_eq!(chatgpt_stable_idle_threshold_ms(120_000), 360_000);
    }

    #[test]
    fn chatgpt_send_state_str_covers_all_variants() {
        assert_eq!(chatgpt_send_state_str(ChatgptSendState::Enabled), "enabled");
        assert_eq!(
            chatgpt_send_state_str(ChatgptSendState::Disabled),
            "disabled"
        );
        assert_eq!(chatgpt_send_state_str(ChatgptSendState::Missing), "missing");
    }

    #[test]
    fn recipe_yaml_rejects_unknown_keys() {
        let top_level_err = serde_yaml_ng::from_str::<Recipe>(
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

        let step_err = serde_yaml_ng::from_str::<Recipe>(
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
    fn recipe_yaml_parses_transport_order() {
        let recipe = serde_yaml_ng::from_str::<Recipe>(
            r#"
name: chatgpt
transports: [dev-browser, agent-browser, chrome-devtools-mcp, chrome-extension-native, manual]
steps:
  - action: open
    args: ["https://chatgpt.com/"]
"#,
        )
        .unwrap();

        assert_eq!(
            recipe.transports,
            Some(vec![
                RecipeTransport::DevBrowser,
                RecipeTransport::AgentBrowser,
                RecipeTransport::ChromeDevtoolsMcp,
                RecipeTransport::ChromeExtensionNative,
                RecipeTransport::Manual,
            ])
        );
    }

    #[test]
    fn recipe_transports_default_to_chatgpt_funnel() {
        let recipe = serde_yaml_ng::from_str::<Recipe>(
            r#"
name: chatgpt
steps:
  - action: open
    args: ["https://chatgpt.com/"]
"#,
        )
        .unwrap();

        // Chrome 147+ waterfall: chrome-devtools-mcp is the primary tier,
        // then dev-browser for Chrome ≤ 146 / Chrome for Testing, then
        // agent-browser for cookie/profile managed flows, then manual.
        assert_eq!(
            recipe_transports(&recipe, true),
            vec![
                RecipeTransport::ChromeDevtoolsMcp,
                RecipeTransport::DevBrowser,
                RecipeTransport::AgentBrowser,
                RecipeTransport::Manual,
            ]
        );
        assert_eq!(
            recipe_transports(&recipe, false),
            vec![RecipeTransport::AgentBrowser]
        );
    }

    #[test]
    fn maybe_prefer_extension_native_prepends_for_chatgpt_when_connected() {
        let base = vec![
            RecipeTransport::ChromeDevtoolsMcp,
            RecipeTransport::DevBrowser,
            RecipeTransport::AgentBrowser,
            RecipeTransport::Manual,
        ];
        let promoted = maybe_prefer_extension_native_for_chatgpt(base, true, false, true);
        assert_eq!(
            promoted,
            vec![
                RecipeTransport::ChromeExtensionNative,
                RecipeTransport::ChromeDevtoolsMcp,
                RecipeTransport::DevBrowser,
                RecipeTransport::AgentBrowser,
                RecipeTransport::Manual,
            ]
        );
    }

    #[test]
    fn maybe_prefer_extension_native_noop_when_extension_disconnected() {
        let base = vec![
            RecipeTransport::ChromeDevtoolsMcp,
            RecipeTransport::DevBrowser,
        ];
        let result = maybe_prefer_extension_native_for_chatgpt(base.clone(), true, false, false);
        assert_eq!(result, base);
    }

    #[test]
    fn maybe_prefer_extension_native_noop_when_recipe_pinned_transports() {
        let base = vec![RecipeTransport::ChromeDevtoolsMcp, RecipeTransport::Manual];
        let result = maybe_prefer_extension_native_for_chatgpt(base.clone(), true, true, true);
        assert_eq!(result, base);
    }

    #[test]
    fn maybe_prefer_extension_native_noop_for_non_chatgpt() {
        let base = vec![RecipeTransport::AgentBrowser];
        let result = maybe_prefer_extension_native_for_chatgpt(base.clone(), false, false, true);
        assert_eq!(result, base);
    }

    #[test]
    fn maybe_prefer_extension_native_noop_when_already_present() {
        let base = vec![
            RecipeTransport::ChromeDevtoolsMcp,
            RecipeTransport::ChromeExtensionNative,
            RecipeTransport::Manual,
        ];
        let result = maybe_prefer_extension_native_for_chatgpt(base.clone(), true, false, true);
        assert_eq!(result, base);
    }

    #[test]
    fn recipe_step_parses_timeout_ms() {
        let recipe = serde_yaml_ng::from_str::<Recipe>(
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
    fn recipe_step_with_sleep_and_action_runs_after_sleep() {
        let _guard = lock_env();
        let log_dir = unique_test_dir("sleep_then_action");
        let log_path = log_dir.join("agent-browser.log");
        let bin = fake_agent_browser_bin();
        let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);
        let _log_env = EnvVarGuard::set("LOG_PATH", &log_path);
        let recipe = serde_yaml_ng::from_str::<Recipe>(
            r#"
name: noop
steps:
  - sleep_ms: 0
    action: open
    args: ["https://chatgpt.com/"]
"#,
        )
        .unwrap();
        run_recipe_with_connection(
            recipe,
            recipe_context(),
            Some(&BrowserConnection::AutoConnect),
            OutputFormat::Text,
        )
        .unwrap();

        let logged = fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            logged.contains("open https://chatgpt.com/"),
            "expected the action to run after sleep, got `{logged}`"
        );
    }

    #[test]
    fn chatgpt_recipe_uses_built_in_model_selection_action() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../recipes/chatgpt.yaml");
        let content = fs::read_to_string(&path).expect("read recipes/chatgpt.yaml");

        assert!(content.contains("- action: chatgpt_select_model"));
        assert!(content.contains("- action: chatgpt_open_attachment_ui"));
        assert!(content.contains("- action: chatgpt_send"));
        assert!(!content.contains("const fallbackLabels = ['instant', 'thinking', 'pro'"));
        assert!(!content.contains("send button disabled — page is likely recoverable"));
    }

    #[test]
    fn resolve_recipe_ignores_same_name_cwd_directory_for_bare_builtin_name() {
        let _guard = lock_env();
        let cwd = unique_test_dir("recipe_shadow_cwd");
        fs::create_dir(cwd.join("chatgpt")).unwrap();
        let _cwd = CurrentDirGuard::enter(&cwd);

        let resolved = resolve_recipe(Path::new("chatgpt")).unwrap();

        assert_eq!(resolved.file_name().unwrap(), "chatgpt.yaml");
        assert!(resolved.is_file());
        assert_ne!(resolved, cwd.join("chatgpt"));
    }

    #[test]
    fn resolve_recipe_ignores_same_name_cwd_file_for_bare_builtin_name() {
        let _guard = lock_env();
        let cwd = unique_test_dir("recipe_shadow_file_cwd");
        fs::write(cwd.join("chatgpt"), "not: a built-in recipe").unwrap();
        let _cwd = CurrentDirGuard::enter(&cwd);

        let resolved = resolve_recipe(Path::new("chatgpt")).unwrap();

        assert_eq!(resolved.file_name().unwrap(), "chatgpt.yaml");
        assert!(resolved.is_file());
        assert_ne!(resolved, cwd.join("chatgpt"));
    }

    #[test]
    #[allow(unsafe_code)]
    fn recipe_step_errors_fail_the_recipe() {
        let _guard = lock_env();
        let bin = fake_agent_browser_timeout_bin();
        let _bin_env = EnvVarGuard::set("YOETZ_AGENT_BROWSER_BIN", &bin);
        let recipe = serde_yaml_ng::from_str::<Recipe>(
            r#"
name: fail-fast
steps:
  - action: eval
    args: ["(() => { throw new Error('boom'); })()"]
"#,
        )
        .unwrap();
        let connection = BrowserConnection::AutoConnect;
        let err = run_recipe_with_connection(
            recipe,
            recipe_context(),
            Some(&connection),
            OutputFormat::Text,
        )
        .unwrap_err();

        assert!(err.to_string().contains("recipe step 0 (eval) failed"));
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

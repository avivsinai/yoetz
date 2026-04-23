//! dev-browser integration layer.
//!
//! `dev-browser` is a Playwright-based browser automation CLI that runs
//! JavaScript scripts in a QuickJS sandbox with a pre-connected `browser`
//! global. This module provides the primary browser backend for yoetz,
//! replacing the older `agent-browser` approach with a more capable and
//! reliable Playwright-based API.
//!
//! Key advantages over agent-browser:
//! - Full Playwright Page API (goto, click, fill, locator, evaluate, etc.)
//! - File upload via host-level sandbox helper backed by Node Playwright
//! - Persistent named pages across script runs
//! - Daemon-managed browser instances with auto-reconnect
//! - Single script executes batch operations (fewer IPC round-trips)

use anyhow::{anyhow, Context, Result};
use reqwest::Url;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
#[cfg(test)]
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(test)]
use yoetz_core::paths::home_dir;

use crate::{browser, chatgpt_web};

/// Cached dev-browser resolution.
static DEV_BROWSER: OnceLock<String> = OnceLock::new();
const DEV_BROWSER_INSTALL_GUIDANCE: &str = concat!(
    "dev-browser not found in PATH or npm global prefix. Install it explicitly ",
    "using a pinned, vetted binary/package, or set YOETZ_DEV_BROWSER_BIN to the ",
    "exact executable to run."
);

/// Default timeout for dev-browser scripts in seconds.
const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 30;
const DEV_BROWSER_ATTACH_TO_OTHER_ENV: &str = "PW_CHROMIUM_ATTACH_TO_OTHER";
const DEV_BROWSER_PARENT_TIMEOUT_GRACE_SECS: u64 = 20;
const DEV_BROWSER_WAIT_POLL_MS: u64 = 100;

/// Extended timeout for ChatGPT response polling (30 minutes by default).
const CHATGPT_POLL_TIMEOUT_MS_DEFAULT: u64 = 1_800_000;
const CHATGPT_POLL_INTERVAL_MS_DEFAULT: u64 = 30_000;
const CHATGPT_BROWSER_NAME: &str = "yoetz-chatgpt";
const CHATGPT_AUTH_PROBE_PAGE_NAME: &str = "yoetz-chatgpt-main";
const CHATGPT_RECIPE_PAGE_NAME_PREFIX: &str = "yoetz-chatgpt-run";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatgptPollSettings {
    pub timeout_ms: u64,
    pub interval_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatgptRecipeRunResult {
    pub response: String,
    pub model_used: Option<String>,
    pub warnings: Vec<String>,
}

impl Default for ChatgptPollSettings {
    fn default() -> Self {
        Self {
            timeout_ms: CHATGPT_POLL_TIMEOUT_MS_DEFAULT,
            interval_ms: CHATGPT_POLL_INTERVAL_MS_DEFAULT,
        }
    }
}

/// dev-browser tmp directory for file staging.
#[cfg(test)]
#[allow(dead_code)]
fn dev_browser_tmp_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".dev-browser")
        .join("tmp")
}

fn command_is_available(bin: &str) -> bool {
    // Treat "process could be spawned at all" as availability. Some
    // dev-browser builds print help and exit non-zero, and we do not want that
    // to trigger a pointless npm reinstall over an existing binary.
    Command::new(bin)
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok()
}

fn configured_dev_browser_bin() -> Result<Option<String>> {
    let Some(bin) = env::var_os("YOETZ_DEV_BROWSER_BIN") else {
        return Ok(None);
    };
    let bin = bin.to_string_lossy().to_string();
    if command_is_available(&bin) {
        return Ok(Some(bin));
    }
    Err(anyhow!(
        "YOETZ_DEV_BROWSER_BIN points to `{bin}`, but it is not executable"
    ))
}

fn find_dev_browser() -> Result<Option<String>> {
    if let Some(bin) = DEV_BROWSER.get() {
        return Ok(Some(bin.clone()));
    }
    if let Some(bin) = configured_dev_browser_bin()? {
        let _ = DEV_BROWSER.set(bin.clone());
        return Ok(Some(bin));
    }
    if command_is_available("dev-browser") {
        let bin = "dev-browser".to_string();
        let _ = DEV_BROWSER.set(bin.clone());
        return Ok(Some(bin));
    }
    // npm global bin may not be in PATH (e.g. Homebrew node on macOS).
    // Check `npm prefix -g`/bin/ as a fallback.
    if let Some(bin) = find_dev_browser_via_npm_prefix() {
        let _ = DEV_BROWSER.set(bin.clone());
        return Ok(Some(bin));
    }
    Ok(None)
}

/// Locate dev-browser under the npm global prefix directory.
fn find_dev_browser_via_npm_prefix() -> Option<String> {
    let output = Command::new("npm")
        .args(["prefix", "-g"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if prefix.is_empty() {
        return None;
    }

    npm_prefix_dev_browser_candidates(std::path::Path::new(&prefix), cfg!(windows))
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .find(|candidate| command_is_available(candidate))
}

/// Platform-specific native binary name shipped inside the dev-browser npm
/// package (e.g. `dev-browser-darwin-arm64`).  Returns `None` on unsupported
/// platforms.
fn dev_browser_native_binary_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("dev-browser-darwin-arm64"),
        ("macos", "x86_64") => Some("dev-browser-darwin-x64"),
        ("linux", "x86_64") => {
            if cfg!(target_env = "musl") {
                Some("dev-browser-linux-musl-x64")
            } else {
                Some("dev-browser-linux-x64")
            }
        }
        ("linux", "aarch64") if !cfg!(target_env = "musl") => Some("dev-browser-linux-arm64"),
        ("windows", "x86_64") => Some("dev-browser-windows-x64.exe"),
        _ => None,
    }
}

fn npm_prefix_dev_browser_candidates(prefix: &Path, windows: bool) -> Vec<PathBuf> {
    let mut candidates = if windows {
        vec![
            prefix.join("dev-browser.cmd"),
            prefix.join("dev-browser.exe"),
            prefix.join("dev-browser"),
        ]
    } else {
        vec![
            prefix.join("bin").join("dev-browser"),
            prefix.join("dev-browser"),
        ]
    };

    // Homebrew Node sometimes installs the package under node_modules but
    // does not create a bin symlink.  Probe the native binary directly.
    // On Unix, global packages live under {prefix}/lib/node_modules/;
    // on Windows they live directly under {prefix}/node_modules/.
    if let Some(native) = dev_browser_native_binary_name() {
        let modules_root = if windows {
            prefix.join("node_modules")
        } else {
            prefix.join("lib").join("node_modules")
        };
        candidates.push(modules_root.join("dev-browser").join("bin").join(native));
    }

    candidates
}

/// Resolve the dev-browser binary after installation has already been handled.
fn resolve_dev_browser() -> Result<String> {
    find_dev_browser()?.ok_or_else(missing_dev_browser_error)
}

/// Returns true if dev-browser is already available without side effects.
pub fn is_available() -> bool {
    find_dev_browser().is_ok_and(|bin| bin.is_some())
}

fn missing_dev_browser_error() -> anyhow::Error {
    anyhow!(DEV_BROWSER_INSTALL_GUIDANCE)
}

/// Ensure dev-browser is already available without downloading code at runtime.
pub fn ensure_installed() -> Result<()> {
    if find_dev_browser()?.is_some() {
        return Ok(());
    }
    Err(missing_dev_browser_error())
}

/// Stop the dev-browser daemon explicitly. Returns true when a running daemon
/// was asked to stop, false when no daemon was running.
pub fn stop_daemon() -> Result<bool> {
    let bin = resolve_dev_browser()?;
    let output = dev_browser_command(&bin)
        .arg("stop")
        .output()
        .with_context(|| format!("failed to run dev-browser stop (via {bin})"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit code {:?}", output.status.code())
        };
        return Err(anyhow!("dev-browser stop failed: {detail}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(!stdout.to_lowercase().contains("not running"))
}

/// Run a dev-browser script against a live Chrome instance (auto-connect).
/// Returns the script's stdout output.
pub fn run_script_connect(script: &str, timeout_secs: Option<u64>) -> Result<String> {
    run_script_connect_with_endpoint(script, timeout_secs, None)
}

fn resolve_dev_browser_connect_endpoint(cdp_endpoint: Option<&str>) -> Result<Option<String>> {
    let Some(endpoint) = cdp_endpoint else {
        return Ok(None);
    };

    // dev-browser already owns HTTP probing, /json/version fallback, and
    // DevToolsActivePort resolution. yoetz only validates that the caller
    // passed a recognizable CDP URL shape, then forwards it unchanged.
    let url = Url::parse(endpoint)
        .with_context(|| format!("invalid Chrome CDP endpoint `{endpoint}`"))?;
    match url.scheme() {
        "http" | "https" | "ws" | "wss" => {}
        other => {
            return Err(anyhow!(
                "unsupported Chrome CDP endpoint scheme `{other}` in `{endpoint}`"
            ));
        }
    }
    // Default: only forward localhost CDP endpoints. A non-loopback host can
    // bounce dev-browser onto an unrelated Chrome instance; set
    // YOETZ_CDP_ALLOW_REMOTE=1 to opt in (review finding #5).
    if !crate::chrome_devtools_mcp::client::is_loopback_host(url.host_str())
        && !crate::chrome_devtools_mcp::client::cdp_remote_redirects_allowed()
    {
        return Err(anyhow!(
            "Chrome CDP endpoint `{endpoint}` is not on localhost; set {}=1 to allow remote CDP targets",
            crate::chrome_devtools_mcp::client::YOETZ_CDP_ALLOW_REMOTE_ENV
        ));
    }
    Ok(Some(endpoint.to_string()))
}

fn connect_args(
    timeout_secs: u64,
    browser_name: Option<&str>,
    cdp_endpoint: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(browser_name) = browser_name {
        args.push("--browser".to_string());
        args.push(browser_name.to_string());
    }
    args.push("--connect".to_string());
    if let Some(endpoint) = cdp_endpoint {
        args.push(endpoint.to_string());
    }
    args.push("--timeout".to_string());
    args.push(timeout_secs.to_string());
    args
}

fn run_script_connect_with_browser_and_endpoint(
    script: &str,
    timeout_secs: Option<u64>,
    browser_name: Option<&str>,
    cdp_endpoint: Option<&str>,
) -> Result<String> {
    let bin = resolve_dev_browser()?;
    let timeout = timeout_secs.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS);
    let resolved_endpoint = resolve_dev_browser_connect_endpoint(cdp_endpoint)?;
    let args = connect_args(timeout, browser_name, resolved_endpoint.as_deref());

    let mut child = dev_browser_command(&bin)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run dev-browser (via {bin})"))?;
    {
        use std::io::Write;
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(script.as_bytes())?;
        }
    }
    drop(child.stdin.take());
    let output = wait_with_output_timeout(
        child,
        Duration::from_secs(timeout.saturating_add(DEV_BROWSER_PARENT_TIMEOUT_GRACE_SECS)),
    )
    .with_context(|| format!("failed to run dev-browser (via {bin})"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        // QuickJS WASM crashes with a GC assertion during sandbox disposal
        // AFTER the script has already completed and printed its output.
        // If stdout has content and stderr is just the GC crash, recover.
        let is_gc_crash =
            stderr.contains("list_empty(&rt->gc_obj_list)") || stderr.contains("JS_FreeRuntime");
        if is_gc_crash && !stdout.trim().is_empty() {
            let recovered = stdout
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| line.starts_with('{'))
                .unwrap_or(stdout.trim());
            eprintln!(
                "info: dev-browser sandbox GC crash on disposal (known QuickJS bug), recovering from stdout"
            );
            return Ok(recovered.to_string());
        }

        let detail = format_dev_browser_output_detail(&output);
        return Err(anyhow!("dev-browser script failed: {detail}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn wait_with_output_timeout(mut child: Child, timeout: Duration) -> Result<Output> {
    let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
    let stderr_reader = child.stderr.take().map(spawn_pipe_reader);
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child
                        .wait()
                        .context("failed to stop timed out dev-browser child")?;
                }
                thread::sleep(Duration::from_millis(DEV_BROWSER_WAIT_POLL_MS));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_pipe_reader(stdout_reader, "stdout");
                let _ = join_pipe_reader(stderr_reader, "stderr");
                return Err(err).context("failed while waiting for dev-browser child");
            }
        }
    };
    let stdout = join_pipe_reader(stdout_reader, "stdout")?;
    let stderr = join_pipe_reader(stderr_reader, "stderr")?;
    let output = Output {
        status,
        stdout,
        stderr,
    };

    if timed_out {
        let detail = format_dev_browser_output_detail(&output);
        Err(anyhow!(
            "dev-browser timed out after {}s while waiting for script output: {detail}",
            timeout.as_secs()
        ))
    } else {
        Ok(output)
    }
}

fn spawn_pipe_reader<R>(mut reader: R) -> JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        Ok(buf)
    })
}

fn join_pipe_reader(
    reader: Option<JoinHandle<io::Result<Vec<u8>>>>,
    label: &str,
) -> Result<Vec<u8>> {
    match reader {
        Some(reader) => reader
            .join()
            .map_err(|_| anyhow!("dev-browser {label} reader thread panicked"))?
            .with_context(|| format!("failed to read dev-browser {label}")),
        None => Ok(Vec::new()),
    }
}

fn format_dev_browser_output_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stderr.trim().is_empty() {
        stderr.to_string()
    } else if !stdout.trim().is_empty() {
        stdout.to_string()
    } else {
        format!("exit code {:?}", output.status.code())
    }
}

fn dev_browser_command(bin: &str) -> Command {
    let mut command = Command::new(bin);
    // Chrome 147+ built-in remote debugging can expose the first attached tab
    // as target type `other`. Playwright ignores those targets unless this
    // compatibility flag is set on the dev-browser process/daemon.
    command.env(DEV_BROWSER_ATTACH_TO_OTHER_ENV, "1");
    command
}

/// Run a dev-browser script against a live Chrome instance, optionally via an
/// explicit CDP endpoint.
pub fn run_script_connect_with_endpoint(
    script: &str,
    timeout_secs: Option<u64>,
    cdp_endpoint: Option<&str>,
) -> Result<String> {
    run_script_connect_with_browser_and_endpoint(script, timeout_secs, None, cdp_endpoint)
}

/// Stage a file into dev-browser's tmp directory so scripts can read it
/// via `readFile(name)`.
#[cfg(test)]
#[allow(dead_code)]
pub fn stage_file(name: &str, content: &str) -> Result<PathBuf> {
    let tmp_dir = dev_browser_tmp_dir();
    fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("create dev-browser tmp dir: {}", tmp_dir.display()))?;
    let path = tmp_dir.join(name);
    fs::write(&path, content).with_context(|| format!("write staged file: {}", path.display()))?;
    set_staged_file_permissions(&path)?;
    Ok(path)
}

#[cfg(test)]
#[allow(dead_code)]
fn set_staged_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .with_context(|| format!("read metadata: {}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("set permissions: {}", path.display()))?;
    }
    Ok(())
}

/// Check if Chrome is reachable and dev-browser can connect to it.
/// Uses a short first probe; retries once with a longer timeout only when the
/// first failure looks like a timeout (slow CDP handshake with many tabs).
pub fn check_connection_with_endpoint(cdp_endpoint: Option<&str>) -> Result<()> {
    let script = r#"
const pages = await browser.listPages();
console.log("ok:" + pages.length);
"#;
    match run_script_connect_with_browser_and_endpoint(
        script,
        Some(10),
        Some(CHATGPT_BROWSER_NAME),
        cdp_endpoint,
    ) {
        Ok(stdout) if stdout.trim().starts_with("ok:") => Ok(()),
        Ok(stdout) => Err(anyhow!("dev-browser connection check failed: {stdout}")),
        Err(first_err) => {
            if !is_dev_browser_connect_failure(&first_err) {
                return Err(first_err.context("dev-browser connection check failed"));
            }
            eprintln!("info: dev-browser connection check failed, retrying with longer timeout");
            std::thread::sleep(std::time::Duration::from_secs(2));
            let stdout = run_script_connect_with_browser_and_endpoint(
                script,
                Some(45),
                Some(CHATGPT_BROWSER_NAME),
                cdp_endpoint,
            )
            .context("dev-browser connection check failed after retry")?;
            if stdout.trim().starts_with("ok:") {
                Ok(())
            } else {
                Err(anyhow!("dev-browser connection check failed: {stdout}"))
            }
        }
    }
}

fn build_chatgpt_auth_probe_script(page_name: &str) -> String {
    let page_name_json = serde_json::to_string(page_name).expect("serialize page name");
    let chatgpt_url_json = serde_json::to_string(chatgpt_web::CHATGPT_URL).unwrap();
    format!(
        r##"
const PAGE_NAME = {page_name_json};
const CHATGPT_URL = {chatgpt_url_json};
const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
const page = await browser.getPage(PAGE_NAME);
const currentUrl = normalize(page.url()).toLowerCase();
if (!currentUrl.includes("chatgpt.com")) {{
  await page.goto(CHATGPT_URL, {{ waitUntil: "domcontentloaded" }});
}}
await page.waitForTimeout(1500);
const pageState = await page.evaluate(() => {{
  const composer = document.querySelector('#prompt-textarea, [role="textbox"]');
  const title = document.title || "";
  const bodyText = String(document.body?.innerText || "").replace(/\s+/g, " ").trim().slice(0, 400);
  return {{
    authenticated: !!composer,
    url: window.location.href || "",
    title,
    bodyText,
  }};
}});
console.log(JSON.stringify(pageState));
"##,
        chatgpt_url_json = chatgpt_url_json,
    )
}

#[derive(Debug, serde::Deserialize)]
struct ChatgptAuthProbeResult {
    authenticated: bool,
    url: String,
    title: String,
    #[serde(rename = "bodyText")]
    body_text: String,
}

fn chatgpt_page_probe_haystack(url: &str, title: &str, body_text: &str) -> String {
    format!("{url} {title} {body_text}")
}

fn check_chatgpt_auth_with_endpoint(cdp_endpoint: Option<&str>) -> Result<ChatgptAuthProbeResult> {
    let script = build_chatgpt_auth_probe_script(CHATGPT_AUTH_PROBE_PAGE_NAME);
    let stdout = run_script_connect_with_browser_and_endpoint(
        &script,
        Some(30),
        Some(CHATGPT_BROWSER_NAME),
        cdp_endpoint,
    )?;
    let result: ChatgptAuthProbeResult = serde_json::from_str(stdout.trim())
        .with_context(|| format!("check_chatgpt_auth: malformed script output: {stdout}"))?;
    Ok(result)
}

/// Ensure the connected Chrome session can reach an authenticated ChatGPT page.
/// Reuses the shared ChatGPT dev-browser slot when available so repeated checks
/// do not force a fresh Chrome live-attach handshake.
pub fn ensure_chatgpt_auth_with_page_check_and_endpoint(cdp_endpoint: Option<&str>) -> Result<()> {
    eprintln!("info: probing Chrome reachability via dev-browser");
    check_connection_with_endpoint(cdp_endpoint)
        .map_err(maybe_add_dev_browser_connect_guidance)
        .context(
            "dev-browser cannot connect to Chrome. Enable remote debugging: chrome://inspect/#remote-debugging",
        )?;

    eprintln!("info: probing ChatGPT auth state via dev-browser");
    let probe = check_chatgpt_auth_with_endpoint(cdp_endpoint)?;
    if probe.authenticated {
        return Ok(());
    }

    let haystack = chatgpt_page_probe_haystack(&probe.url, &probe.title, &probe.body_text);
    if let Some(issue) = chatgpt_web::detect_auth_issue_text(&haystack, true) {
        return Err(anyhow!("{issue}"));
    }

    Err(anyhow!(
        "ChatGPT did not finish loading the composer on {}. Title: {:?}",
        probe.url,
        probe.title
    ))
}

/// Context for running a ChatGPT recipe via dev-browser.
///
/// Recipe implementation note:
/// dev-browser runs scripts inside QuickJS/WASM, not Node. Keep browser flows
/// split into micro-scripts with JSON stdout handoffs instead of generating one
/// large helper-heavy script.
pub struct DevBrowserRecipeContext {
    /// Path to the bundle file on disk (used for macOS clipboard file upload).
    pub bundle_path: Option<PathBuf>,
    /// Bundle text content (for paste mode).
    pub bundle_text: Option<String>,
    /// Model slug (e.g., "gpt-5-4-pro"). Empty string = keep current.
    pub model: String,
    /// Whether to disable Extended Pro.
    pub disable_extended: bool,
    /// Whether to paste text instead of uploading as file.
    pub paste_mode: bool,
    /// Custom prompt text.
    pub prompt: String,
    /// Marker for the yoetz-owned ChatGPT tab created for this run.
    pub run_id: String,
    /// ChatGPT response polling settings.
    pub poll_settings: ChatgptPollSettings,
    /// Allow an empty assistant response to count as success.
    pub allow_empty_response: bool,
    /// Optional explicit CDP endpoint for selecting a specific Chrome instance.
    pub cdp_endpoint: Option<String>,
    /// Whether to print interactive Chrome-approval guidance to stderr.
    pub show_approval_guidance: bool,
}

impl Default for DevBrowserRecipeContext {
    fn default() -> Self {
        Self {
            bundle_path: None,
            bundle_text: None,
            model: String::new(),
            prompt: "Review the attached file and provide your analysis.".to_string(),
            run_id: String::new(),
            disable_extended: false,
            paste_mode: false,
            poll_settings: ChatgptPollSettings::default(),
            allow_empty_response: false,
            cdp_endpoint: None,
            show_approval_guidance: false,
        }
    }
}

pub fn resolve_chatgpt_poll_settings(
    vars: &BTreeMap<String, String>,
) -> Result<ChatgptPollSettings> {
    let mut settings = ChatgptPollSettings::default();
    if let Some(timeout_ms) = parse_positive_u64_var(vars, "wait_timeout_ms")? {
        settings.timeout_ms = timeout_ms;
    }
    if let Some(interval_ms) = parse_positive_u64_var(vars, "wait_interval_ms")? {
        settings.interval_ms = interval_ms;
    }
    Ok(settings)
}

fn parse_positive_u64_var(vars: &BTreeMap<String, String>, key: &str) -> Result<Option<u64>> {
    let Some(raw) = vars.get(key) else {
        return Ok(None);
    };
    let value = raw
        .parse::<u64>()
        .with_context(|| format!("invalid recipe var `{key}` value `{raw}`"))?;
    if value == 0 {
        return Err(anyhow!("recipe var `{key}` must be greater than 0"));
    }
    Ok(Some(value))
}

pub(crate) fn is_dev_browser_connect_failure(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    let has_connect_hint = message.contains("connectovercdp")
        || message.contains("auto-connect")
        || message.contains("auto connect")
        || message.contains("could not connect to chrome")
        || message.contains("browser.getversion")
        || message.contains("target.setautoattach");
    let has_connection_failure = message.contains("timed out")
        || message.contains("timeout")
        || message.contains("connectionclosed")
        || message.contains("underlying connection is closed")
        || message.contains("connection refused")
        || message.contains("socket hang up")
        || message.contains("closed");
    has_connect_hint && has_connection_failure
}

fn maybe_add_dev_browser_connect_guidance(err: anyhow::Error) -> anyhow::Error {
    if is_dev_browser_connect_failure(&err) {
        err.context(
            "dev-browser could not connect to Chrome. If Chrome is showing a remote debugging approval dialog, click Allow, then retry. If you recently upgraded yoetz, run `yoetz browser reset` once so the dev-browser daemon relaunches with the Chrome 147 compatibility flag. Raw transport error follows.",
        )
    } else {
        err
    }
}

fn chatgpt_script_timeout_secs(poll_timeout_ms: u64) -> u64 {
    poll_timeout_ms.div_ceil(1000) + 60
}

fn chatgpt_wait_heartbeat_interval_ms(interval_ms: u64) -> u64 {
    interval_ms.clamp(15_000, 60_000)
}

fn format_chatgpt_wait_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn chatgpt_wait_progress_message(elapsed: Duration, poll_settings: ChatgptPollSettings) -> String {
    format!(
        "info: still waiting for the ChatGPT response (elapsed {}, timeout {}, poll every {}s)",
        format_chatgpt_wait_duration(elapsed),
        format_chatgpt_wait_duration(Duration::from_millis(poll_settings.timeout_ms)),
        poll_settings.interval_ms / 1000
    )
}

struct ChatgptWaitHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ChatgptWaitHeartbeat {
    fn start(poll_settings: ChatgptPollSettings) -> Self {
        eprintln!(
            "info: waiting for the ChatGPT response (timeout {}, poll every {}s)",
            format_chatgpt_wait_duration(Duration::from_millis(poll_settings.timeout_ms)),
            poll_settings.interval_ms / 1000
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let started_at = Instant::now();
        let heartbeat_interval = Duration::from_millis(chatgpt_wait_heartbeat_interval_ms(
            poll_settings.interval_ms,
        ));
        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                thread::sleep(heartbeat_interval);
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                eprintln!(
                    "{}",
                    chatgpt_wait_progress_message(started_at.elapsed(), poll_settings)
                );
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct ChatgptPrepareResult {
    status: String,
    #[serde(rename = "loggedIn")]
    logged_in: bool,
    #[serde(rename = "composerReady")]
    composer_ready: bool,
    #[serde(rename = "modelUsed")]
    model_used: Option<String>,
    url: String,
    title: String,
    #[serde(rename = "bodyText")]
    body_text: String,
}

#[derive(Debug, serde::Deserialize)]
struct ChatgptSendResult {
    status: String,
    error: Option<String>,
    #[serde(rename = "assistantCountBeforeSend")]
    assistant_count_before_send: Option<usize>,
    #[serde(rename = "assistantLastLenBeforeSend")]
    assistant_last_len_before_send: Option<usize>,
    warning: Option<String>,
}

fn parse_script_json<T: serde::de::DeserializeOwned>(label: &str, stdout: &str) -> Result<T> {
    serde_json::from_str(stdout.trim())
        .with_context(|| format!("{label}: malformed script output: {stdout}"))
}

fn classify_dev_browser_page_issue(
    url: &str,
    title: &str,
    body_text: &str,
) -> Option<&'static str> {
    let haystack = chatgpt_page_probe_haystack(url, title, body_text);
    chatgpt_web::detect_auth_issue_text(&haystack, true)
}

fn build_chatgpt_prepare_script(page_name: &str, model: &str, run_id: &str) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    let model_json = serde_json::to_string(model).unwrap();
    let marked_url_json = serde_json::to_string(&chatgpt_web::mark_chatgpt_url(run_id)).unwrap();
    let window_name_json =
        serde_json::to_string(&format!("yoetz:{run_id}")).expect("serialize yoetz window name");
    let model_selection_function_json =
        serde_json::to_string(&chatgpt_web::build_model_selection_function(model)).unwrap();
    let composer_selector_json = chatgpt_web::composer_selector_json();
    format!(
        r##"
const PAGE_NAME = {page_name_json};
const MODEL = {model_json};
const AUTO_MODEL = !String(MODEL || "").trim() || String(MODEL || "").trim().toLowerCase() === "auto";
const MARKED_URL = {marked_url_json};
const WINDOW_NAME = {window_name_json};
const MODEL_SELECTION_FUNCTION_SOURCE = {model_selection_function_json};
const COMPOSER_SELECTOR = {composer_selector_json};
const ASSISTANT_SELECTOR = "[data-message-author-role='assistant']";
const NEW_CHAT_SELECTOR = "[data-testid='new-chat-button']";
const page = await browser.getPage(PAGE_NAME);
await page.goto(MARKED_URL, {{ waitUntil: "domcontentloaded" }});
await page.waitForTimeout(800);
await page.evaluate((name) => {{
  window.name = name;
  return window.name;
}}, WINDOW_NAME);
const loggedIn = (await page.locator("[data-testid='login-button']").first().count()) === 0;
const readState = async () => await page.evaluate(() => {{
  const composer = document.querySelector("#prompt-textarea, [role='textbox']");
  const assistantCount = document.querySelectorAll("[data-message-author-role='assistant']").length;
  const pathname = window.location.pathname || "/";
  const title = document.title || "";
  const bodyText = String(document.body?.innerText || "").replace(/\s+/g, " ").trim().slice(0, 400);
  return {{
    url: window.location.href,
    title,
    bodyText,
    pathname,
    onConversationPath: pathname.startsWith("/c/"),
    assistantCount,
    composerVisible: !!composer,
  }};
}});
let state = await readState();
let composerReady = loggedIn && state.composerVisible;
let selectedModel = null;
composerReady =
  composerReady &&
  state.assistantCount === 0 &&
  !state.onConversationPath;
if (loggedIn && !composerReady) {{
  for (let attempt = 0; attempt < 2 && !composerReady; attempt += 1) {{
    const canUseNewChat = attempt === 0;
    const newChatButton = canUseNewChat ? page.locator(NEW_CHAT_SELECTOR).first() : null;
    if (newChatButton && (await newChatButton.count() > 0)) {{
      await newChatButton.click({{ timeout: 5000 }});
    }} else {{
      await page.reload({{ waitUntil: "domcontentloaded" }});
    }}
    await page.waitForTimeout(1000);
    state = await readState();
    composerReady = state.composerVisible;
    composerReady =
      composerReady &&
      state.assistantCount === 0 &&
      !state.onConversationPath;
  }}
}}
if (loggedIn && composerReady) {{
  try {{
    await page.locator(COMPOSER_SELECTOR).first().waitFor({{ state: "visible", timeout: 20000 }});
  }} catch (_) {{
    composerReady = false;
  }}
}}
if (loggedIn && composerReady) {{
  const selection = await page.evaluate((functionSource) => {{
    const fn = eval("(" + functionSource + ")");
    return fn();
  }}, MODEL_SELECTION_FUNCTION_SOURCE);
  const selectionStatus = selection?.status || "unknown";
  const keepCurrentModel = AUTO_MODEL && ["missing-selector", "not-found"].includes(selectionStatus);
  if (!["selected", "already-selected"].includes(selectionStatus) && !keepCurrentModel) {{
    const diagnostics = JSON.stringify({{
      status: selectionStatus,
      requested: selection?.requested || MODEL || "",
      selectedLabel: selection?.selectedLabel || "",
      targetTestId: selection?.targetTestId || "",
      availableItems: selection?.availableItems || [],
      availableItemsAfter: selection?.availableItemsAfter || [],
    }});
    if (selectionStatus === "missing-selector") {{
      throw new Error("model selector button not found (" + diagnostics + ")");
    }}
    if (selectionStatus === "not-found") {{
      throw new Error("requested model '" + (selection?.requested || MODEL || "") + "' not found (" + diagnostics + ")");
    }}
    if (selectionStatus === "selection-mismatch") {{
      throw new Error("requested model '" + (selection?.requested || MODEL || "") + "' was not selected (" + diagnostics + ")");
    }}
    throw new Error("unexpected model selection status '" + selectionStatus + "' (" + diagnostics + ")");
  }}
  selectedModel =
    selection?.targetTestId ||
    selection?.modelUsed ||
    selection?.selectedLabel ||
    selection?.currentLabel ||
    null;
  await page.waitForTimeout(500);
}}
console.log(JSON.stringify({{
  status: !loggedIn ? "login_required" : composerReady ? "ready" : "not_ready",
  loggedIn,
  composerReady,
  modelUsed: selectedModel,
  url: state.url,
  title: state.title,
  bodyText: state.bodyText,
}}));
"##,
        page_name_json = page_name_json,
        model_json = model_json,
        marked_url_json = marked_url_json,
        window_name_json = window_name_json,
        model_selection_function_json = model_selection_function_json,
        composer_selector_json = composer_selector_json,
    )
}

fn build_chatgpt_send_script(
    page_name: &str,
    prompt: &str,
    delivery_text: &str,
    file_on_clipboard: bool,
    disable_extended: bool,
    bundle_file_name: Option<&str>,
) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    let file_on_clipboard_json = serde_json::to_string(&file_on_clipboard).unwrap();
    let delivery_text_json = serde_json::to_string(delivery_text).unwrap();
    let prompt_json = serde_json::to_string(prompt).unwrap();
    let bundle_file_name_json = serde_json::to_string(&bundle_file_name).unwrap();
    let composer_selector_json = chatgpt_web::composer_selector_json();
    let send_button_selector_json = chatgpt_web::send_button_selector_json();
    let stop_button_selector_json = chatgpt_web::stop_button_selector_json();
    let send_click_function_json =
        serde_json::to_string(&chatgpt_web::build_send_button_click_function()).unwrap();
    let attachment_probe_function_json = bundle_file_name.map(|file_name| {
        serde_json::to_string(&chatgpt_web::build_attachment_probe_function(file_name).unwrap())
            .unwrap()
    });
    format!(
        r##"
const PAGE_NAME = {page_name_json};
const FILE_ON_CLIPBOARD = {file_on_clipboard_json};
const DELIVERY_TEXT = {delivery_text_json};
const PROMPT = {prompt_json};
const DISABLE_EXTENDED = {disable_extended};
const BUNDLE_FILE_NAME = {bundle_file_name_json};
const COMPOSER_SELECTOR = {composer_selector_json};
const SEND_BUTTON_SELECTOR = {send_button_selector_json};
const STOP_BUTTON_SELECTOR = {stop_button_selector_json};
const SEND_CLICK_FUNCTION_SOURCE = {send_click_function_json};
const ATTACHMENT_PROBE_FUNCTION_SOURCE = {attachment_probe_function_json};
const page = await browser.getPage(PAGE_NAME);
let warning = null;
const waitForAttachmentReady = async () => {{
  if (!ATTACHMENT_PROBE_FUNCTION_SOURCE) return true;
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {{
    const state = await page.evaluate((functionSource) => {{
      const fn = eval("(" + functionSource + ")");
      return fn();
    }}, ATTACHMENT_PROBE_FUNCTION_SOURCE);
    if (state?.status === "done") {{
      return true;
    }}
    await page.waitForTimeout(500);
  }}
  return false;
}};
if (DISABLE_EXTENDED) {{
  const extButton = page.locator("button[aria-label*='click to remove'][aria-label*='Extended'], button[aria-label*='remove'][aria-label*='Extended']").first();
  if (await extButton.count() > 0) {{
    await extButton.click();
    await page.waitForTimeout(500);
  }} else {{
    warning = "extended disable requested but toggle not found";
  }}
}}
const composer = page.locator(COMPOSER_SELECTOR).first();
if (FILE_ON_CLIPBOARD) {{
  await composer.waitFor({{ state: "visible", timeout: 15000 }});
  await composer.click();
  await page.keyboard.press("Meta+v");
  const attached = await waitForAttachmentReady();
  if (!attached) {{
    throw new Error("file attachment did not finish uploading after clipboard paste");
  }}
}}
await composer.waitFor({{ state: "visible", timeout: 15000 }});
await composer.click();
await composer.pressSequentially(DELIVERY_TEXT, {{ delay: 15 }});
const sendBtn = page.locator(SEND_BUTTON_SELECTOR).first();
const readSendState = async () => await page.evaluate((composerSelector, sendSelector, bundleFileName) => {{
  const send = Array.from(document.querySelectorAll(sendSelector)).find((button) => {{
    const rect = button.getBoundingClientRect();
    const style = window.getComputedStyle(button);
    return rect.width > 0 &&
      rect.height > 0 &&
      style.visibility !== "hidden" &&
      style.display !== "none";
  }}) || null;
  const composerEl = document.querySelector(composerSelector);
  return {{
    sendButtonPresent: !!send,
    sendDisabled: send ? !!send.disabled : false,
    composerTextLength: (composerEl?.innerText || composerEl?.textContent || "").trim().length,
    attachmentPresent: !!bundleFileName,
  }};
}}, COMPOSER_SELECTOR, SEND_BUTTON_SELECTOR, BUNDLE_FILE_NAME);
const enableDeadline = Date.now() + 10000;
let sendState = await readSendState();
while (Date.now() < enableDeadline) {{
  if (await sendBtn.count() > 0 && await sendBtn.isEnabled()) break;
  await page.waitForTimeout(500);
  sendState = await readSendState();
}}
if (await sendBtn.count() === 0 || !(await sendBtn.isEnabled())) {{
  console.log(JSON.stringify({{
    status: "error",
    error: "ChatGPT send button never became enabled after typing; this usually means dev-browser is still on the broken Playwright live-attach path. If you upgraded yoetz, run `yoetz browser reset` once so the dev-browser daemon restarts with the Chrome 147 compatibility flag. " + JSON.stringify(sendState),
    warning,
  }}));
  return;
}}
const sendClick = await page.evaluate((functionSource) => {{
  const fn = eval("(" + functionSource + ")");
  return fn();
}}, SEND_CLICK_FUNCTION_SOURCE);
if (sendClick?.status !== "sent") {{
  console.log(JSON.stringify({{
    status: "error",
    error: "ChatGPT send click did not succeed: " + JSON.stringify(sendClick || null),
    warning,
  }}));
  return;
}}
const transitionDeadline = Date.now() + 10000;
let transitionState = null;
while (Date.now() < transitionDeadline) {{
  transitionState = await page.evaluate((baselineCount, composerSelector, sendSelector, stopSelector, bundleFileName) => {{
    const send = Array.from(document.querySelectorAll(sendSelector)).find((button) => {{
      const rect = button.getBoundingClientRect();
      const style = window.getComputedStyle(button);
      return rect.width > 0 &&
        rect.height > 0 &&
        style.visibility !== "hidden" &&
        style.display !== "none";
    }}) || null;
    const stopButton = document.querySelector(stopSelector);
    const assistantCount = document.querySelectorAll("[data-message-author-role='assistant']").length;
    const composerEl = document.querySelector(composerSelector);
    const composerText = (composerEl?.innerText || composerEl?.textContent || "").trim();
    return {{
      sendButtonPresent: !!send,
      sendDisabled: send ? !!send.disabled : false,
      stopButtonPresent: !!stopButton,
      assistantCount,
      composerTextLength: composerText.length,
      attachmentPresent: !!bundleFileName,
      transitionObserved:
        !!stopButton ||
        assistantCount > baselineCount ||
        !send ||
        (!!send && !!send.disabled) ||
        composerText.length === 0,
    }};
  }}, sendClick.assistantCountBeforeSend || 0, COMPOSER_SELECTOR, SEND_BUTTON_SELECTOR, STOP_BUTTON_SELECTOR, BUNDLE_FILE_NAME);
  if (transitionState.transitionObserved) break;
  await page.waitForTimeout(500);
}}
if (!transitionState || !transitionState.transitionObserved) {{
  console.log(JSON.stringify({{
    status: "error",
    error: "ChatGPT send click did not trigger a UI transition within 10s. " + JSON.stringify(transitionState || {{}}),
    assistantCountBeforeSend: sendClick.assistantCountBeforeSend || 0,
    assistantLastLenBeforeSend: sendClick.assistantLastLenBeforeSend || 0,
    warning,
  }}));
  return;
}}
console.log(JSON.stringify({{
        status: "sent",
        assistantCountBeforeSend: sendClick.assistantCountBeforeSend || 0,
        assistantLastLenBeforeSend: sendClick.assistantLastLenBeforeSend || 0,
        warning,
}}));
"##,
        page_name_json = page_name_json,
        file_on_clipboard_json = file_on_clipboard_json,
        delivery_text_json = delivery_text_json,
        prompt_json = prompt_json,
        bundle_file_name_json = bundle_file_name_json,
        composer_selector_json = composer_selector_json,
        send_button_selector_json = send_button_selector_json,
        stop_button_selector_json = stop_button_selector_json,
        send_click_function_json = send_click_function_json,
        attachment_probe_function_json =
            attachment_probe_function_json.unwrap_or_else(|| "null".to_string()),
        disable_extended = disable_extended,
    )
}

fn set_file_on_clipboard(path: &Path) -> Result<()> {
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("resolve bundle path: {}", path.display()))?;

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "set the clipboard to (POSIX file (item 1 of argv))",
                "-e",
                "end run",
            ])
            .arg(&canonical_path)
            .output()
            .context("failed to run osascript for ChatGPT file upload")?;
        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit code {:?}", output.status.code())
        };
        Err(anyhow!(
            "failed to set macOS clipboard to bundle file `{}`: {detail}",
            canonical_path.display()
        ))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = canonical_path;
        Err(anyhow!(
            "dev-browser file upload via clipboard currently requires macOS"
        ))
    }
}

fn build_chatgpt_poll_script(
    page_name: &str,
    assistant_count_before_send: usize,
    assistant_last_len_before_send: usize,
    poll_settings: ChatgptPollSettings,
    allow_empty_response: bool,
) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    let stable_idle_threshold_ms = chatgpt_web::stable_idle_threshold_ms(poll_settings.interval_ms);
    format!(
        r#"
const PAGE_NAME = {page_name_json};
const BASELINE_COUNT = {assistant_count_before_send};
const BASELINE_LAST_LEN = {assistant_last_len_before_send};
const POLL_TIMEOUT_MS = {poll_timeout_ms};
const POLL_INTERVAL_MS = {poll_interval_ms};
const STABLE_IDLE_THRESHOLD_MS = {stable_idle_threshold_ms};
const ALLOW_EMPTY_RESPONSE = {allow_empty_response};
const page = await browser.getPage(PAGE_NAME);
const start = Date.now();
let stableSince = null;
let stableKey = null;
while (Date.now() - start < POLL_TIMEOUT_MS) {{
  const state = await page.evaluate((baselineCount, baselineLastLen) => {{
    const errorEl = document.querySelector("[role='alert'], [data-testid*='error']");
    const hasThinkingIndicator = !!document.querySelector(".result-thinking, [data-testid*='thinking']");
    const allAssistantMessages = Array.from(document.querySelectorAll("[data-message-author-role='assistant']"));
    const assistantMessages = allAssistantMessages.slice(baselineCount);
    const lastAssistantMessage = allAssistantMessages.length > 0 ? allAssistantMessages[allAssistantMessages.length - 1] : null;
    const lastAssistantLen = (lastAssistantMessage?.innerText || "").length;
    const newAssistantCount = assistantMessages.length;
    const newMessage = allAssistantMessages.length > baselineCount;
    const sameMessageGrew = allAssistantMessages.length === baselineCount && lastAssistantLen > baselineLastLen;
    const response = (newMessage
      ? assistantMessages.map((message) => message.innerText).join("\n---\n")
      : (sameMessageGrew ? (lastAssistantMessage?.innerText || "") : "")
    ).trim();
    return {{
      error: errorEl ? errorEl.innerText.slice(0, 200).trim() : null,
      hasThinkingIndicator,
      hasStopButton: !!document.querySelector("[data-testid='stop-button']"),
      newAssistantCount,
      assistantCount: allAssistantMessages.length,
      lastAssistantLen,
      newMessage,
      sameMessageGrew,
      response,
    }};
  }}, BASELINE_COUNT, BASELINE_LAST_LEN);
  if (state.error) {{
    console.log(JSON.stringify({{ status: "error", error: state.error }}));
    return;
  }}
  const completionCandidate =
    !state.hasStopButton &&
    !state.hasThinkingIndicator &&
    ((state.newMessage && (ALLOW_EMPTY_RESPONSE || state.response.length > 0)) || state.sameMessageGrew);
  if (completionCandidate) {{
    const responseKey = `${{state.assistantCount}}:${{state.lastAssistantLen}}:${{state.response.length}}`;
    if (stableKey === responseKey) {{
      if (stableSince !== null && (Date.now() - stableSince) >= STABLE_IDLE_THRESHOLD_MS) {{
        console.log(JSON.stringify({{
          status: "ok",
          response: state.response,
          stable_for_ms: Date.now() - stableSince,
          stable_idle_threshold_ms: STABLE_IDLE_THRESHOLD_MS,
        }}));
        return;
      }}
    }} else {{
      stableKey = responseKey;
      stableSince = Date.now();
    }}
  }} else {{
    stableKey = null;
    stableSince = null;
  }}
  await page.waitForTimeout(POLL_INTERVAL_MS);
}}
console.log(JSON.stringify({{
  status: "timeout",
  error: `ChatGPT response timed out after ${{POLL_TIMEOUT_MS}}ms`,
}}));
"#,
        page_name_json = page_name_json,
        assistant_count_before_send = assistant_count_before_send,
        assistant_last_len_before_send = assistant_last_len_before_send,
        poll_timeout_ms = poll_settings.timeout_ms,
        poll_interval_ms = poll_settings.interval_ms,
        stable_idle_threshold_ms = stable_idle_threshold_ms,
        allow_empty_response = allow_empty_response,
    )
}

fn parse_chatgpt_recipe_result(
    stdout: &str,
    poll_timeout_ms: u64,
) -> Result<(String, Vec<String>)> {
    let result: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("parse chatgpt recipe result: {stdout}"))?;
    let pretty_result =
        || serde_json::to_string_pretty(&result).unwrap_or_else(|_| stdout.to_string());
    let warnings: Vec<String> = result
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let warning_suffix = || {
        if warnings.is_empty() {
            String::new()
        } else {
            format!(" (warnings: {})", warnings.join(" | "))
        }
    };

    let status = result
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "ChatGPT recipe result missing string status: {}",
                pretty_result()
            )
        })?;

    match status {
        "error" => {
            let err_msg = result["error"].as_str().unwrap_or("unknown error");
            Err(anyhow!("ChatGPT error: {err_msg}{}", warning_suffix()))
        }
        "timeout" => {
            let detail = result
                .get("error")
                .and_then(Value::as_str)
                .filter(|msg| !msg.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("ChatGPT response timed out after {}ms", poll_timeout_ms));
            Err(anyhow!("{detail}{}", warning_suffix()))
        }
        "ok" => match result["response"].as_str() {
            Some(response) => Ok((response.to_string(), warnings)),
            None => Err(anyhow!(
                "ChatGPT recipe returned status 'ok' but response field is missing or non-string: {}",
                pretty_result()
            )),
        },
        other => Err(anyhow!(
            "ChatGPT recipe returned unexpected status '{other}': {}",
            pretty_result()
        )),
    }
}

/// Run the ChatGPT recipe via dev-browser.
///
/// The flow is intentionally split into micro-scripts because dev-browser runs
/// QuickJS/WASM sandboxes, and large scripts with many closures are prone to a
/// GC assertion on disposal. Rust owns the orchestration; each script only does
/// one browser phase and returns JSON over stdout.
pub fn run_chatgpt_recipe(ctx: &DevBrowserRecipeContext) -> Result<ChatgptRecipeRunResult> {
    let browser_name = CHATGPT_BROWSER_NAME.to_string();
    let page_name = format!("{}-{}", CHATGPT_RECIPE_PAGE_NAME_PREFIX, ctx.run_id);
    let cdp_endpoint = ctx.cdp_endpoint.as_deref();
    let run_script = |script: &str, timeout_secs: Option<u64>| {
        run_script_connect_with_browser_and_endpoint(
            script,
            timeout_secs,
            Some(browser_name.as_str()),
            cdp_endpoint,
        )
    };

    let result = (|| -> Result<(String, Vec<String>, Option<String>)> {
        let mut warnings = Vec::new();
        let file_on_clipboard = if ctx.paste_mode {
            false
        } else if let Some(bundle_path) = &ctx.bundle_path {
            set_file_on_clipboard(bundle_path)?;
            true
        } else if ctx.bundle_text.is_some() {
            return Err(anyhow!(
                "dev-browser file upload requires a bundle path on disk; use `--var paste=true` for text-only delivery"
            ));
        } else {
            false
        };
        let delivery_text = if ctx.paste_mode {
            format!(
                "{}\n\n{}",
                ctx.prompt,
                ctx.bundle_text.as_deref().unwrap_or("")
            )
        } else {
            ctx.prompt.clone()
        };

        let prepare_script = build_chatgpt_prepare_script(&page_name, &ctx.model, &ctx.run_id);
        let prepare_stdout = {
            let approval_lock = browser::acquire_chrome_approval_lock()?;
            if ctx.show_approval_guidance {
                if approval_lock.waited() {
                    eprintln!(
                        "info: another yoetz process is already requesting Chrome approval; waiting for it to finish before trying the dev-browser transport"
                    );
                }
                eprintln!(
                    "info: connecting to Chrome — if prompted, click Allow in Chrome's remote debugging dialog"
                );
            }
            run_script(&prepare_script, Some(60)).map_err(maybe_add_dev_browser_connect_guidance)?
        };
        let prepare: ChatgptPrepareResult =
            parse_script_json("parse chatgpt prepare result", &prepare_stdout)?;
        let model_used = prepare
            .model_used
            .as_deref()
            .and_then(chatgpt_web::canonical_chatgpt_model_slug);
        let classified_issue =
            classify_dev_browser_page_issue(&prepare.url, &prepare.title, &prepare.body_text);
        match prepare.status.as_str() {
            "ready" if prepare.logged_in && prepare.composer_ready => {}
            "login_required" => {
                return Err(anyhow!(
                    "{}",
                    classified_issue.unwrap_or(
                        "chatgpt login required in the attached Chrome session. Log in there and try again."
                    )
                ));
            }
            "not_ready" => {
                if let Some(issue) = classified_issue {
                    return Err(anyhow!("{issue}"));
                }
                return Err(anyhow!(
                    "ChatGPT did not finish loading the composer on {} (title: {:?}). Restart Chrome with chrome://inspect/#remote-debugging enabled and try again.",
                    prepare.url,
                    prepare.title
                ));
            }
            other => {
                return Err(anyhow!(
                    "unexpected ChatGPT prepare status `{other}` on {}",
                    prepare.url
                ));
            }
        }

        let send_script = build_chatgpt_send_script(
            &page_name,
            &ctx.prompt,
            &delivery_text,
            file_on_clipboard,
            ctx.disable_extended,
            ctx.bundle_path
                .as_deref()
                .and_then(|path| path.file_name())
                .and_then(|value| value.to_str()),
        );
        let send_stdout = run_script(&send_script, Some(90))?;
        let send: ChatgptSendResult = parse_script_json("parse chatgpt send result", &send_stdout)?;
        match send.status.as_str() {
            "sent" => {}
            "error" => {
                let detail = send
                    .error
                    .unwrap_or_else(|| "ChatGPT send phase failed".to_string());
                return Err(anyhow!("{detail}"));
            }
            other => {
                return Err(anyhow!("unexpected ChatGPT send status `{other}`"));
            }
        }
        if let Some(warning) = send.warning {
            warnings.push(warning);
        }

        let poll_script = build_chatgpt_poll_script(
            &page_name,
            send.assistant_count_before_send.unwrap_or(0),
            send.assistant_last_len_before_send.unwrap_or(0),
            ctx.poll_settings,
            ctx.allow_empty_response,
        );
        let wait_started_at = Instant::now();
        let heartbeat = ChatgptWaitHeartbeat::start(ctx.poll_settings);
        let poll_result = run_script(
            &poll_script,
            Some(chatgpt_script_timeout_secs(ctx.poll_settings.timeout_ms)),
        );
        heartbeat.stop();
        let poll_stdout = poll_result?;
        let (response, mut poll_warnings) =
            parse_chatgpt_recipe_result(&poll_stdout, ctx.poll_settings.timeout_ms)?;
        eprintln!(
            "info: ChatGPT response completed after {}",
            format_chatgpt_wait_duration(wait_started_at.elapsed())
        );
        warnings.append(&mut poll_warnings);
        Ok((response, warnings, model_used))
    })();

    let (response, warnings, model_used) = result?;
    for warning in &warnings {
        eprintln!("warn: {warning}");
    }
    Ok(ChatgptRecipeRunResult {
        response,
        model_used,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_dev_browser_tmp_dir() {
        let dir = dev_browser_tmp_dir();
        assert!(dir.to_string_lossy().contains(".dev-browser"));
        assert!(dir.to_string_lossy().ends_with("tmp"));
    }

    #[test]
    fn test_stage_file() {
        let path = stage_file("test_stage.txt", "hello world").unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello world");
        let _ = fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn stage_file_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = stage_file("test_permissions.txt", "secret").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn resolve_chatgpt_poll_settings_uses_defaults() {
        assert_eq!(
            resolve_chatgpt_poll_settings(&BTreeMap::new()).unwrap(),
            ChatgptPollSettings::default()
        );
    }

    #[test]
    fn resolve_chatgpt_poll_settings_accepts_recipe_vars() {
        let vars = BTreeMap::from([
            ("wait_timeout_ms".to_string(), "900000".to_string()),
            ("wait_interval_ms".to_string(), "45000".to_string()),
        ]);
        assert_eq!(
            resolve_chatgpt_poll_settings(&vars).unwrap(),
            ChatgptPollSettings {
                timeout_ms: 900_000,
                interval_ms: 45_000,
            }
        );
    }

    #[test]
    fn resolve_chatgpt_poll_settings_rejects_zero_values() {
        let vars = BTreeMap::from([("wait_interval_ms".to_string(), "0".to_string())]);
        let err = resolve_chatgpt_poll_settings(&vars).unwrap_err();
        assert!(err.to_string().contains("wait_interval_ms"));
    }

    #[test]
    fn chatgpt_script_timeout_secs_adds_grace_window() {
        assert_eq!(chatgpt_script_timeout_secs(1_800_000), 1_860);
    }

    #[test]
    fn looks_like_dev_browser_connect_failure_matches_connect_timeout() {
        let err =
            anyhow!("browser.newPage: Timeout 30000ms exceeded while waiting for connectOverCDP");
        assert!(is_dev_browser_connect_failure(&err));

        let without_timeout = anyhow!(
            "browserType.connectOverCDP: connection closed while waiting for connectOverCDP"
        );
        assert!(is_dev_browser_connect_failure(&without_timeout));

        let other = anyhow!("ChatGPT response timed out after 900000ms");
        assert!(!is_dev_browser_connect_failure(&other));
    }

    #[test]
    fn maybe_add_dev_browser_connect_guidance_preserves_raw_cause_without_allow_marker() {
        let err = anyhow!(
            "browserType.connectOverCDP: Timeout 30000ms exceeded while waiting for connectOverCDP"
        );
        let err = maybe_add_dev_browser_connect_guidance(err);
        let detail = format!("{err:#}");
        assert!(detail.contains("dev-browser could not connect to Chrome"));
        assert!(detail.contains("browserType.connectOverCDP: Timeout 30000ms exceeded"));
        assert!(!detail.contains("Allow remote debugging"));
        assert!(detail.contains("yoetz browser reset"));
    }

    #[test]
    fn dev_browser_command_enables_attach_to_other_compat_flag() {
        let command = dev_browser_command("dev-browser");
        let envs = command.get_envs().collect::<Vec<_>>();
        assert!(
            envs.iter().any(|(key, value)| {
                *key == std::ffi::OsStr::new(DEV_BROWSER_ATTACH_TO_OTHER_ENV)
                    && *value == Some(std::ffi::OsStr::new("1"))
            }),
            "expected {DEV_BROWSER_ATTACH_TO_OTHER_ENV}=1 on dev-browser child"
        );
    }

    #[test]
    fn connect_args_include_optional_endpoint() {
        assert_eq!(
            connect_args(
                30,
                Some("yoetz-chatgpt-browser"),
                Some("http://127.0.0.1:9222"),
            ),
            vec![
                "--browser".to_string(),
                "yoetz-chatgpt-browser".to_string(),
                "--connect".to_string(),
                "http://127.0.0.1:9222".to_string(),
                "--timeout".to_string(),
                "30".to_string(),
            ]
        );
        assert_eq!(
            connect_args(45, None, None),
            vec![
                "--connect".to_string(),
                "--timeout".to_string(),
                "45".to_string(),
            ]
        );
    }

    #[test]
    fn resolve_dev_browser_connect_endpoint_skips_probing_for_auto_connect() {
        assert_eq!(resolve_dev_browser_connect_endpoint(None).unwrap(), None);
    }

    #[test]
    fn resolve_dev_browser_connect_endpoint_passes_explicit_endpoints_through() {
        assert_eq!(
            resolve_dev_browser_connect_endpoint(Some("http://127.0.0.1:9222")).unwrap(),
            Some("http://127.0.0.1:9222".to_string())
        );
        assert_eq!(
            resolve_dev_browser_connect_endpoint(Some(
                "ws://127.0.0.1:9222/devtools/browser/test-browser-id"
            ))
            .unwrap(),
            Some("ws://127.0.0.1:9222/devtools/browser/test-browser-id".to_string())
        );
        assert_eq!(
            resolve_dev_browser_connect_endpoint(Some("http://localhost:9222")).unwrap(),
            Some("http://localhost:9222".to_string())
        );
    }

    #[test]
    #[serial_test::serial]
    fn resolve_dev_browser_connect_endpoint_rejects_remote_by_default() {
        // Non-loopback CDP endpoints must be rejected unless the operator opts
        // in via YOETZ_CDP_ALLOW_REMOTE=1 (review finding #5).
        let previous =
            std::env::var(crate::chrome_devtools_mcp::client::YOETZ_CDP_ALLOW_REMOTE_ENV).ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(crate::chrome_devtools_mcp::client::YOETZ_CDP_ALLOW_REMOTE_ENV);
        }
        let err = resolve_dev_browser_connect_endpoint(Some("http://attacker.example.com:9222"))
            .expect_err("remote endpoints must be rejected by default");
        let message = format!("{err:#}");
        assert!(message.contains("not on localhost"));
        assert!(message.contains(crate::chrome_devtools_mcp::client::YOETZ_CDP_ALLOW_REMOTE_ENV));
        if let Some(value) = previous {
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var(
                    crate::chrome_devtools_mcp::client::YOETZ_CDP_ALLOW_REMOTE_ENV,
                    value,
                );
            }
        }
    }

    #[test]
    fn build_chatgpt_prepare_script_uses_named_page_and_login_check() {
        let script = build_chatgpt_prepare_script("yoetz-chatgpt-test", "gpt-5-4-pro", "run-123");

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const MODEL = \"gpt-5-4-pro\";"));
        assert!(script.contains(
            "const AUTO_MODEL = !String(MODEL || \"\").trim() || String(MODEL || \"\").trim().toLowerCase() === \"auto\";"
        ));
        assert!(script.contains("const MARKED_URL = \"https://chatgpt.com/?_yoetz=run-123\";"));
        assert!(script.contains("const WINDOW_NAME = \"yoetz:run-123\";"));
        assert!(
            script.contains("await page.goto(MARKED_URL, { waitUntil: \"domcontentloaded\" });")
        );
        assert!(script.contains("window.name = name;"));
        assert!(script.contains("[data-testid='login-button']"));
        assert!(script.contains("const NEW_CHAT_SELECTOR = \"[data-testid='new-chat-button']\";"));
        assert!(script.contains("const MODEL_SELECTION_FUNCTION_SOURCE ="));
        assert!(script.contains("const canUseNewChat = attempt === 0;"));
        assert!(script.contains("await page.reload({ waitUntil: \"domcontentloaded\" });"));
        assert!(script.contains("page.evaluate(() => {"));
        assert!(script.contains("state.assistantCount === 0"));
        assert!(script.contains("pathname.startsWith(\"/c/\")"));
        assert!(script.contains("const selection = await page.evaluate((functionSource) => {"));
        assert!(script.contains("\\\"gpt-5-pro\\\":\\\"gpt-5-4-pro\\\""));
        assert!(!script.contains("\\\"gpt-5-3-pro\\\""));
        assert!(script.contains(
            "requested model '\" + (selection?.requested || MODEL || \"\") + \"' was not selected"
        ));
        assert!(script.contains("let selectedModel = null;"));
        assert!(script.contains(
            "const keepCurrentModel = AUTO_MODEL && [\"missing-selector\", \"not-found\"].includes(selectionStatus);"
        ));
        assert!(script.contains("selectedModel ="));
        assert!(script.contains("selection?.currentLabel ||"));
        assert!(script.contains("modelUsed: selectedModel,"));
        assert!(script.contains("bodyText"));
        assert!(script.contains(
            "status: !loggedIn ? \"login_required\" : composerReady ? \"ready\" : \"not_ready\""
        ));
    }

    #[test]
    fn build_chatgpt_prepare_script_marks_a_yoetz_owned_tab() {
        let script = build_chatgpt_prepare_script("yoetz-chatgpt-test", "pro", "run-456");
        assert!(script.contains("const MARKED_URL = \"https://chatgpt.com/?_yoetz=run-456\";"));
        assert!(script.contains("const WINDOW_NAME = \"yoetz:run-456\";"));
        assert!(
            script.contains("await page.goto(MARKED_URL, { waitUntil: \"domcontentloaded\" });")
        );
        assert!(script.contains("window.name = name;"));
    }

    #[test]
    fn chatgpt_auth_probe_script_uses_exact_named_page_only() {
        let script = build_chatgpt_auth_probe_script("yoetz-chatgpt-test");

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("await browser.getPage(PAGE_NAME)"));
        assert!(
            script.contains("await page.goto(CHATGPT_URL, { waitUntil: \"domcontentloaded\" });")
        );
        assert!(script.contains("bodyText"));
        assert!(!script.contains("browser.newPage()"));
        assert!(!script.contains(
            "pages.find((entry) => normalize(entry.url).toLowerCase().includes(\"chatgpt.com\"))"
        ));
    }

    #[test]
    fn chatgpt_recipe_uses_stable_browser_and_page_names() {
        assert_eq!(CHATGPT_BROWSER_NAME, "yoetz-chatgpt");
        assert_eq!(CHATGPT_AUTH_PROBE_PAGE_NAME, "yoetz-chatgpt-main");
        assert_eq!(CHATGPT_RECIPE_PAGE_NAME_PREFIX, "yoetz-chatgpt-run");
        assert!(!CHATGPT_AUTH_PROBE_PAGE_NAME.contains("pid"));
    }

    #[test]
    fn build_chatgpt_send_script_uses_clipboard_upload_and_press_sequentially() {
        let script = build_chatgpt_send_script(
            "yoetz-chatgpt-test",
            "Review this file.",
            "Review this file.",
            true,
            true,
            Some("bundle.txt"),
        );

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const FILE_ON_CLIPBOARD = true;"));
        assert!(script.contains("await composer.waitFor({ state: \"visible\", timeout: 15000 });"));
        assert!(script.contains("await page.keyboard.press(\"Meta+v\");"));
        assert!(script.contains("file attachment did not finish uploading after clipboard paste"));
        assert!(script.contains("const ATTACHMENT_PROBE_FUNCTION_SOURCE ="));
        assert!(script.contains("const SEND_CLICK_FUNCTION_SOURCE ="));
        assert!(script.contains("assistantLastLenBeforeSend"));
        assert!(script.contains("pressSequentially(DELIVERY_TEXT, { delay: 15 })"));
        assert!(script.contains("status: \"sent\""));
    }

    #[test]
    fn parse_script_json_reads_prepare_result() {
        let result: ChatgptPrepareResult = parse_script_json(
            "prepare",
            r#"{"status":"ready","loggedIn":true,"composerReady":true,"modelUsed":"model-switcher-gpt-5-4-pro","url":"https://chatgpt.com/","title":"ChatGPT","bodyText":"Send a message"}"#,
        )
        .unwrap();

        assert_eq!(result.status, "ready");
        assert!(result.logged_in);
        assert!(result.composer_ready);
        assert_eq!(
            result.model_used.as_deref(),
            Some("model-switcher-gpt-5-4-pro")
        );
        assert_eq!(result.title, "ChatGPT");
    }

    #[test]
    fn build_chatgpt_poll_script_waits_for_stable_non_thinking_idle() {
        let script = build_chatgpt_poll_script(
            "yoetz-chatgpt-test",
            3,
            120,
            ChatgptPollSettings {
                timeout_ms: 900_000,
                interval_ms: 45_000,
            },
            false,
        );

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const BASELINE_COUNT = 3;"));
        assert!(script.contains("const BASELINE_LAST_LEN = 120;"));
        assert!(script.contains("const POLL_TIMEOUT_MS = 900000;"));
        assert!(script.contains("const POLL_INTERVAL_MS = 45000;"));
        assert!(script.contains("const STABLE_IDLE_THRESHOLD_MS = 135000;"));
        assert!(script.contains("const ALLOW_EMPTY_RESPONSE = false;"));
        assert!(script.contains("let stableSince = null;"));
        assert!(script.contains(".result-thinking, [data-testid*='thinking']"));
        assert!(script.contains("[data-testid='stop-button']"));
        assert!(script.contains("[data-message-author-role='assistant']"));
        assert!(script.contains("sameMessageGrew"));
        assert!(script.contains("stableKey === responseKey"));
        assert!(script.contains("status: \"ok\""));
        assert!(script.contains("status: \"timeout\""));
    }

    #[test]
    fn classify_dev_browser_page_issue_matches_challenge_and_login_states() {
        assert_eq!(
            classify_dev_browser_page_issue(
                "https://chatgpt.com/",
                "Just a moment...",
                "Verify you are human"
            ),
            Some(
                "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again."
            )
        );
        assert_eq!(
            classify_dev_browser_page_issue(
                "https://auth.openai.com/login",
                "Log in",
                "Continue with Google"
            ),
            Some("chatgpt login required in the attached Chrome session. Log in there and try again.")
        );
    }

    #[test]
    fn chatgpt_wait_heartbeat_interval_is_clamped() {
        assert_eq!(chatgpt_wait_heartbeat_interval_ms(1_000), 15_000);
        assert_eq!(chatgpt_wait_heartbeat_interval_ms(30_000), 30_000);
        assert_eq!(chatgpt_wait_heartbeat_interval_ms(120_000), 60_000);
    }

    #[test]
    fn chatgpt_wait_progress_message_is_human_readable() {
        let message = chatgpt_wait_progress_message(
            Duration::from_secs(95),
            ChatgptPollSettings {
                timeout_ms: 1_800_000,
                interval_ms: 30_000,
            },
        );
        assert!(message.contains("elapsed 1m 35s"));
        assert!(message.contains("timeout 30m 0s"));
        assert!(message.contains("poll every 30s"));
    }

    #[test]
    fn parse_chatgpt_recipe_result_requires_string_response_for_ok_status() {
        let err =
            parse_chatgpt_recipe_result(r#"{"status":"ok","response":null}"#, 900_000).unwrap_err();
        assert!(err
            .to_string()
            .contains("status 'ok' but response field is missing or non-string"));
    }

    #[test]
    fn parse_chatgpt_recipe_result_returns_response_and_warnings() {
        let (response, warnings) = parse_chatgpt_recipe_result(
            r#"{"status":"ok","response":"done","warnings":["kept current model"]}"#,
            900_000,
        )
        .unwrap();

        assert_eq!(response, "done");
        assert_eq!(warnings, vec!["kept current model".to_string()]);
    }

    #[test]
    fn parse_chatgpt_recipe_result_includes_warnings_on_timeout() {
        let err = parse_chatgpt_recipe_result(
            r#"{"status":"timeout","error":"ChatGPT response timed out after 900000ms (last_state={})","warnings":["extended disable requested but toggle not found"]}"#,
            900_000,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("extended disable requested but toggle not found"));
    }

    #[test]
    fn npm_prefix_candidates_cover_unix_and_windows_layouts() {
        let unix = npm_prefix_dev_browser_candidates(Path::new("/prefix"), false);
        // First two are the standard bin symlink paths.
        assert_eq!(unix[0], PathBuf::from("/prefix/bin/dev-browser"));
        assert_eq!(unix[1], PathBuf::from("/prefix/dev-browser"));
        // Third is the native binary inside node_modules (Homebrew fallback).
        assert!(
            unix.len() >= 3,
            "expected native binary candidate for this platform"
        );
        assert!(
            unix[2]
                .to_string_lossy()
                .starts_with("/prefix/lib/node_modules/dev-browser/bin/dev-browser-"),
            "native candidate should be under node_modules: {:?}",
            unix[2]
        );

        let windows = npm_prefix_dev_browser_candidates(Path::new(r"C:\npm"), true);
        assert_eq!(windows[0], PathBuf::from(r"C:\npm/dev-browser.cmd"));
        assert_eq!(windows[1], PathBuf::from(r"C:\npm/dev-browser.exe"));
        assert_eq!(windows[2], PathBuf::from(r"C:\npm/dev-browser"));
        // Windows native candidate lives under node_modules/ (no lib/ prefix).
        assert!(
            windows.len() >= 4,
            "expected native binary candidate for windows"
        );
        assert!(
            windows[3]
                .to_string_lossy()
                .starts_with(r"C:\npm/node_modules/dev-browser/bin/dev-browser-"),
            "windows native candidate should be under node_modules (no lib/): {:?}",
            windows[3]
        );
    }

    #[test]
    fn dev_browser_native_binary_name_returns_some_on_supported_platforms() {
        // This test runs on the host platform, so it should always return Some.
        let name = dev_browser_native_binary_name();
        assert!(
            name.is_some(),
            "expected a native binary name for the current platform"
        );
        assert!(name.unwrap().starts_with("dev-browser-"));
    }

    #[test]
    fn command_is_available_accepts_existing_binary_even_with_non_zero_help_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = if cfg!(windows) {
            dir.path().join("fake-dev-browser.cmd")
        } else {
            dir.path().join("fake-dev-browser")
        };
        let contents = if cfg!(windows) {
            "@echo off\r\nexit /b 1\r\n"
        } else {
            "#!/bin/sh\nexit 1\n"
        };
        fs::write(&script, contents).unwrap();
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();
        }

        assert!(command_is_available(script.to_str().unwrap()));
    }

    #[test]
    fn missing_dev_browser_error_requires_explicit_install() {
        let detail = missing_dev_browser_error().to_string();
        assert!(detail.contains("Install it explicitly"));
        assert!(detail.contains("YOETZ_DEV_BROWSER_BIN"));
        assert!(!detail.contains("installing via npm"));
    }
}

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

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Url;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
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

use crate::chatgpt_recipe::AnyhowResultExt;
use crate::claude_recipe::AnyhowResultExt as ClaudeAnyhowResultExt;
use crate::web_recipe::{WebModelSelectionStatus, WebRecipeTransportPhase};
use crate::{browser, chatgpt_recipe, chatgpt_web, claude_recipe, claude_web};

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

/// Extended timeout for ChatGPT response polling (90 minutes by default).
const CHATGPT_POLL_TIMEOUT_MS_DEFAULT: u64 = 5_400_000;
const CHATGPT_POLL_INTERVAL_MS_DEFAULT: u64 = 30_000;
const CHATGPT_UPLOAD_TIMEOUT_MS_DEFAULT: u64 = 120_000;
const CHATGPT_SEND_TIMEOUT_MS_DEFAULT: u64 = 120_000;
const CHATGPT_BROWSER_NAME: &str = "yoetz-chatgpt";
const CHATGPT_AUTH_PROBE_PAGE_NAME: &str = "yoetz-chatgpt-main";
const CHATGPT_RECIPE_PAGE_NAME_PREFIX: &str = "yoetz-chatgpt-run";
const CLAUDE_BROWSER_NAME: &str = "yoetz-claude";
const CLAUDE_AUTH_PROBE_PAGE_NAME: &str = "yoetz-claude-main";
const CLAUDE_RECIPE_PAGE_NAME_PREFIX: &str = "yoetz-claude-run";
const CLAUDE_NO_PROGRESS_POLL_LIMIT: u8 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatgptPollSettings {
    pub timeout_ms: u64,
    pub interval_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatgptRecipeRunResult {
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: chatgpt_recipe::ChatgptModelSelectionStatus,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeRecipeRunResult {
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: WebModelSelectionStatus,
    pub warnings: Vec<String>,
    pub conversation_id: Option<String>,
    pub conversation_url: Option<String>,
    pub used_clipboard: bool,
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
    if let Some(bin) = configured_dev_browser_bin()? {
        let _ = DEV_BROWSER.set(bin.clone());
        return Ok(Some(bin));
    }
    if let Some(bin) = DEV_BROWSER.get() {
        return Ok(Some(bin.clone()));
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

/// Returns true when yoetz can execute dev-browser-style scripts through any
/// supported backend, including the bundled live-CDP compatibility daemon.
pub fn has_any_backend() -> bool {
    is_available() || crate::live_cdp_daemon::is_available()
}

fn missing_dev_browser_error() -> anyhow::Error {
    anyhow!(DEV_BROWSER_INSTALL_GUIDANCE)
}

/// Ensure dev-browser is already available without downloading code at runtime.
#[allow(dead_code)]
pub fn ensure_installed() -> Result<()> {
    if find_dev_browser()?.is_some() {
        return Ok(());
    }
    Err(missing_dev_browser_error())
}

/// Stop the dev-browser daemon explicitly. Returns true when a running daemon
/// was asked to stop, false when no daemon was running.
pub fn stop_daemon() -> Result<bool> {
    let external_result = find_dev_browser();
    let live_cdp_stopped = crate::live_cdp_daemon::stop_live_cdp_daemon()?;

    let external_bin = match external_result {
        Ok(bin) => bin,
        Err(error) if live_cdp_stopped => {
            eprintln!("warning: could not resolve dev-browser while stopping daemons: {error}");
            return Ok(true);
        }
        Err(error) => return Err(error),
    };

    let external_stopped = if let Some(bin) = external_bin {
        let output = dev_browser_command(&bin).arg("stop").output();
        let output = match output {
            Ok(output) => output,
            Err(error) if live_cdp_stopped => {
                eprintln!("warning: failed to run dev-browser stop (via {bin}): {error}");
                return Ok(true);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to run dev-browser stop (via {bin})"));
            }
        };

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
        !stdout.to_lowercase().contains("not running")
    } else {
        false
    };
    Ok(external_stopped || live_cdp_stopped)
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
    let timeout = timeout_secs.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS);
    let resolved_endpoint = resolve_dev_browser_connect_endpoint(cdp_endpoint)?;
    if crate::live_cdp_daemon::is_available() {
        return crate::live_cdp_daemon::run_script_connect(
            script,
            timeout,
            browser_name,
            resolved_endpoint.as_deref(),
        );
    }

    let bin = resolve_dev_browser()?;
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
            if !should_retry_dev_browser_connect_failure(&first_err) {
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
    /// Path to the bundle file on disk (used for composer-scoped file upload).
    pub bundle_path: Option<PathBuf>,
    /// Bundle text content (for paste mode).
    pub bundle_text: Option<String>,
    /// ChatGPT model slug to select.
    pub model: String,
    /// ChatGPT model selection strategy.
    pub model_strategy: chatgpt_recipe::ChatgptModelStrategy,
    /// Whether to paste text instead of uploading as file.
    pub paste_mode: bool,
    /// Custom prompt text.
    pub prompt: String,
    /// Marker for the yoetz-owned ChatGPT tab created for this run.
    pub run_id: String,
    /// ChatGPT response polling settings.
    pub poll_settings: ChatgptPollSettings,
    /// Maximum time to wait for a file attachment to finish uploading.
    pub upload_timeout_ms: u64,
    /// Allow an empty assistant response to count as success.
    pub allow_empty_response: bool,
    /// Optional explicit CDP endpoint for selecting a specific Chrome instance.
    pub cdp_endpoint: Option<String>,
    /// Whether to print interactive Chrome-approval guidance to stderr.
    pub show_approval_guidance: bool,
}

#[derive(Clone, Debug)]
pub struct ClaudeDevBrowserRecipeContext {
    pub bundle_path: Option<PathBuf>,
    pub prompt: String,
    pub run_id: String,
    pub poll_settings: ChatgptPollSettings,
    pub cdp_endpoint: Option<String>,
    pub show_approval_guidance: bool,
    pub upload_timeout_ms: u64,
    pub send_timeout_ms: u64,
    pub warnings: Vec<String>,
}

impl Default for DevBrowserRecipeContext {
    fn default() -> Self {
        Self {
            bundle_path: None,
            bundle_text: None,
            model: String::new(),
            model_strategy: chatgpt_recipe::ChatgptModelStrategy::Select,
            prompt: "Review the attached file and provide your analysis.".to_string(),
            run_id: String::new(),
            paste_mode: false,
            poll_settings: ChatgptPollSettings::default(),
            upload_timeout_ms: CHATGPT_UPLOAD_TIMEOUT_MS_DEFAULT,
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

pub fn resolve_chatgpt_upload_timeout_ms(
    vars: &BTreeMap<String, String>,
    bundle_path: Option<&Path>,
) -> Result<u64> {
    let base_timeout_ms = parse_positive_u64_var(vars, "upload_timeout_ms")?
        .unwrap_or(CHATGPT_UPLOAD_TIMEOUT_MS_DEFAULT);
    let Some(bundle_path) = bundle_path else {
        return Ok(base_timeout_ms);
    };
    let Ok(metadata) = fs::metadata(bundle_path) else {
        return Ok(base_timeout_ms);
    };
    Ok(scale_chatgpt_upload_timeout_ms(
        base_timeout_ms,
        metadata.len(),
    ))
}

pub fn resolve_chatgpt_send_timeout_ms(vars: &BTreeMap<String, String>) -> Result<u64> {
    parse_positive_u64_var(vars, "send_timeout_ms")
        .map(|value| value.unwrap_or(CHATGPT_SEND_TIMEOUT_MS_DEFAULT))
}

fn scale_chatgpt_upload_timeout_ms(base_timeout_ms: u64, file_size_bytes: u64) -> u64 {
    const BYTES_PER_MIB: u64 = 1024 * 1024;
    const EXTRA_MS_PER_MIB: u64 = 5_000;
    const MAX_UPLOAD_TIMEOUT_MS: u64 = 3_600_000;

    let mib = file_size_bytes.div_ceil(BYTES_PER_MIB);
    let scaled = base_timeout_ms.saturating_add(mib.saturating_mul(EXTRA_MS_PER_MIB));
    scaled.min(MAX_UPLOAD_TIMEOUT_MS)
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
        || message.contains("target.setautoattach")
        || message.contains("target.gettargets")
        || message.contains("initializing live cdp browser")
        || message.contains("initializing live cdp targets")
        || message.contains("remote-debugging consent");
    let has_connection_failure = message.contains("timed out")
        || message.contains("timeout")
        || message.contains("connectionclosed")
        || message.contains("underlying connection is closed")
        || message.contains("connection refused")
        || message.contains("socket hang up")
        || message.contains("closed");
    has_connect_hint && has_connection_failure
}

fn is_dev_browser_target_auto_attach_failure(err: &anyhow::Error) -> bool {
    format!("{err:#}")
        .to_lowercase()
        .contains("target.setautoattach")
}

fn is_dev_browser_remote_debugging_consent_wait(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    message.contains("remote-debugging consent") || message.contains("remote debugging consent")
}

fn should_retry_dev_browser_connect_failure(err: &anyhow::Error) -> bool {
    is_dev_browser_connect_failure(err)
        && !is_dev_browser_target_auto_attach_failure(err)
        && !is_dev_browser_remote_debugging_consent_wait(err)
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
    #[serde(rename = "modelSelection")]
    model_selection: Option<Value>,
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

fn build_chatgpt_prepare_script(
    page_name: &str,
    model: &str,
    model_strategy: chatgpt_recipe::ChatgptModelStrategy,
    run_id: &str,
) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    let model_json = serde_json::to_string(model).unwrap();
    let model_strategy_json = serde_json::to_string(&model_strategy).unwrap();
    let marked_url_json = serde_json::to_string(&chatgpt_web::mark_chatgpt_url(run_id)).unwrap();
    let window_name_json =
        serde_json::to_string(&format!("yoetz:{run_id}")).expect("serialize yoetz window name");
    let model_selection_function_json = serde_json::to_string(
        &chatgpt_web::build_model_selection_function(model, model_strategy),
    )
    .unwrap();
    let composer_selector_json = chatgpt_web::composer_selector_json();
    format!(
        r##"
const PAGE_NAME = {page_name_json};
const MODEL = {model_json};
const MODEL_STRATEGY = {model_strategy_json};
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
let modelSelection = null;
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
  modelSelection = selection || null;
  if (MODEL_STRATEGY === "current") {{
    if (selectionStatus !== "current") {{
      const diagnostics = JSON.stringify({{
        status: selectionStatus,
        requested: selection?.requested || MODEL || "",
        familyStatus: selection?.familyStatus || "skipped",
        effortStatus: selection?.effortStatus || "skipped",
        pillText: selection?.pillText || "",
        warning: selection?.warning || "",
        availableItems: selection?.availableItems || [],
        availableFamilies: selection?.availableFamilies || [],
      }});
      throw new Error("unexpected model selection status '" + selectionStatus + "' (" + diagnostics + ")");
    }}
    selectedModel = selection?.modelUsed ?? "";
  }} else if (selectionStatus !== "selected") {{
    const diagnostics = JSON.stringify({{
      status: selectionStatus,
      requested: selection?.requested || MODEL || "",
      familyStatus: selection?.familyStatus || "unverified",
      effortStatus: selection?.effortStatus || "unverified",
      familyLabel: selection?.familyLabel || "",
      warning: selection?.warning || "",
      availableItems: selection?.availableItems || [],
      availableFamilies: selection?.availableFamilies || [],
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
  selectedModel = selection?.modelUsed ?? null;
  await page.waitForTimeout(500);
}}
console.log(JSON.stringify({{
  status: !loggedIn ? "login_required" : composerReady ? "ready" : "not_ready",
  loggedIn,
  composerReady,
  modelUsed: selectedModel,
  modelSelection,
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
    file_upload_path: Option<&str>,
    upload_timeout_ms: u64,
    bundle_file_name: Option<&str>,
) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    let file_upload_path_json = serde_json::to_string(&file_upload_path).unwrap();
    let delivery_text_json = serde_json::to_string(delivery_text).unwrap();
    let prompt_json = serde_json::to_string(prompt).unwrap();
    let bundle_file_name_json = serde_json::to_string(&bundle_file_name).unwrap();
    let composer_selector_json = chatgpt_web::composer_selector_json();
    let send_button_selector_json = chatgpt_web::send_button_selector_json();
    let stop_button_selector_json = chatgpt_web::stop_button_selector_json();
    let send_click_function_json =
        serde_json::to_string(&chatgpt_web::build_send_button_click_function()).unwrap();
    let open_attachment_ui_function_json =
        serde_json::to_string(&chatgpt_web::build_open_attachment_ui_function()).unwrap();
    let upload_menu_item_click_function_json =
        serde_json::to_string(&chatgpt_web::build_upload_menu_item_click_function()).unwrap();
    let scope_file_input_function_json =
        serde_json::to_string(&chatgpt_web::build_scope_composer_file_input_function()).unwrap();
    let composer_file_input_marker_json =
        serde_json::to_string(chatgpt_web::COMPOSER_FILE_INPUT_MARKER).unwrap();
    let attachment_probe_function_json = bundle_file_name.map(|file_name| {
        serde_json::to_string(&chatgpt_web::build_attachment_probe_function(file_name).unwrap())
            .unwrap()
    });
    format!(
        r##"
const PAGE_NAME = {page_name_json};
const FILE_UPLOAD_PATH = {file_upload_path_json};
const UPLOAD_TIMEOUT_MS = {upload_timeout_ms};
const DELIVERY_TEXT = {delivery_text_json};
const PROMPT = {prompt_json};
const BUNDLE_FILE_NAME = {bundle_file_name_json};
const COMPOSER_SELECTOR = {composer_selector_json};
const SEND_BUTTON_SELECTOR = {send_button_selector_json};
const STOP_BUTTON_SELECTOR = {stop_button_selector_json};
const SEND_CLICK_FUNCTION_SOURCE = {send_click_function_json};
const OPEN_ATTACHMENT_UI_FUNCTION_SOURCE = {open_attachment_ui_function_json};
const UPLOAD_MENU_ITEM_CLICK_FUNCTION_SOURCE = {upload_menu_item_click_function_json};
const SCOPE_FILE_INPUT_FUNCTION_SOURCE = {scope_file_input_function_json};
const COMPOSER_FILE_INPUT_MARKER = {composer_file_input_marker_json};
const ATTACHMENT_PROBE_FUNCTION_SOURCE = {attachment_probe_function_json};
const CHATGPT_UPLOAD_STABLE_POLLS = {upload_stable_polls};
const page = await browser.getPage(PAGE_NAME);
let warning = null;
const runPageFunction = async (functionSource) => await page.evaluate((source) => {{
  const fn = eval("(" + source + ")");
  return fn();
}}, functionSource);
const waitForScopedFileInput = async () => {{
  const deadline = Date.now() + 15000;
  let lastState = null;
  while (Date.now() < deadline) {{
    lastState = await runPageFunction(SCOPE_FILE_INPUT_FUNCTION_SOURCE);
    if (lastState?.status === "marked") {{
      return {{ ok: true, state: lastState }};
    }}
    await page.waitForTimeout(250);
  }}
  return {{ ok: false, state: lastState }};
}};
const markedFileInputSelector = () => `input[type='file'][title='${{COMPOSER_FILE_INPUT_MARKER}}']`;
const trySetInputFiles = async () => {{
  const attempts = [];
  for (const selector of [markedFileInputSelector()]) {{
    try {{
      await page.setInputFiles(selector, FILE_UPLOAD_PATH);
      return {{ ok: true, selector, attempts }};
    }} catch (error) {{
      attempts.push({{ selector, error: String(error?.message || error) }});
    }}
  }}
  return {{ ok: false, attempts }};
}};
const waitForAttachmentReady = async () => {{
  if (!ATTACHMENT_PROBE_FUNCTION_SOURCE) return true;
  const deadline = Date.now() + UPLOAD_TIMEOUT_MS;
  while (Date.now() < deadline) {{
    const state = await page.evaluate((functionSource) => {{
      const fn = eval("(" + functionSource + ")");
      return fn();
    }}, ATTACHMENT_PROBE_FUNCTION_SOURCE);
    if (state?.status === "failed") {{
      throw new Error("file attachment upload failed: " + JSON.stringify(state));
    }}
    if (state?.status === "done" && Number(state?.stableReadyCount || 0) >= CHATGPT_UPLOAD_STABLE_POLLS) {{
      return true;
    }}
    await page.waitForTimeout(500);
  }}
  return false;
}};
const composer = page.locator(COMPOSER_SELECTOR).first();
if (FILE_UPLOAD_PATH !== null) {{
  await composer.waitFor({{ state: "visible", timeout: 15000 }});
  let inputState = await waitForScopedFileInput();
  if (!inputState.ok) {{
    await runPageFunction(OPEN_ATTACHMENT_UI_FUNCTION_SOURCE);
    await page.waitForTimeout(300);
    inputState = await waitForScopedFileInput();
  }}
  if (!inputState.ok) {{
    await runPageFunction(UPLOAD_MENU_ITEM_CLICK_FUNCTION_SOURCE);
    await page.waitForTimeout(300);
    inputState = await waitForScopedFileInput();
  }}
  let uploadResult = await trySetInputFiles();
  if (!uploadResult.ok) {{
    await runPageFunction(UPLOAD_MENU_ITEM_CLICK_FUNCTION_SOURCE);
    await page.waitForTimeout(300);
    inputState = await waitForScopedFileInput();
    uploadResult = await trySetInputFiles();
  }}
  if (!uploadResult.ok) {{
    throw new Error("could not set ChatGPT upload input files: " + JSON.stringify({{
      scopeState: inputState.state || null,
      attempts: uploadResult.attempts || [],
    }}));
  }}
  const attached = await waitForAttachmentReady();
  if (!attached) {{
    throw new Error("file attachment did not finish uploading after setInputFiles");
  }}
}}
await composer.waitFor({{ state: "visible", timeout: 15000 }});
await composer.click();
await composer.pressSequentially(DELIVERY_TEXT, {{ delay: 15 }});
const readSendState = async () => await page.evaluate((composerSelector, sendSelector, bundleFileName) => {{
  const COMPOSER_SELECTOR = composerSelector;
{visibility_helpers}
{composer_scope_helpers}
  const {{ composerEl, roots }} = getComposerScope();
  const send = roots.flatMap((root) => Array.from(root.querySelectorAll(sendSelector))).find((button) => isVisible(button)) || null;
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
  if (sendState.sendButtonPresent && !sendState.sendDisabled) break;
  await page.waitForTimeout(500);
  sendState = await readSendState();
}}
if (!sendState.sendButtonPresent || sendState.sendDisabled) {{
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
    const COMPOSER_SELECTOR = composerSelector;
{visibility_helpers}
{composer_scope_helpers}
{turn_root_helpers}
    const {{ composerEl, roots }} = getComposerScope();
    const send = roots.flatMap((root) => Array.from(root.querySelectorAll(sendSelector))).find((button) => isVisible(button)) || null;
    const assistantCount = document.querySelectorAll("[data-message-author-role='assistant']").length;
    const lastAssistant = Array.from(document.querySelectorAll("[data-message-author-role='assistant']")).at(-1) || null;
    const turnRoot = latestAssistantTurn(lastAssistant);
    const stopButton = (turnRoot ? findVisible(turnRoot, stopSelector) : null) ||
      (!turnRoot && findVisible(document, stopSelector));
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
        file_upload_path_json = file_upload_path_json,
        upload_timeout_ms = upload_timeout_ms,
        delivery_text_json = delivery_text_json,
        prompt_json = prompt_json,
        bundle_file_name_json = bundle_file_name_json,
        composer_selector_json = composer_selector_json,
        send_button_selector_json = send_button_selector_json,
        stop_button_selector_json = stop_button_selector_json,
        send_click_function_json = send_click_function_json,
        open_attachment_ui_function_json = open_attachment_ui_function_json,
        upload_menu_item_click_function_json = upload_menu_item_click_function_json,
        scope_file_input_function_json = scope_file_input_function_json,
        composer_file_input_marker_json = composer_file_input_marker_json,
        attachment_probe_function_json =
            attachment_probe_function_json.unwrap_or_else(|| "null".to_string()),
        upload_stable_polls = chatgpt_web::CHATGPT_UPLOAD_STABLE_POLLS,
        visibility_helpers = chatgpt_web::JS_VISIBILITY_HELPERS,
        composer_scope_helpers = chatgpt_web::JS_COMPOSER_SCOPE_HELPERS,
        turn_root_helpers = chatgpt_web::JS_TURN_ROOT_HELPERS,
    )
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
  const state = await page.evaluate((baselineCount, baselineLastLen, allowEmptyResponse) => {{
{visibility_helpers}
{turn_root_helpers}
      const errorEl = document.querySelector("[role='alert'], [data-testid*='error']");
    const allAssistantMessages = Array.from(document.querySelectorAll("[data-message-author-role='assistant']"));
    const assistantMessages = allAssistantMessages.slice(baselineCount);
    const lastAssistantMessage = allAssistantMessages.length > 0 ? allAssistantMessages[allAssistantMessages.length - 1] : null;
    const turnRoot = latestAssistantTurn(lastAssistantMessage);
    const stopButton = (turnRoot ? findVisible(turnRoot, "[data-testid='stop-button']") : null) ||
      (!turnRoot ? findVisible(document, "[data-testid='stop-button']") : null);
    const thinkingSelector = ".result-thinking, [data-testid*='thinking']";
    const hasThinkingIndicator = !!((turnRoot ? findVisible(turnRoot, thinkingSelector) : null) ||
      (!turnRoot ? findVisible(document, thinkingSelector) : null));
    const lastAssistantLen = (lastAssistantMessage?.innerText || "").length;
    const newAssistantCount = assistantMessages.length;
    const newMessage = allAssistantMessages.length > baselineCount;
    const sameMessageGrew = allAssistantMessages.length === baselineCount && lastAssistantLen > baselineLastLen;
      const response = (newMessage
        ? assistantMessages.map((message) => message.innerText).join("\n---\n")
        : (sameMessageGrew ? (lastAssistantMessage?.innerText || "") : "")
      ).trim();
      const responseReady =
        !stopButton &&
        !hasThinkingIndicator &&
        ((newMessage && (allowEmptyResponse || response.length > 0)) || sameMessageGrew);
      return {{
      error: errorEl ? errorEl.innerText.slice(0, 200).trim() : null,
      hasThinkingIndicator,
      hasStopButton: !!stopButton,
      newAssistantCount,
      assistantCount: allAssistantMessages.length,
      lastAssistantLen,
      newMessage,
      sameMessageGrew,
        responseLength: response.length,
        responseTail: response.slice(-256),
        response: responseReady ? response : "",
      }};
  }}, BASELINE_COUNT, BASELINE_LAST_LEN, ALLOW_EMPTY_RESPONSE);
  if (state.error) {{
    console.log(JSON.stringify({{ status: "error", error: state.error }}));
    return;
  }}
  const completionCandidate =
      !state.hasStopButton &&
      !state.hasThinkingIndicator &&
      ((state.newMessage && (ALLOW_EMPTY_RESPONSE || state.responseLength > 0)) || state.sameMessageGrew);
    if (completionCandidate) {{
      const responseKey = `${{state.assistantCount}}:${{state.lastAssistantLen}}:${{state.responseLength}}:${{state.responseTail}}`;
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
        visibility_helpers = chatgpt_web::JS_VISIBILITY_HELPERS,
        turn_root_helpers = chatgpt_web::JS_TURN_ROOT_HELPERS,
    )
}

fn build_claude_prepare_script(page_name: &str, run_id: &str) -> String {
    let page_name_json = serde_json::to_string(page_name).expect("page name JSON");
    let marked_url_json = serde_json::to_string(
        &claude_web::mark_claude_url(run_id).expect("validated Claude run id"),
    )
    .expect("Claude URL JSON");
    let window_name_json =
        serde_json::to_string(&format!("yoetz:{run_id}")).expect("window name JSON");
    let wait_function_json = serde_json::to_string(&claude_web::build_wait_for_composer_function())
        .expect("wait function JSON");
    format!(
        r#"
const PAGE_NAME = {page_name_json};
const MARKED_URL = {marked_url_json};
const WINDOW_NAME = {window_name_json};
const WAIT_FUNCTION_SOURCE = {wait_function_json};
const page = await browser.getPage(PAGE_NAME);
await page.goto(MARKED_URL, {{ waitUntil: "domcontentloaded" }});
await page.waitForTimeout(800);
await page.evaluate((name) => {{ window.name = name; return window.name; }}, WINDOW_NAME);
const state = await page.evaluate((source) => eval("(" + source + ")")(), WAIT_FUNCTION_SOURCE);
console.log(JSON.stringify(state));
"#
    )
}

fn build_claude_model_script(page_name: &str) -> String {
    let page_name_json = serde_json::to_string(page_name).expect("page name JSON");
    let open_json = serde_json::to_string(&claude_web::build_open_model_menu_function())
        .expect("open model function JSON");
    let close_json = serde_json::to_string(&claude_web::build_close_model_menu_function())
        .expect("close model function JSON");
    let fable_json = serde_json::to_string(&claude_web::build_select_fable_function())
        .expect("Fable function JSON");
    let mark_effort_json = serde_json::to_string(&claude_web::build_mark_effort_parent_function())
        .expect("mark effort function JSON");
    let max_json =
        serde_json::to_string(&claude_web::build_select_max_function()).expect("Max function JSON");
    let thinking_json = serde_json::to_string(&claude_web::build_ensure_thinking_on_function())
        .expect("Thinking function JSON");
    let verify_json =
        serde_json::to_string(&claude_web::build_verify_fable_max_thinking_function())
            .expect("verification function JSON");
    let effort_selector_json =
        serde_json::to_string(&format!("[title='{}']", claude_web::EFFORT_HOVER_MARKER))
            .expect("effort selector JSON");
    format!(
        r#"
const PAGE_NAME = {page_name_json};
const OPEN = {open_json};
const CLOSE = {close_json};
const SELECT_FABLE = {fable_json};
const MARK_EFFORT = {mark_effort_json};
const SELECT_MAX = {max_json};
const ENABLE_THINKING = {thinking_json};
const VERIFY = {verify_json};
const EFFORT_SELECTOR = {effort_selector_json};
const page = await browser.getPage(PAGE_NAME);
let state = await page.evaluate((source) => eval("(" + source + ")")(), OPEN);
if (!['opened', 'opening'].includes(state?.status)) {{
  console.log(JSON.stringify({{ status: 'error', error: 'Claude model selector unavailable', diagnostics: state }}));
  return;
}}
await page.waitForTimeout(250);
state = await page.evaluate((source) => eval("(" + source + ")")(), SELECT_FABLE);
if (state?.status !== 'selected') {{
  console.log(JSON.stringify({{ status: 'unavailable', error: 'Fable 5 is unavailable', diagnostics: state }}));
  return;
}}
await page.waitForTimeout(300);
await page.evaluate((source) => eval("(" + source + ")")(), OPEN);
state = await page.evaluate((source) => eval("(" + source + ")")(), MARK_EFFORT);
if (state?.status !== 'marked') {{
  console.log(JSON.stringify({{ status: 'unavailable', error: 'Claude Effort menu unavailable', diagnostics: state }}));
  return;
}}
await page.locator(EFFORT_SELECTOR).first().hover();
await page.waitForTimeout(400);
state = await page.evaluate((source) => eval("(" + source + ")")(), SELECT_MAX);
if (state?.status !== 'selected') {{
  console.log(JSON.stringify({{ status: 'unavailable', error: 'Claude Max effort unavailable', diagnostics: state }}));
  return;
}}
await page.waitForTimeout(300);
await page.evaluate((source) => eval("(" + source + ")")(), OPEN);
await page.evaluate((source) => eval("(" + source + ")")(), MARK_EFFORT);
await page.locator(EFFORT_SELECTOR).first().hover();
await page.waitForTimeout(400);
state = await page.evaluate((source) => eval("(" + source + ")")(), ENABLE_THINKING);
if (!['already_on', 'clicked'].includes(state?.status)) {{
  console.log(JSON.stringify({{ status: 'unavailable', error: 'Claude Thinking switch unavailable', diagnostics: state }}));
  return;
}}
await page.waitForTimeout(300);
await page.evaluate((source) => eval("(" + source + ")")(), CLOSE);
await page.waitForTimeout(250);
await page.evaluate((source) => eval("(" + source + ")")(), OPEN);
state = await page.evaluate((source) => eval("(" + source + ")")(), MARK_EFFORT);
if (state?.status !== 'marked') {{
  console.log(JSON.stringify({{ status: 'unavailable', error: 'Claude Effort menu unavailable during verification', diagnostics: state }}));
  return;
}}
await page.locator(EFFORT_SELECTOR).first().hover();
await page.waitForTimeout(400);
const verification = await page.evaluate((source) => eval("(" + source + ")")(), VERIFY);
await page.evaluate((source) => eval("(" + source + ")")(), CLOSE);
console.log(JSON.stringify(verification));
"#
    )
}

fn build_claude_delivery_script(
    page_name: &str,
    delivery_text: &str,
    bundle_file_name: Option<&str>,
    use_clipboard: bool,
    upload_timeout_ms: u64,
    send_timeout_ms: u64,
) -> String {
    let page_name_json = serde_json::to_string(page_name).expect("page name JSON");
    let delivery_text_json = serde_json::to_string(delivery_text).expect("delivery text JSON");
    let bundle_name_json = serde_json::to_string(&bundle_file_name).expect("bundle file name JSON");
    let composer_json =
        serde_json::to_string(claude_web::COMPOSER_SELECTOR).expect("composer selector JSON");
    let send_json =
        serde_json::to_string(&claude_web::build_send_function()).expect("send function JSON");
    let attachment_json = bundle_file_name
        .map(claude_web::build_attachment_probe_function)
        .transpose()
        .expect("attachment probe")
        .map(|source| serde_json::to_string(&source).expect("attachment probe JSON"))
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"
const PAGE_NAME = {page_name_json};
const DELIVERY_TEXT = {delivery_text_json};
const BUNDLE_FILE_NAME = {bundle_name_json};
const USE_CLIPBOARD = {use_clipboard};
const UPLOAD_TIMEOUT_MS = {upload_timeout_ms};
const SEND_TIMEOUT_MS = {send_timeout_ms};
const COMPOSER_SELECTOR = {composer_json};
const SEND_FUNCTION_SOURCE = {send_json};
const ATTACHMENT_FUNCTION_SOURCE = {attachment_json};
const page = await browser.getPage(PAGE_NAME);
const composer = page.locator(COMPOSER_SELECTOR).first();
await composer.waitFor({{ state: "visible", timeout: 20000 }});
await composer.click();
if (USE_CLIPBOARD) {{
  await page.keyboard.press("Meta+V");
  const deadline = Date.now() + UPLOAD_TIMEOUT_MS;
  let ready = false;
  let lastState = null;
  while (Date.now() < deadline) {{
    lastState = ATTACHMENT_FUNCTION_SOURCE
      ? await page.evaluate((source) => eval("(" + source + ")")(), ATTACHMENT_FUNCTION_SOURCE)
      : null;
    const composerLength = await composer.evaluate((el) => String(el.innerText || el.textContent || '').trim().length);
    if (lastState?.status === 'candidate' || lastState?.pastedContent || composerLength > 0) {{ ready = true; break; }}
    await page.waitForTimeout(250);
  }}
  if (!ready) {{
    console.log(JSON.stringify({{ status: 'error', phase: 'upload', error: 'Claude clipboard paste did not become inline content or a pasted content attachment', diagnostics: lastState }}));
    return;
  }}
  await composer.pressSequentially("\n\n" + DELIVERY_TEXT, {{ delay: 5 }});
}} else {{
  await composer.pressSequentially(DELIVERY_TEXT, {{ delay: 1 }});
}}
const enableDeadline = Date.now() + SEND_TIMEOUT_MS;
let send = null;
while (Date.now() < enableDeadline) {{
  send = await page.evaluate((source) => eval("(" + source + ")")(), SEND_FUNCTION_SOURCE);
  if (send?.status === 'sent') break;
  if (!['disabled', 'missing'].includes(send?.status)) break;
  await page.waitForTimeout(250);
}}
if (send?.status !== 'sent') {{
  console.log(JSON.stringify({{ status: 'error', phase: 'send', error: 'Claude send button did not become enabled', diagnostics: send }}));
  return;
}}
console.log(JSON.stringify({{
  status: 'sent',
  assistantCountBeforeSend: send.assistantCount || 0,
  assistantLastLenBeforeSend: send.assistantLastLength || 0,
  copyButtonsBeforeSend: send.copyButtons || 0,
  bundleFileName: BUNDLE_FILE_NAME,
}}));
"#
    )
}

fn build_claude_poll_script(
    page_name: &str,
    assistant_count_before_send: i64,
    assistant_last_len_before_send: i64,
    poll_settings: ChatgptPollSettings,
) -> String {
    let page_name_json = serde_json::to_string(page_name).expect("page name JSON");
    let probe_json = serde_json::to_string(&claude_web::build_response_poll_function())
        .expect("response probe JSON");
    let stable_idle_threshold_ms = claude_web::stable_idle_threshold_ms(poll_settings.interval_ms);
    format!(
        r#"
const PAGE_NAME = {page_name_json};
const BASELINE_COUNT = {assistant_count_before_send};
const BASELINE_LENGTH = {assistant_last_len_before_send};
const POLL_TIMEOUT_MS = {poll_timeout_ms};
const POLL_INTERVAL_MS = {poll_interval_ms};
const STABLE_IDLE_THRESHOLD_MS = {stable_idle_threshold_ms};
const NO_PROGRESS_POLL_LIMIT = {no_progress_limit};
const PROBE_FUNCTION_SOURCE = {probe_json};
const page = await browser.getPage(PAGE_NAME);
const started = Date.now();
let stableSince = null;
let stableKey = null;
let noProgressPolls = 0;
while (Date.now() - started < POLL_TIMEOUT_MS) {{
  await page.waitForTimeout(POLL_INTERVAL_MS);
  const state = await page.evaluate((source) => eval("(" + source + ")")(), PROBE_FUNCTION_SOURCE);
  if (state?.error) {{
    console.log(JSON.stringify({{ status: 'error', error: 'Claude page error: ' + state.error }}));
    return;
  }}
  const inactive = !state?.streaming && !state?.hasStopButton && !state?.thinking;
  const grew = Number(state?.count || 0) > BASELINE_COUNT ||
    (Number(state?.count || 0) === BASELINE_COUNT && Number(state?.length || 0) > BASELINE_LENGTH);
  const candidate = inactive && grew && Number(state?.length || 0) > 0;
  if (inactive && !grew) noProgressPolls += 1; else noProgressPolls = 0;
  if (noProgressPolls >= NO_PROGRESS_POLL_LIMIT) {{
    console.log(JSON.stringify({{ status: 'error', error: 'Claude response made no post-send progress after generation became idle', diagnostics: state }}));
    return;
  }}
  if (!candidate) {{ stableSince = null; stableKey = null; continue; }}
  const key = `${{state.count}}:${{state.length}}:${{String(state.text || '').slice(-256)}}`;
  if (stableKey !== key) {{ stableKey = key; stableSince = Date.now(); continue; }}
  if (stableSince !== null && Date.now() - stableSince >= STABLE_IDLE_THRESHOLD_MS) {{
    console.log(JSON.stringify({{
      status: 'ok', response: state.text, url: await page.url(),
      stable_for_ms: Date.now() - stableSince,
      stable_idle_threshold_ms: STABLE_IDLE_THRESHOLD_MS,
    }}));
    return;
  }}
}}
console.log(JSON.stringify({{ status: 'timeout', error: `Claude response timed out after ${{POLL_TIMEOUT_MS}}ms` }}));
"#,
        poll_timeout_ms = poll_settings.timeout_ms,
        poll_interval_ms = poll_settings.interval_ms,
        no_progress_limit = CLAUDE_NO_PROGRESS_POLL_LIMIT,
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

    let result = (|| -> Result<(
        String,
        Vec<String>,
        Option<String>,
        chatgpt_recipe::ChatgptModelSelectionStatus,
    )> {
        let mut warnings = Vec::new();
        let file_upload_path = if ctx.paste_mode {
            None
        } else if let Some(bundle_path) = &ctx.bundle_path {
            let canonical_path = bundle_path
                .canonicalize()
                .with_context(|| format!("resolve bundle path: {}", bundle_path.display()))?;
            Some(
                canonical_path
                    .to_str()
                    .ok_or_else(|| {
                        anyhow!(
                            "bundle path is not valid UTF-8: {}",
                            canonical_path.display()
                        )
                    })?
                    .to_string(),
            )
        } else if ctx.bundle_text.is_some() {
            return Err(anyhow!(
                "dev-browser file upload requires a bundle path on disk; use `--var paste=true` for text-only delivery"
            ));
        } else {
            None
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

        let prepare_script = build_chatgpt_prepare_script(
            &page_name,
            &ctx.model,
            ctx.model_strategy,
            &ctx.run_id,
        );
        let prepare_stdout = {
            let attach_attempt_lock = browser::acquire_attach_attempt_lock()?;
            if ctx.show_approval_guidance {
                if attach_attempt_lock.waited() {
                    eprintln!(
                        "info: another yoetz process is already starting a Chrome attach attempt; waiting for it to finish before trying the dev-browser transport"
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
        let model_selection = prepare
            .model_selection
            .clone()
            .unwrap_or_else(|| serde_json::json!({"status": "unknown"}));
        let model_used = prepare
            .model_selection
            .as_ref()
            .and_then(|selection| chatgpt_web::select_reported_chatgpt_model(selection, &ctx.model));
        let model_selection_status =
            chatgpt_web::chatgpt_model_selection_status(&model_selection, &ctx.model);
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
        if model_selection_status != chatgpt_recipe::ChatgptModelSelectionStatus::Selected {
            return Err(anyhow!(
                "ChatGPT did not provide verified GPT-5.6 Sol + Pro selection proof: {}",
                model_selection
            ));
        }

        let send_script = build_chatgpt_send_script(
            &page_name,
            &ctx.prompt,
            &delivery_text,
            file_upload_path.as_deref(),
            ctx.upload_timeout_ms,
            file_upload_path
                .as_deref()
                .map(Path::new)
                .and_then(|path| path.file_name())
                .and_then(|value| value.to_str()),
        );
        let send_stdout = run_script(
            &send_script,
            Some(chatgpt_script_timeout_secs(ctx.upload_timeout_ms)),
        )
        .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Upload)?;
        let send: ChatgptSendResult = parse_script_json("parse chatgpt send result", &send_stdout)?;
        match send.status.as_str() {
            "sent" => {}
            "error" => {
                let detail = send
                    .error
                    .unwrap_or_else(|| "ChatGPT send phase failed".to_string());
                let phase = chatgpt_recipe::classify_terminal_fallback_phase_message(&detail)
                    .unwrap_or(chatgpt_recipe::ChatgptTransportPhase::Send);
                return Err(anyhow!("{detail}")).with_chatgpt_phase(phase);
            }
            other => {
                return Err(anyhow!("unexpected ChatGPT send status `{other}`"))
                    .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Send);
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
        let poll_stdout =
            poll_result.with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::WaitResponse)?;
        let (response, mut poll_warnings) =
            parse_chatgpt_recipe_result(&poll_stdout, ctx.poll_settings.timeout_ms)
                .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::WaitResponse)?;
        eprintln!(
            "info: ChatGPT response completed after {}",
            format_chatgpt_wait_duration(wait_started_at.elapsed())
        );
        warnings.append(&mut poll_warnings);
        Ok((response, warnings, model_used, model_selection_status))
    })();

    let (response, warnings, model_used, model_selection_status) = result?;
    for warning in &warnings {
        eprintln!("warn: {warning}");
    }
    Ok(ChatgptRecipeRunResult {
        response,
        model_used,
        model_selection_status,
        warnings,
    })
}

#[cfg(target_os = "macos")]
fn stage_macos_clipboard_from_file(path: &Path) -> Result<()> {
    let mut bundle = fs::File::open(path)
        .with_context(|| format!("open Claude bundle for clipboard: {}", path.display()))?;
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("stage Claude bundle on the macOS clipboard")?;
    let mut stdin = child
        .stdin
        .take()
        .context("open pbcopy stdin for Claude bundle")?;
    std::io::copy(&mut bundle, &mut stdin).context("write Claude bundle to the macOS clipboard")?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .context("wait for macOS clipboard staging")?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "could not stage Claude bundle on the macOS clipboard: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn claude_delivery_text(
    bundle_path: &Path,
    prompt: &str,
    warnings: &mut Vec<String>,
) -> Result<(String, bool)> {
    #[cfg(target_os = "macos")]
    {
        stage_macos_clipboard_from_file(bundle_path)?;
        warnings.push(
            "dev-browser delivered the Claude bundle through the macOS clipboard because the QuickJS transport cannot drive a file input; claude.ai may convert large pastes into a pasted content attachment."
                .to_string(),
        );
        Ok((prompt.to_string(), true))
    }

    #[cfg(not(target_os = "macos"))]
    {
        let bundle = fs::read_to_string(bundle_path).with_context(|| {
            format!(
                "read Claude bundle {} for dev-browser inline delivery",
                bundle_path.display()
            )
        })?;
        warnings.push(
            "dev-browser cannot drive Claude file inputs on this platform; the bundle was inserted inline and may be slow or exceed composer limits. Use chrome-devtools-mcp or chrome-extension-native for file upload."
                .to_string(),
        );
        Ok((format!("{prompt}\n\n{bundle}"), false))
    }
}

fn parse_claude_poll_result(stdout: &str, timeout_ms: u64) -> Result<(String, Option<String>)> {
    let value: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("parse Claude dev-browser response result: {stdout}"))?;
    match value.get("status").and_then(Value::as_str) {
        Some("ok") => {
            let response = value
                .get("response")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .context("Claude stable-idle result did not contain response text")?;
            Ok((
                response.to_string(),
                value.get("url").and_then(Value::as_str).map(str::to_string),
            ))
        }
        Some("error") => Err(anyhow!(
            "{}",
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Claude response polling failed")
        )),
        Some("timeout") => {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!("Claude response polling timed out after {timeout_ms}ms")
                });
            Err(anyhow!(message))
        }
        other => Err(anyhow!(
            "unexpected Claude dev-browser response status {other:?}: {value}"
        )),
    }
}

pub fn ensure_claude_auth_with_page_check_and_endpoint(cdp_endpoint: Option<&str>) -> Result<()> {
    let run_id = claude_web::generate_run_id();
    let script = build_claude_prepare_script(CLAUDE_AUTH_PROBE_PAGE_NAME, &run_id);
    eprintln!("info: probing Claude auth state via dev-browser");
    let stdout = run_script_connect_with_browser_and_endpoint(
        &script,
        Some(60),
        Some(CLAUDE_BROWSER_NAME),
        cdp_endpoint,
    )
    .map_err(maybe_add_dev_browser_connect_guidance)?;
    let state: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("parse Claude auth probe result: {stdout}"))?;
    match state.get("status").and_then(Value::as_str) {
        Some("ready") => Ok(()),
        Some("login") => Err(anyhow!(
            "Claude login is required in the attached Chrome profile"
        )),
        Some("challenge") => Err(anyhow!(
            "Cloudflare challenge detected on claude.ai; solve it in the attached Chrome window and retry"
        )),
        other => Err(anyhow!(
            "Claude did not finish loading the composer; status={other:?}, diagnostics={state}"
        )),
    }
}

/// Run the Claude fallback through small QuickJS scripts. Rust owns phase
/// orchestration and every script communicates through one JSON stdout value.
pub fn run_claude_recipe(ctx: &ClaudeDevBrowserRecipeContext) -> Result<ClaudeRecipeRunResult> {
    let bundle_path = ctx
        .bundle_path
        .as_deref()
        .context("Claude dev-browser transport requires `--bundle`")?;
    let bundle_file_name = bundle_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("Claude bundle path must end in a UTF-8 filename")?;
    let mut warnings = ctx.warnings.clone();
    let (delivery_text, use_clipboard) =
        claude_delivery_text(bundle_path, &ctx.prompt, &mut warnings)?;
    let page_name = format!("{}-{}", CLAUDE_RECIPE_PAGE_NAME_PREFIX, ctx.run_id);
    let cdp_endpoint = ctx.cdp_endpoint.as_deref();
    let run_script = |script: &str, timeout_secs: Option<u64>| {
        run_script_connect_with_browser_and_endpoint(
            script,
            timeout_secs,
            Some(CLAUDE_BROWSER_NAME),
            cdp_endpoint,
        )
    };

    let prepare_script = build_claude_prepare_script(&page_name, &ctx.run_id);
    let prepare_stdout = {
        let attach_attempt_lock = browser::acquire_attach_attempt_lock()?;
        if ctx.show_approval_guidance {
            if attach_attempt_lock.waited() {
                eprintln!(
                    "info: another yoetz process is already starting a Chrome attach attempt; waiting before trying Claude via dev-browser"
                );
            }
            eprintln!(
                "info: connecting to Chrome — if prompted, click Allow in Chrome's remote debugging dialog"
            );
        }
        run_script(&prepare_script, Some(60)).map_err(maybe_add_dev_browser_connect_guidance)?
    };
    let prepare: Value = serde_json::from_str(prepare_stdout.trim())
        .with_context(|| format!("parse Claude prepare result: {prepare_stdout}"))?;
    match prepare.get("status").and_then(Value::as_str) {
        Some("ready") => {}
        Some("login") => bail!("Claude login is required in the attached Chrome profile"),
        Some("challenge") => bail!(
            "Cloudflare challenge detected on claude.ai; solve it in the attached Chrome window and retry"
        ),
        other => bail!(
            "Claude composer did not become ready; status={other:?}, diagnostics={prepare}"
        ),
    }

    let model_script = build_claude_model_script(&page_name);
    let model_stdout = run_script(&model_script, Some(120))?;
    let model: Value = serde_json::from_str(model_stdout.trim())
        .with_context(|| format!("parse Claude model selection result: {model_stdout}"))?;
    let model_selection_status = claude_web::model_selection_status(&model);
    if model_selection_status != WebModelSelectionStatus::Selected {
        bail!(
            "Claude exact model contract is unavailable or mismatched; required Fable 5 + Max + Thinking on; diagnostics={model}"
        );
    }

    let delivery_script = build_claude_delivery_script(
        &page_name,
        &delivery_text,
        Some(bundle_file_name),
        use_clipboard,
        ctx.upload_timeout_ms,
        ctx.send_timeout_ms,
    );
    let delivery_stdout = run_script(
        &delivery_script,
        Some(chatgpt_script_timeout_secs(
            ctx.upload_timeout_ms.saturating_add(ctx.send_timeout_ms),
        )),
    )
    .with_claude_phase(WebRecipeTransportPhase::Upload)?;
    let delivery: Value = serde_json::from_str(delivery_stdout.trim())
        .with_context(|| format!("parse Claude delivery result: {delivery_stdout}"))?;
    if delivery.get("status").and_then(Value::as_str) != Some("sent") {
        let error = anyhow!(
            "{}; diagnostics={delivery}",
            delivery
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Claude delivery failed")
        );
        return if delivery.get("phase").and_then(Value::as_str) == Some("send") {
            Err(error).with_claude_phase(WebRecipeTransportPhase::Send)
        } else {
            Err(error).with_claude_phase(WebRecipeTransportPhase::Upload)
        };
    }
    let baseline_count = delivery
        .get("assistantCountBeforeSend")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let baseline_length = delivery
        .get("assistantLastLenBeforeSend")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let poll_script = build_claude_poll_script(
        &page_name,
        baseline_count,
        baseline_length,
        ctx.poll_settings,
    );
    let poll_stdout = run_script(
        &poll_script,
        Some(chatgpt_script_timeout_secs(ctx.poll_settings.timeout_ms)),
    )
    .with_claude_phase(WebRecipeTransportPhase::WaitResponse)?;
    let (response, conversation_url) =
        parse_claude_poll_result(&poll_stdout, ctx.poll_settings.timeout_ms)
            .with_claude_phase(WebRecipeTransportPhase::WaitResponse)?;
    let conversation = conversation_url
        .as_deref()
        .and_then(|url| claude_web::normalize_conversation(url).ok());
    for warning in &warnings {
        eprintln!("warn: {warning}");
    }
    Ok(ClaudeRecipeRunResult {
        response,
        model_used: Some(claude_recipe::CLAUDE_REPORTED_MODEL.to_string()),
        model_selection_status,
        warnings,
        conversation_id: conversation.as_ref().map(|value| value.id.clone()),
        conversation_url: conversation.map(|value| value.url),
        used_clipboard: use_clipboard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};

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
    fn resolve_chatgpt_upload_timeout_ms_accepts_recipe_var() {
        let vars = BTreeMap::from([("upload_timeout_ms".to_string(), "180000".to_string())]);

        assert_eq!(
            resolve_chatgpt_upload_timeout_ms(&vars, None).unwrap(),
            180_000
        );
        assert_eq!(
            resolve_chatgpt_upload_timeout_ms(&BTreeMap::new(), None).unwrap(),
            CHATGPT_UPLOAD_TIMEOUT_MS_DEFAULT
        );
    }

    #[test]
    fn resolve_chatgpt_upload_timeout_ms_scales_with_bundle_size() {
        let dir =
            env::temp_dir().join(format!("yoetz-upload-timeout-scale-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bundle_path = dir.join("large-bundle.md");
        fs::write(&bundle_path, vec![b'x'; 3 * 1024 * 1024 + 1]).unwrap();

        let timeout =
            resolve_chatgpt_upload_timeout_ms(&BTreeMap::new(), Some(&bundle_path)).unwrap();

        assert_eq!(timeout, CHATGPT_UPLOAD_TIMEOUT_MS_DEFAULT + 20_000);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn chatgpt_script_timeout_secs_adds_grace_window() {
        assert_eq!(chatgpt_script_timeout_secs(900_000), 960);
    }

    #[test]
    fn looks_like_dev_browser_connect_failure_matches_connect_timeout() {
        let err =
            anyhow!("browser.newPage: Timeout 30000ms exceeded while waiting for connectOverCDP");
        assert!(is_dev_browser_connect_failure(&err));
        assert!(should_retry_dev_browser_connect_failure(&err));

        let without_timeout = anyhow!(
            "browserType.connectOverCDP: connection closed while waiting for connectOverCDP"
        );
        assert!(is_dev_browser_connect_failure(&without_timeout));
        assert!(should_retry_dev_browser_connect_failure(&without_timeout));

        let auto_attach_hang =
            anyhow!("browserType.connectOverCDP: Target.setAutoAttach timed out after 30000ms");
        assert!(is_dev_browser_connect_failure(&auto_attach_hang));
        assert!(!should_retry_dev_browser_connect_failure(&auto_attach_hang));

        let other = anyhow!("ChatGPT response timed out after 900000ms");
        assert!(!is_dev_browser_connect_failure(&other));
        assert!(!should_retry_dev_browser_connect_failure(&other));
    }

    #[test]
    fn is_dev_browser_connect_failure_matches_target_gettargets_token_alone() {
        let err = anyhow!("Timed out after 5000ms during Target.getTargets");
        assert!(is_dev_browser_connect_failure(&err));
        assert!(should_retry_dev_browser_connect_failure(&err));
    }

    #[test]
    fn is_dev_browser_connect_failure_matches_initializing_live_cdp_browser_token_alone() {
        let err = anyhow!("Timed out after 5000ms initializing live CDP browser");
        assert!(is_dev_browser_connect_failure(&err));
        assert!(should_retry_dev_browser_connect_failure(&err));
    }

    #[test]
    fn is_dev_browser_connect_failure_matches_initializing_live_cdp_targets_token_alone() {
        let err =
            anyhow!("Timed out after 5000ms initializing live CDP targets during Page.enable");
        assert!(is_dev_browser_connect_failure(&err));
        assert!(should_retry_dev_browser_connect_failure(&err));
    }

    #[test]
    fn is_dev_browser_connect_failure_matches_remote_debugging_consent_token_alone() {
        let err =
            anyhow!("Timed out after 5000ms; Chrome may be waiting for remote-debugging consent");
        assert!(is_dev_browser_connect_failure(&err));
        assert!(
            !should_retry_dev_browser_connect_failure(&err),
            "consent waits must not retry — would re-trigger the Allow popup"
        );
    }

    #[test]
    fn is_dev_browser_connect_failure_requires_connection_failure_gate() {
        let hint_without_timeout = anyhow!("Chrome may be waiting for remote-debugging consent");
        assert!(!is_dev_browser_connect_failure(&hint_without_timeout));
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
    #[serial_test::serial]
    fn live_cdp_disable_falls_through_to_external_dev_browser_resolution() {
        let _live_disabled = EnvVarGuard::set(
            crate::live_cdp_daemon::YOETZ_LIVE_CDP_DAEMON_ENV,
            OsStr::new("0"),
        );
        let _external_missing = EnvVarGuard::set(
            "YOETZ_DEV_BROWSER_BIN",
            OsStr::new("/definitely/missing/dev-browser"),
        );

        let err = run_script_connect_with_endpoint("console.log('ok')", Some(1), None)
            .expect_err("disabled bundled daemon should fall through to external dev-browser");
        let message = format!("{err:#}");
        assert!(message.contains("YOETZ_DEV_BROWSER_BIN points to"));
        assert!(!message.contains("yoetz live-CDP daemon is disabled"));
    }

    #[test]
    fn build_chatgpt_prepare_script_uses_named_page_and_login_check() {
        let script = build_chatgpt_prepare_script(
            "yoetz-chatgpt-test",
            crate::chatgpt_recipe::CHATGPT_SOL_PRO_MODEL,
            crate::chatgpt_recipe::ChatgptModelStrategy::Select,
            "run-123",
        );

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const MODEL = \"gpt-5-6-sol-pro\";"));
        assert!(script.contains("const MODEL_STRATEGY = \"select\";"));
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
        assert!(script.contains("classList.contains(\\\"__composer-pill\\\")"));
        assert!(script.contains("familyStatus"));
        assert!(script.contains("effortStatus"));
        assert!(script.contains(
            "requested model '\" + (selection?.requested || MODEL || \"\") + \"' was not selected"
        ));
        assert!(script.contains("let selectedModel = null;"));
        assert!(script.contains("let modelSelection = null;"));
        assert!(script.contains("modelSelection = selection || null;"));
        assert!(!script.contains("modelSelectionStatus ="));
        assert!(script.contains("selectedModel ="));
        assert!(script.contains("selectedModel = selection?.modelUsed ?? null;"));
        assert!(script.contains("modelUsed: selectedModel,"));
        assert!(script.contains("modelSelection,"));
        assert!(script.contains("bodyText"));
        assert!(script.contains(
            "status: !loggedIn ? \"login_required\" : composerReady ? \"ready\" : \"not_ready\""
        ));
    }

    #[test]
    fn build_chatgpt_prepare_script_marks_a_yoetz_owned_tab() {
        let script = build_chatgpt_prepare_script(
            "yoetz-chatgpt-test",
            "pro",
            crate::chatgpt_recipe::ChatgptModelStrategy::Select,
            "run-456",
        );
        assert!(script.contains("const MARKED_URL = \"https://chatgpt.com/?_yoetz=run-456\";"));
        assert!(script.contains("const WINDOW_NAME = \"yoetz:run-456\";"));
        assert!(
            script.contains("await page.goto(MARKED_URL, { waitUntil: \"domcontentloaded\" });")
        );
        assert!(script.contains("window.name = name;"));
    }

    #[test]
    fn build_chatgpt_prepare_script_current_skips_picker_selection() {
        let script = build_chatgpt_prepare_script(
            "yoetz-chatgpt-test",
            "current",
            crate::chatgpt_recipe::ChatgptModelStrategy::Current,
            "run-789",
        );
        assert!(script.contains("const MODEL = \"current\";"));
        assert!(script.contains("const MODEL_STRATEGY = \"current\";"));
        assert!(script.contains(r#"MODEL_STRATEGY === "current""#));
        assert!(script.contains(r#"selectedModel = selection?.modelUsed ?? "";"#));
        assert!(script.contains("model pinning bypassed — answer may come from any model"));
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
    fn claude_recipe_uses_named_page_json_micro_scripts() {
        let prepare = build_claude_prepare_script("yoetz-claude-test", "20260718T102228Z_ab12cd");
        assert!(prepare.contains("const PAGE_NAME = \"yoetz-claude-test\";"));
        assert!(prepare.contains("await browser.getPage(PAGE_NAME)"));
        assert!(prepare.contains("console.log(JSON.stringify"));
        assert!(!prepare.contains("require("));
        assert!(!prepare.contains("fetch("));

        let model = build_claude_model_script("yoetz-claude-test");
        assert!(model.contains("Fable 5"));
        assert!(model.contains("effort-option-max"));
        assert!(model.contains("aria-checked"));
        assert!(model.contains(".hover()"));
        assert!(model.contains("Effort menu unavailable during verification"));

        let delivery = build_claude_delivery_script(
            "yoetz-claude-test",
            "Review this bundle.",
            Some("bundle.md"),
            true,
            120_000,
            120_000,
        );
        assert!(delivery.contains("Meta+V"));
        assert!(delivery.contains("pasted content"));
        assert!(delivery.contains("pressSequentially"));

        let poll = build_claude_poll_script(
            "yoetz-claude-test",
            2,
            100,
            ChatgptPollSettings {
                timeout_ms: 540_000,
                interval_ms: 30_000,
            },
        );
        assert!(poll.contains("STABLE_IDLE_THRESHOLD_MS"));
        assert!(poll.contains("console.log(JSON.stringify"));
    }

    #[test]
    fn build_chatgpt_send_script_uses_file_input_upload_and_press_sequentially() {
        let script = build_chatgpt_send_script(
            "yoetz-chatgpt-test",
            "Review this file.",
            "Review this file.",
            Some("/tmp/bundle.txt"),
            180_000,
            Some("bundle.txt"),
        );

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const FILE_UPLOAD_PATH = \"/tmp/bundle.txt\";"));
        assert!(script.contains("if (FILE_UPLOAD_PATH !== null)"));
        assert!(script.contains("const UPLOAD_TIMEOUT_MS = 180000;"));
        assert!(!script.contains("DISABLE_EXTENDED"));
        assert!(script.contains("await composer.waitFor({ state: \"visible\", timeout: 15000 });"));
        assert!(script.contains("const COMPOSER_FILE_INPUT_MARKER = \"yoetz-upload-target\";"));
        assert!(script.contains("for (const selector of [markedFileInputSelector()])"));
        assert!(!script.contains("#upload-files"));
        assert!(script.contains("await page.setInputFiles(selector, FILE_UPLOAD_PATH);"));
        assert!(script.contains("could not set ChatGPT upload input files"));
        assert!(script.contains("file attachment did not finish uploading after setInputFiles"));
        assert!(script.contains("const OPEN_ATTACHMENT_UI_FUNCTION_SOURCE ="));
        assert!(script.contains("const UPLOAD_MENU_ITEM_CLICK_FUNCTION_SOURCE ="));
        assert!(script.contains("const SCOPE_FILE_INPUT_FUNCTION_SOURCE ="));
        assert!(script.contains("const ATTACHMENT_PROBE_FUNCTION_SOURCE ="));
        assert!(script.contains("const SEND_CLICK_FUNCTION_SOURCE ="));
        assert!(script.contains("assistantLastLenBeforeSend"));
        assert!(script.contains("pressSequentially(DELIVERY_TEXT, { delay: 15 })"));
        assert!(script.contains("status: \"sent\""));
        assert!(!script.contains("Meta+v"));
    }

    #[test]
    fn parse_script_json_reads_prepare_result() {
        let result: ChatgptPrepareResult = parse_script_json(
            "prepare",
            r#"{"status":"ready","loggedIn":true,"composerReady":true,"modelUsed":"GPT-5.6 Sol Pro","modelSelection":{"status":"selected","requested":"gpt-5-6-sol-pro","modelUsed":"GPT-5.6 Sol Pro","familyStatus":"verified","effortStatus":"verified"},"url":"https://chatgpt.com/","title":"ChatGPT","bodyText":"Send a message"}"#,
        )
        .unwrap();

        assert_eq!(result.status, "ready");
        assert!(result.logged_in);
        assert!(result.composer_ready);
        assert_eq!(
            result
                .model_selection
                .as_ref()
                .and_then(|selection| selection.get("status"))
                .and_then(Value::as_str),
            Some("selected")
        );
        assert_eq!(
            result
                .model_selection
                .as_ref()
                .and_then(|selection| selection.get("modelUsed"))
                .and_then(Value::as_str),
            Some("GPT-5.6 Sol Pro")
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
        assert!(script.contains("allowEmptyResponse || response.length > 0"));
        assert!(script.contains("BASELINE_COUNT, BASELINE_LAST_LEN, ALLOW_EMPTY_RESPONSE"));
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
                timeout_ms: 5_400_000,
                interval_ms: 30_000,
            },
        );
        assert!(message.contains("elapsed 1m 35s"));
        assert!(message.contains("timeout 1h 30m"));
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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            #[allow(unsafe_code)]
            unsafe {
                if let Some(previous) = &self.previous {
                    env::set_var(self.key, previous);
                } else {
                    env::remove_var(self.key);
                }
            }
        }
    }
}

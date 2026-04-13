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
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(test)]
use yoetz_core::paths::home_dir;

use crate::browser;

/// Cached dev-browser resolution.
static DEV_BROWSER: OnceLock<String> = OnceLock::new();

/// Default timeout for dev-browser scripts in seconds.
const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 30;
const DEV_BROWSER_ATTACH_TO_OTHER_ENV: &str = "PW_CHROMIUM_ATTACH_TO_OTHER";
const DEV_BROWSER_PARENT_TIMEOUT_GRACE_SECS: u64 = 20;
const DEV_BROWSER_WAIT_POLL_MS: u64 = 100;

/// Extended timeout for ChatGPT response polling (30 minutes by default).
const CHATGPT_POLL_TIMEOUT_MS_DEFAULT: u64 = 1_800_000;
const CHATGPT_POLL_INTERVAL_MS_DEFAULT: u64 = 30_000;
const CHATGPT_BROWSER_NAME: &str = "yoetz-chatgpt";
const CHATGPT_PAGE_NAME: &str = "yoetz-chatgpt-main";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatgptPollSettings {
    pub timeout_ms: u64,
    pub interval_ms: u64,
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
    // dev-browser doesn't support --version (exits 2). Use --help which
    // exits 0 and is universally supported.
    Command::new(bin)
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
    find_dev_browser()?.ok_or_else(|| {
        anyhow!("dev-browser not found in PATH. Install manually: npm i -g dev-browser")
    })
}

/// Returns true if dev-browser is already available without side effects.
pub fn is_available() -> bool {
    find_dev_browser().is_ok_and(|bin| bin.is_some())
}

/// Ensure dev-browser is installed, auto-installing if needed.
pub fn ensure_installed() -> Result<()> {
    if find_dev_browser()?.is_some() {
        return Ok(());
    }

    eprintln!("info: dev-browser not found, installing via npm...");
    let output = Command::new("npm")
        .args(["install", "-g", "dev-browser"])
        .output()
        .context("failed to run npm. Install dev-browser manually: npm i -g dev-browser")?;

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
        return Err(anyhow!(
            "npm install -g dev-browser failed: {detail}. Install manually: npm i -g dev-browser"
        ));
    }

    // After install, re-run full discovery (PATH + npm prefix fallback).
    if let Some(bin) = find_dev_browser()? {
        let _ = DEV_BROWSER.set(bin);
        eprintln!("info: dev-browser installed successfully");
        return Ok(());
    }
    let hint = if cfg!(target_os = "macos") {
        " On Homebrew Node, npm may not create PATH symlinks. \
         Fix: ln -sf \"$(npm prefix -g)/lib/node_modules/dev-browser/bin/dev-browser-darwin-\"* \"$(npm prefix -g)/bin/dev-browser\""
    } else {
        ""
    };
    Err(anyhow!(
        "dev-browser installed but not discoverable in PATH or npm prefix.{hint}"
    ))
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
        "http" | "https" | "ws" | "wss" => Ok(Some(endpoint.to_string())),
        other => Err(anyhow!(
            "unsupported Chrome CDP endpoint scheme `{other}` in `{endpoint}`"
        )),
    }
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
    match run_script_connect_with_endpoint(script, Some(10), cdp_endpoint) {
        Ok(stdout) if stdout.trim().starts_with("ok:") => Ok(()),
        Ok(stdout) => Err(anyhow!("dev-browser connection check failed: {stdout}")),
        Err(first_err) => {
            let msg = format!("{first_err:#}");
            let is_timeout = msg.contains("Timeout") || msg.contains("timed out");
            if !is_timeout {
                return Err(first_err.context("dev-browser connection check failed"));
            }
            eprintln!("info: dev-browser connection timed out, retrying with longer timeout");
            std::thread::sleep(std::time::Duration::from_secs(2));
            let stdout = run_script_connect_with_endpoint(script, Some(45), cdp_endpoint)
                .context("dev-browser connection check failed after retry")?;
            if stdout.trim().starts_with("ok:") {
                Ok(())
            } else {
                Err(anyhow!("dev-browser connection check failed: {stdout}"))
            }
        }
    }
}

pub fn check_chatgpt_auth_with_endpoint(cdp_endpoint: Option<&str>) -> Result<bool> {
    let script = r#"
const page = await browser.newPage();
try {
    await page.goto("https://chatgpt.com/");
    await page.waitForTimeout(3000);
    // Check for the composer textarea — the most reliable auth marker.
    // Text markers like "new chat" / "send a message" drift with UI updates.
    const authenticated = await page.evaluate(() => {
        if (document.querySelector('#prompt-textarea')) return true;
        const text = document.body.innerText.toLowerCase();
        return text.includes("send a message") || text.includes("ask anything")
            || text.includes("what are you working on") || text.includes("new chat");
    });
    console.log(JSON.stringify({ authenticated }));
} finally {
    await page.close().catch(() => {});
}
"#;
    let stdout = run_script_connect_with_endpoint(script, Some(30), cdp_endpoint)?;
    let result: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("check_chatgpt_auth: malformed script output: {stdout}"))?;
    result["authenticated"]
        .as_bool()
        .ok_or_else(|| anyhow!("check_chatgpt_auth: missing 'authenticated' field in: {stdout}"))
}

/// Ensure the connected Chrome session can reach an authenticated ChatGPT page.
/// Opens a temporary page — use only when not immediately followed by a recipe.
pub fn ensure_chatgpt_auth_with_page_check_and_endpoint(cdp_endpoint: Option<&str>) -> Result<()> {
    eprintln!("info: probing Chrome reachability via dev-browser");
    check_connection_with_endpoint(cdp_endpoint)
        .map_err(maybe_add_dev_browser_connect_guidance)
        .context(
            "dev-browser cannot connect to Chrome. Enable remote debugging: chrome://inspect/#remote-debugging",
        )?;

    eprintln!("info: probing ChatGPT auth state via dev-browser");
    if check_chatgpt_auth_with_endpoint(cdp_endpoint)? {
        return Ok(());
    }

    Err(anyhow!(
        "chatgpt login required in the attached Chrome session. Log in there and try again."
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

fn looks_like_dev_browser_connect_failure(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_lowercase();
    (message.contains("connectovercdp")
        || message.contains("auto-connect")
        || message.contains("auto connect")
        || message.contains("could not connect to chrome")
        || message.contains("browser.getversion"))
        && (message.contains("timed out") || message.contains("timeout"))
}

fn maybe_add_dev_browser_connect_guidance(err: anyhow::Error) -> anyhow::Error {
    if looks_like_dev_browser_connect_failure(&err) {
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

#[derive(Debug, serde::Deserialize)]
struct ChatgptPrepareResult {
    status: String,
    #[serde(rename = "loggedIn")]
    logged_in: bool,
    #[serde(rename = "composerReady")]
    composer_ready: bool,
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct ChatgptSendResult {
    status: String,
    error: Option<String>,
    #[serde(rename = "assistantCountBeforeSend")]
    assistant_count_before_send: Option<usize>,
    warning: Option<String>,
}

fn parse_script_json<T: serde::de::DeserializeOwned>(label: &str, stdout: &str) -> Result<T> {
    serde_json::from_str(stdout.trim())
        .with_context(|| format!("{label}: malformed script output: {stdout}"))
}

fn build_chatgpt_prepare_script(page_name: &str, model: &str) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    let model_json = serde_json::to_string(model).unwrap();
    format!(
        r##"
const PAGE_NAME = {page_name_json};
const MODEL = {model_json};
const page = await browser.getPage(PAGE_NAME);
const CHATGPT_URL = "https://chatgpt.com/";
const COMPOSER_SELECTOR = "#prompt-textarea, [role='textbox']";
const ASSISTANT_SELECTOR = "[data-message-author-role='assistant']";
const NEW_CHAT_SELECTOR = "[data-testid='new-chat-button']";
await page.goto(CHATGPT_URL, {{ waitUntil: "domcontentloaded" }});
await page.waitForTimeout(800);
const loggedIn = (await page.locator("[data-testid='login-button']").first().count()) === 0;
// QuickJS: keep this helper small and let Rust own the broader orchestration.
// Playwright's page.evaluate accepts exactly one arg after the function, so
// the selectors are passed as a single object.
const readState = async () => await page.evaluate(({{ composerSelector, assistantSelector }}) => {{
  const composer = document.querySelector(composerSelector);
  const assistantCount = document.querySelectorAll(assistantSelector).length;
  const pathname = window.location.pathname || "/";
  return {{
    url: window.location.href,
    pathname,
    onConversationPath: pathname.startsWith("/c/"),
    assistantCount,
    composerVisible: !!composer,
  }};
}}, {{ composerSelector: COMPOSER_SELECTOR, assistantSelector: ASSISTANT_SELECTOR }});
let state = await readState();
let composerReady =
  loggedIn &&
  state.composerVisible &&
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
    composerReady =
      state.composerVisible &&
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
if (loggedIn && composerReady && MODEL) {{
  const modelBtn = page.locator("[data-testid='model-switcher-dropdown-button'], button[aria-label='Model selector']").first();
  await modelBtn.waitFor({{ state: "visible", timeout: 5000 }});
  await modelBtn.click({{ timeout: 5000 }});
  const slug = MODEL.toLowerCase();
  const byTestId = page.locator(`[data-testid="model-switcher-${{slug}}"]`).first();
  if (await byTestId.count() > 0) {{
    await byTestId.click({{ timeout: 5000 }});
  }} else {{
    const menuItem = page.locator("[role='menuitem']").filter({{ hasText: new RegExp(slug.includes("thinking") ? "thinking" : slug.includes("pro") ? "pro" : slug.includes("instant") || slug.includes("5-3") ? "instant" : slug, "i") }}).first();
    await menuItem.waitFor({{ state: "visible", timeout: 5000 }});
    await menuItem.click({{ timeout: 5000 }});
  }}
  await page.waitForTimeout(500);
}}
console.log(JSON.stringify({{
  status: !loggedIn ? "login_required" : composerReady ? "ready" : "not_ready",
  loggedIn,
  composerReady,
  url: state.url,
}}));
"##,
        page_name_json = page_name_json,
        model_json = model_json,
    )
}

fn build_chatgpt_send_script(
    page_name: &str,
    prompt: &str,
    delivery_text: &str,
    file_on_clipboard: bool,
    disable_extended: bool,
) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    let file_on_clipboard_json = serde_json::to_string(&file_on_clipboard).unwrap();
    let delivery_text_json = serde_json::to_string(delivery_text).unwrap();
    let prompt_json = serde_json::to_string(prompt).unwrap();
    format!(
        r##"
const PAGE_NAME = {page_name_json};
const FILE_ON_CLIPBOARD = {file_on_clipboard_json};
const DELIVERY_TEXT = {delivery_text_json};
const PROMPT = {prompt_json};
const DISABLE_EXTENDED = {disable_extended};
const page = await browser.getPage(PAGE_NAME);
let warning = null;
if (DISABLE_EXTENDED) {{
  const extButton = page.locator("button[aria-label*='click to remove'][aria-label*='Extended'], button[aria-label*='remove'][aria-label*='Extended']").first();
  if (await extButton.count() > 0) {{
    await extButton.click();
    await page.waitForTimeout(500);
  }} else {{
    warning = "extended disable requested but toggle not found";
  }}
}}
const composer = page.locator("[role='textbox']").first();
if (FILE_ON_CLIPBOARD) {{
  await composer.waitFor({{ state: "visible", timeout: 15000 }});
  await composer.click();
  await page.keyboard.press("Meta+v");
  await page.waitForTimeout(5000);
  const attached = await page.evaluate(() => {{
    return !!document.querySelector("[class*='file-tile'], [data-testid*='attachment']");
  }});
  if (!attached) {{
    throw new Error("file not attached after clipboard paste");
  }}
}}
await composer.waitFor({{ state: "visible", timeout: 15000 }});
await composer.click();
await composer.pressSequentially(DELIVERY_TEXT, {{ delay: 15 }});
const sendBtn = page.locator("[data-testid='send-button']").first();
const readSendState = async () => await page.evaluate(() => {{
  const send = document.querySelector("[data-testid='send-button']");
  const composerEl = document.querySelector("#prompt-textarea, [role='textbox']");
  return {{
    sendButtonPresent: !!send,
    sendDisabled: send ? !!send.disabled : false,
    composerTextLength: (composerEl?.innerText || composerEl?.textContent || "").trim().length,
    attachmentPresent: !!document.querySelector("[class*='file-tile'], [data-testid*='attachment']"),
  }};
}});
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
const assistantCountBeforeSend = await page.locator("[data-message-author-role='assistant']").count();
await sendBtn.click();
const transitionDeadline = Date.now() + 10000;
let transitionState = null;
while (Date.now() < transitionDeadline) {{
  transitionState = await page.evaluate((baselineCount) => {{
    const send = document.querySelector("[data-testid='send-button']");
    const stopButton = document.querySelector("[data-testid='stop-button']");
    const assistantCount = document.querySelectorAll("[data-message-author-role='assistant']").length;
    const composerEl = document.querySelector("#prompt-textarea, [role='textbox']");
    const composerText = (composerEl?.innerText || composerEl?.textContent || "").trim();
    const attachmentPresent = !!document.querySelector("[class*='file-tile'], [data-testid*='attachment']");
    return {{
      sendButtonPresent: !!send,
      sendDisabled: send ? !!send.disabled : false,
      stopButtonPresent: !!stopButton,
      assistantCount,
      composerTextLength: composerText.length,
      attachmentPresent,
      transitionObserved:
        !!stopButton ||
        assistantCount > baselineCount ||
        !send ||
        (!!send && !!send.disabled) ||
        composerText.length === 0,
    }};
  }}, assistantCountBeforeSend);
  if (transitionState.transitionObserved) break;
  await page.waitForTimeout(500);
}}
if (!transitionState || !transitionState.transitionObserved) {{
  console.log(JSON.stringify({{
    status: "error",
    error: "ChatGPT send click did not trigger a UI transition within 10s. " + JSON.stringify(transitionState || {{}}),
    assistantCountBeforeSend,
    warning,
  }}));
  return;
}}
console.log(JSON.stringify({{
        status: "sent",
        assistantCountBeforeSend,
        warning,
}}));
"##,
        page_name_json = page_name_json,
        file_on_clipboard_json = file_on_clipboard_json,
        delivery_text_json = delivery_text_json,
        prompt_json = prompt_json,
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
    poll_settings: ChatgptPollSettings,
    allow_empty_response: bool,
) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    format!(
        r#"
const PAGE_NAME = {page_name_json};
const BASELINE = {assistant_count_before_send};
const POLL_TIMEOUT_MS = {poll_timeout_ms};
const POLL_INTERVAL_MS = {poll_interval_ms};
const ALLOW_EMPTY_RESPONSE = {allow_empty_response};
const page = await browser.getPage(PAGE_NAME);
const start = Date.now();
let lastResponseText = null;
while (Date.now() - start < POLL_TIMEOUT_MS) {{
  const state = await page.evaluate((baselineCount) => {{
    const errorEl = document.querySelector("[role='alert'], [data-testid*='error']");
    const hasThinkingIndicator = !!document.querySelector(".result-thinking, [data-testid*='thinking']");
    const assistantMessages = Array.from(document.querySelectorAll("[data-message-author-role='assistant']")).slice(baselineCount);
    const response = assistantMessages.map((message) => message.innerText).join("\n---\n").trim();
    return {{
      error: errorEl ? errorEl.innerText.slice(0, 200).trim() : null,
      hasThinkingIndicator,
      hasStopButton: !!document.querySelector("[data-testid='stop-button']"),
      newAssistantCount: assistantMessages.length,
      response,
    }};
  }}, BASELINE);
  if (state.error) {{
    console.log(JSON.stringify({{ status: "error", error: state.error }}));
    return;
  }}
  const completionCandidate =
    !state.hasStopButton &&
    !state.hasThinkingIndicator &&
    state.newAssistantCount > 0 &&
    (ALLOW_EMPTY_RESPONSE || state.response.length > 0);
  if (completionCandidate) {{
    if (lastResponseText !== null && state.response === lastResponseText) {{
      console.log(JSON.stringify({{ status: "ok", response: state.response }}));
      return;
    }}
    lastResponseText = state.response;
  }} else {{
    lastResponseText = null;
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
        poll_timeout_ms = poll_settings.timeout_ms,
        poll_interval_ms = poll_settings.interval_ms,
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
pub fn run_chatgpt_recipe(ctx: &DevBrowserRecipeContext) -> Result<String> {
    let recipe_lock = browser::acquire_chatgpt_recipe_lock()?;
    let browser_name = CHATGPT_BROWSER_NAME.to_string();
    let page_name = CHATGPT_PAGE_NAME.to_string();
    let cdp_endpoint = ctx.cdp_endpoint.as_deref();
    let run_script = |script: &str, timeout_secs: Option<u64>| {
        run_script_connect_with_browser_and_endpoint(
            script,
            timeout_secs,
            Some(browser_name.as_str()),
            cdp_endpoint,
        )
    };
    if ctx.show_approval_guidance && recipe_lock.waited() {
        eprintln!(
            "info: another yoetz process is already using the shared ChatGPT dev-browser page; waiting for it to finish before reusing the tab"
        );
    }

    let result = (|| -> Result<(String, Vec<String>)> {
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

        let prepare_script = build_chatgpt_prepare_script(&page_name, &ctx.model);
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
        match prepare.status.as_str() {
            "ready" if prepare.logged_in && prepare.composer_ready => {}
            "login_required" => {
                return Err(anyhow!(
                    "chatgpt login required in the attached Chrome session. Log in there and try again."
                ));
            }
            "not_ready" => {
                return Err(anyhow!(
                    "ChatGPT did not finish loading the composer on {}. Restart Chrome with chrome://inspect/#remote-debugging enabled and try again.",
                    prepare.url
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
            ctx.poll_settings,
            ctx.allow_empty_response,
        );
        let poll_stdout = run_script(
            &poll_script,
            Some(chatgpt_script_timeout_secs(ctx.poll_settings.timeout_ms)),
        )?;
        let (response, mut poll_warnings) =
            parse_chatgpt_recipe_result(&poll_stdout, ctx.poll_settings.timeout_ms)?;
        warnings.append(&mut poll_warnings);
        Ok((response, warnings))
    })();

    let (response, warnings) = result?;
    for warning in warnings {
        eprintln!("warn: {warning}");
    }
    Ok(response)
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
        assert!(looks_like_dev_browser_connect_failure(&err));

        let other = anyhow!("ChatGPT response timed out after 900000ms");
        assert!(!looks_like_dev_browser_connect_failure(&other));
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
    }

    #[test]
    fn build_chatgpt_prepare_script_uses_named_page_and_login_check() {
        let script = build_chatgpt_prepare_script("yoetz-chatgpt-test", "gpt-5-4-pro");

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const MODEL = \"gpt-5-4-pro\";"));
        assert!(script.contains("const page = await browser.getPage(PAGE_NAME);"));
        assert!(
            script.contains("await page.goto(CHATGPT_URL, { waitUntil: \"domcontentloaded\" });")
        );
        assert!(script.contains("[data-testid='login-button']"));
        assert!(script.contains("const NEW_CHAT_SELECTOR = \"[data-testid='new-chat-button']\";"));
        assert!(script.contains("const canUseNewChat = attempt === 0;"));
        assert!(script.contains("await page.reload({ waitUntil: \"domcontentloaded\" });"));
        assert!(script.contains("page.evaluate(({ composerSelector, assistantSelector })"));
        assert!(!script.contains("page.evaluate((composerSelector, assistantSelector)"));
        assert!(script.contains("state.assistantCount === 0"));
        assert!(script.contains("pathname.startsWith(\"/c/\")"));
        assert!(script.contains(
            "status: !loggedIn ? \"login_required\" : composerReady ? \"ready\" : \"not_ready\""
        ));
    }

    #[test]
    fn chatgpt_recipe_uses_stable_browser_and_page_names() {
        assert_eq!(CHATGPT_BROWSER_NAME, "yoetz-chatgpt");
        assert_eq!(CHATGPT_PAGE_NAME, "yoetz-chatgpt-main");
        assert!(!CHATGPT_PAGE_NAME.contains("pid"));
        assert!(!CHATGPT_PAGE_NAME.contains('_'));
    }

    #[test]
    fn build_chatgpt_send_script_uses_clipboard_upload_and_press_sequentially() {
        let script = build_chatgpt_send_script(
            "yoetz-chatgpt-test",
            "Review this file.",
            "Review this file.",
            true,
            true,
        );

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const FILE_ON_CLIPBOARD = true;"));
        assert!(script.contains("await composer.waitFor({ state: \"visible\", timeout: 15000 });"));
        assert!(script.contains("await page.keyboard.press(\"Meta+v\");"));
        assert!(script.contains("file not attached after clipboard paste"));
        assert!(script.contains("[class*='file-tile'], [data-testid*='attachment']"));
        assert!(script.contains("pressSequentially(DELIVERY_TEXT, { delay: 15 })"));
        assert!(script.contains("status: \"sent\""));
    }

    #[test]
    fn parse_script_json_reads_prepare_result() {
        let result: ChatgptPrepareResult = parse_script_json(
            "prepare",
            r#"{"status":"ready","loggedIn":true,"composerReady":true,"url":"https://chatgpt.com/"}"#,
        )
        .unwrap();

        assert_eq!(result.status, "ready");
        assert!(result.logged_in);
        assert!(result.composer_ready);
    }

    #[test]
    fn build_chatgpt_poll_script_waits_for_stable_non_thinking_idle() {
        let script = build_chatgpt_poll_script(
            "yoetz-chatgpt-test",
            3,
            ChatgptPollSettings {
                timeout_ms: 900_000,
                interval_ms: 45_000,
            },
            false,
        );

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const BASELINE = 3;"));
        assert!(script.contains("const POLL_TIMEOUT_MS = 900000;"));
        assert!(script.contains("const POLL_INTERVAL_MS = 45000;"));
        assert!(script.contains("const ALLOW_EMPTY_RESPONSE = false;"));
        assert!(script.contains("let lastResponseText = null;"));
        assert!(script.contains(".result-thinking, [data-testid*='thinking']"));
        assert!(script.contains("[data-testid='stop-button']"));
        assert!(script.contains("[data-message-author-role='assistant']"));
        assert!(script.contains("!state.hasThinkingIndicator"));
        assert!(script.contains("state.response === lastResponseText"));
        assert!(script.contains("status: \"ok\""));
        assert!(script.contains("status: \"timeout\""));
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
}

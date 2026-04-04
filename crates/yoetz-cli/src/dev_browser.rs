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
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use reqwest::Url;
use yoetz_core::paths::home_dir;

use crate::browser;

/// Cached dev-browser resolution.
static DEV_BROWSER: OnceLock<String> = OnceLock::new();

/// Default timeout for dev-browser scripts in seconds.
const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 30;
const CDP_HEALTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const CHROME_EXPLICIT_REMOTE_DEBUGGING_PORT: u16 = 9222;
const CDP_HALF_STATE_MARKER: &str =
    "Chrome CDP endpoint is reachable but not responding to protocol commands";

/// Extended timeout for ChatGPT response polling (30 minutes by default).
const CHATGPT_POLL_TIMEOUT_MS_DEFAULT: u64 = 1_800_000;
const CHATGPT_POLL_INTERVAL_MS_DEFAULT: u64 = 30_000;

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct CdpCandidate {
    endpoint: String,
    source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedCdpEndpoint {
    ws_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DevToolsActivePortEntry {
    port: u16,
    ws_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CdpProbeOutcome {
    Healthy(ResolvedCdpEndpoint),
    Unavailable(String),
    Poisoned(String),
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

/// Run a dev-browser script against a live Chrome instance (auto-connect).
/// Returns the script's stdout output.
pub fn run_script_connect(script: &str, timeout_secs: Option<u64>) -> Result<String> {
    run_script_connect_with_endpoint(script, timeout_secs, None)
}

fn chrome_default_profile_cdp_guidance() -> String {
    #[cfg(target_os = "macos")]
    {
        let profile_dir = home_dir()
            .map(|home| {
                home.join("Library")
                    .join("Application Support")
                    .join("Google")
                    .join("Chrome")
            })
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "~/Library/Application Support/Google/Chrome".to_string());
        format!(
            "This is a known Chrome 136+ limitation when using the default profile.\n\
             For a reliable explicit CDP endpoint, relaunch Chrome with:\n\
               --remote-debugging-port={CHROME_EXPLICIT_REMOTE_DEBUGGING_PORT} --user-data-dir={profile_dir}\n\
             Or use Chrome for Testing."
        )
    }

    #[cfg(not(target_os = "macos"))]
    {
        format!(
        "This is a known Chrome 136+ limitation when using the default profile.\n\
         For a reliable explicit CDP endpoint, relaunch Chrome with:\n\
           --remote-debugging-port={CHROME_EXPLICIT_REMOTE_DEBUGGING_PORT} --user-data-dir=~/.config/yoetz/chrome-profile\n\
         Or use Chrome for Testing."
        )
    }
}

fn is_poisoned_cdp_error(err: &anyhow::Error) -> bool {
    err.to_string().contains(CDP_HALF_STATE_MARKER)
}

fn is_localhost_url(url: &Url) -> bool {
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
}

fn endpoint_port(endpoint: &str) -> Option<u16> {
    let url = Url::parse(endpoint).ok()?;
    url.port_or_known_default()
}

fn standard_devtools_active_port_candidates() -> Vec<PathBuf> {
    let Some(home_dir) = home_dir() else {
        return Vec::new();
    };

    #[cfg(target_os = "macos")]
    {
        return vec![
            home_dir
                .join("Library")
                .join("Application Support")
                .join("Google")
                .join("Chrome")
                .join("DevToolsActivePort"),
            home_dir
                .join("Library")
                .join("Application Support")
                .join("Google")
                .join("Chrome Canary")
                .join("DevToolsActivePort"),
            home_dir
                .join("Library")
                .join("Application Support")
                .join("Chromium")
                .join("DevToolsActivePort"),
            home_dir
                .join("Library")
                .join("Application Support")
                .join("BraveSoftware")
                .join("Brave-Browser")
                .join("DevToolsActivePort"),
        ];
    }

    #[cfg(target_os = "linux")]
    {
        return vec![
            home_dir
                .join(".config")
                .join("google-chrome")
                .join("DevToolsActivePort"),
            home_dir
                .join(".config")
                .join("chromium")
                .join("DevToolsActivePort"),
            home_dir
                .join(".config")
                .join("google-chrome-beta")
                .join("DevToolsActivePort"),
            home_dir
                .join(".config")
                .join("google-chrome-unstable")
                .join("DevToolsActivePort"),
            home_dir
                .join(".config")
                .join("BraveSoftware")
                .join("Brave-Browser")
                .join("DevToolsActivePort"),
        ];
    }

    #[cfg(target_os = "windows")]
    {
        return vec![
            home_dir
                .join("AppData")
                .join("Local")
                .join("Google")
                .join("Chrome")
                .join("User Data")
                .join("DevToolsActivePort"),
            home_dir
                .join("AppData")
                .join("Local")
                .join("Google")
                .join("Chrome Beta")
                .join("User Data")
                .join("DevToolsActivePort"),
            home_dir
                .join("AppData")
                .join("Local")
                .join("Google")
                .join("Chrome SxS")
                .join("User Data")
                .join("DevToolsActivePort"),
            home_dir
                .join("AppData")
                .join("Local")
                .join("Chromium")
                .join("User Data")
                .join("DevToolsActivePort"),
            home_dir
                .join("AppData")
                .join("Local")
                .join("BraveSoftware")
                .join("Brave-Browser")
                .join("User Data")
                .join("DevToolsActivePort"),
        ];
    }

    #[allow(unreachable_code)]
    Vec::new()
}

fn parse_devtools_active_port(contents: &str) -> Option<DevToolsActivePortEntry> {
    let lines = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let port = lines.first()?.parse::<u16>().ok()?;
    let ws_path = *lines.get(1)?;
    if port == 0 || !ws_path.starts_with("/devtools/browser/") {
        return None;
    }
    Some(DevToolsActivePortEntry {
        port,
        ws_url: format!("ws://127.0.0.1:{port}{ws_path}"),
    })
}

fn devtools_active_port_entries() -> Vec<(PathBuf, DevToolsActivePortEntry)> {
    standard_devtools_active_port_candidates()
        .into_iter()
        .filter_map(|path| {
            let contents = fs::read_to_string(&path).ok()?;
            let entry = parse_devtools_active_port(&contents)?;
            Some((path, entry))
        })
        .collect()
}

fn matching_devtools_active_port_candidate(port: u16) -> Option<CdpCandidate> {
    devtools_active_port_entries()
        .into_iter()
        .find_map(|(path, entry)| {
            (entry.port == port).then_some(CdpCandidate {
                endpoint: entry.ws_url,
                source: format!("DevToolsActivePort ({})", path.display()),
            })
        })
}

fn json_version_url(endpoint: &str) -> Result<Url> {
    let mut url = Url::parse(endpoint)
        .with_context(|| format!("invalid Chrome CDP endpoint `{endpoint}`"))?;
    match url.scheme() {
        "http" | "https" => {}
        "ws" => {
            url.set_scheme("http")
                .map_err(|_| anyhow!("invalid CDP endpoint scheme for `{endpoint}`"))?;
        }
        "wss" => {
            url.set_scheme("https")
                .map_err(|_| anyhow!("invalid CDP endpoint scheme for `{endpoint}`"))?;
        }
        other => {
            return Err(anyhow!(
                "unsupported Chrome CDP endpoint scheme `{other}` in `{endpoint}`"
            ));
        }
    }
    url.set_path("/json/version");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

#[derive(serde::Deserialize)]
struct JsonVersionResponse {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: Option<String>,
}

fn build_half_state_diagnostic(candidate: &CdpCandidate, probe_url: &str, detail: &str) -> String {
    format!(
        "{CDP_HALF_STATE_MARKER}.\n\
         Endpoint: {}\n\
         Probe: {probe_url}\n\
         Source: {}\n\
         Observed: {detail}\n\
         {}\n\
         Restart Chrome with chrome://inspect/#remote-debugging enabled, then retry auto-connect or pass the exact ws:// CDP endpoint.",
        candidate.endpoint,
        candidate.source,
        chrome_default_profile_cdp_guidance(),
    )
}

fn probe_cdp_candidate(candidate: &CdpCandidate) -> CdpProbeOutcome {
    let probe_url = match json_version_url(&candidate.endpoint) {
        Ok(url) => url,
        Err(err) => return CdpProbeOutcome::Unavailable(err.to_string()),
    };

    let client = match reqwest::blocking::Client::builder()
        .timeout(CDP_HEALTH_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => return CdpProbeOutcome::Unavailable(err.to_string()),
    };
    let response = match client
        .get(probe_url.as_str())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
    {
        Ok(response) => response,
        Err(err) => return CdpProbeOutcome::Unavailable(err.to_string()),
    };
    let status = response.status();

    // Chrome 136+ can leave localhost CDP in a half-state where DevTools is
    // listening but HTTP discovery on the real profile returns 404.
    if status == reqwest::StatusCode::NOT_FOUND && is_localhost_url(&probe_url) {
        return CdpProbeOutcome::Poisoned(build_half_state_diagnostic(
            candidate,
            probe_url.as_str(),
            "GET /json/version returned HTTP 404 from a localhost Chrome endpoint",
        ));
    }

    if !status.is_success() {
        return CdpProbeOutcome::Unavailable(format!("HTTP {} from {}", status, probe_url));
    }

    let body = match response.text() {
        Ok(body) => body,
        Err(err) => return CdpProbeOutcome::Unavailable(err.to_string()),
    };
    if body.trim().is_empty() {
        return CdpProbeOutcome::Poisoned(build_half_state_diagnostic(
            candidate,
            probe_url.as_str(),
            "GET /json/version returned an empty body",
        ));
    }

    let payload: JsonVersionResponse = match serde_json::from_str(&body) {
        Ok(payload) => payload,
        Err(err) => {
            return CdpProbeOutcome::Poisoned(build_half_state_diagnostic(
                candidate,
                probe_url.as_str(),
                &format!("GET /json/version returned invalid JSON ({err})"),
            ));
        }
    };

    let Some(ws_url) = payload
        .web_socket_debugger_url
        .filter(|url| !url.trim().is_empty())
    else {
        return CdpProbeOutcome::Poisoned(build_half_state_diagnostic(
            candidate,
            probe_url.as_str(),
            "GET /json/version did not include webSocketDebuggerUrl",
        ));
    };

    CdpProbeOutcome::Healthy(ResolvedCdpEndpoint { ws_url })
}

fn resolve_dev_browser_connect_endpoint(cdp_endpoint: Option<&str>) -> Result<Option<String>> {
    let Some(endpoint) = cdp_endpoint else {
        // Let dev-browser own auto-connect discovery. It already knows how to
        // read DevToolsActivePort without relying on /json/version.
        return Ok(None);
    };

    let url = Url::parse(endpoint)
        .with_context(|| format!("invalid Chrome CDP endpoint `{endpoint}`"))?;
    match url.scheme() {
        "ws" | "wss" => return Ok(Some(endpoint.to_string())),
        "http" | "https" => {}
        other => {
            return Err(anyhow!(
                "unsupported Chrome CDP endpoint scheme `{other}` in `{endpoint}`"
            ));
        }
    }

    let candidate = CdpCandidate {
        endpoint: endpoint.to_string(),
        source: "explicit --cdp".to_string(),
    };
    match probe_cdp_candidate(&candidate) {
        CdpProbeOutcome::Healthy(resolved) => Ok(Some(resolved.ws_url)),
        CdpProbeOutcome::Poisoned(detail) => {
            if let Some(port) = endpoint_port(endpoint) {
                if let Some(ws_candidate) = matching_devtools_active_port_candidate(port) {
                    match probe_cdp_candidate(&ws_candidate) {
                        CdpProbeOutcome::Healthy(resolved) => return Ok(Some(resolved.ws_url)),
                        CdpProbeOutcome::Poisoned(_) | CdpProbeOutcome::Unavailable(_) => {}
                    }
                }
            }
            Err(anyhow!(detail))
        }
        CdpProbeOutcome::Unavailable(detail) => Err(anyhow!(
            "dev-browser could not resolve a healthy Chrome CDP endpoint from `{endpoint}`: {detail}"
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

    let output = Command::new(&bin)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(script.as_bytes())?;
            }
            drop(child.stdin.take());
            child.wait_with_output()
        })
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

        let detail = if !stderr.is_empty() {
            stderr.to_string()
        } else if !stdout.is_empty() {
            stdout.to_string()
        } else {
            format!("exit code {:?}", output.status.code())
        };
        return Err(anyhow!("dev-browser script failed: {detail}"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
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
            if is_poisoned_cdp_error(&first_err) {
                return Err(first_err.context("dev-browser connection check failed"));
            }
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
    check_connection_with_endpoint(cdp_endpoint).context(
        "dev-browser cannot connect to Chrome. Enable remote debugging: chrome://inspect/#remote-debugging",
    )?;

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
    #[serde(rename = "assistantCountBeforeSend")]
    assistant_count_before_send: usize,
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
        r#"
const PAGE_NAME = {page_name_json};
const MODEL = {model_json};
const page = await browser.getPage(PAGE_NAME);
await page.goto("https://chatgpt.com/");
let composerReady = true;
try {{
  await page.locator("[role='textbox']").first().waitFor({{ state: "visible", timeout: 20000 }});
}} catch (_) {{
  composerReady = false;
}}
const loggedIn = (await page.locator("[data-testid='login-button']").first().count()) === 0;
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
  url: page.url(),
}}));
"#,
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
        r#"
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
const deadline = Date.now() + 30000;
while (Date.now() < deadline) {{
  if (await sendBtn.count() > 0 && await sendBtn.isEnabled()) break;
  await page.waitForTimeout(1000);
}}
if (await sendBtn.count() === 0 || !(await sendBtn.isEnabled())) {{
  throw new Error("send button did not become enabled for prompt: " + PROMPT.slice(0, 80));
}}
const assistantCountBeforeSend = await page.locator("[data-message-author-role='assistant']").count();
await sendBtn.click();
console.log(JSON.stringify({{
        status: "sent",
        assistantCountBeforeSend,
        warning,
}}));
"#,
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

fn build_chatgpt_cleanup_script(page_name: &str) -> String {
    let page_name_json = serde_json::to_string(page_name).unwrap();
    format!(
        r#"
const PAGE_NAME = {page_name_json};
const pages = await browser.listPages();
if (pages.find((page) => page.name === PAGE_NAME)) {{
    const page = await browser.getPage(PAGE_NAME);
    await page.close().catch(() => {{}});
}}
"#,
        page_name_json = page_name_json,
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
    let unique_prefix = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let browser_name = format!("yoetz-chatgpt-browser-{unique_prefix}");
    let page_name = format!("yoetz-chatgpt-page-{unique_prefix}");
    let cdp_endpoint = ctx.cdp_endpoint.as_deref();
    let run_script = |script: &str, timeout_secs: Option<u64>| {
        run_script_connect_with_browser_and_endpoint(
            script,
            timeout_secs,
            Some(browser_name.as_str()),
            cdp_endpoint,
        )
    };
    let cleanup = |cdp_endpoint| -> Result<()> {
        let cleanup_script = build_chatgpt_cleanup_script(&page_name);
        match run_script_connect_with_browser_and_endpoint(
            &cleanup_script,
            Some(15),
            Some(browser_name.as_str()),
            cdp_endpoint,
        ) {
            Ok(_) => Ok(()),
            Err(err) => {
                eprintln!("warn: failed to clean up dev-browser recipe page `{page_name}`: {err}");
                Ok(())
            }
        }
    };

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
            run_script(&prepare_script, Some(60)).map_err(|err| {
                if looks_like_dev_browser_connect_failure(&err) {
                    err.context(
                        "dev-browser could not connect to Chrome. Chrome may be showing an \"Allow remote debugging?\" dialog — click Allow in Chrome, then retry."
                    )
                } else {
                    err
                }
            })?
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
        if send.status != "sent" {
            return Err(anyhow!("unexpected ChatGPT send status `{}`", send.status));
        }
        if let Some(warning) = send.warning {
            warnings.push(warning);
        }

        let poll_script = build_chatgpt_poll_script(
            &page_name,
            send.assistant_count_before_send,
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
    cleanup(cdp_endpoint)?;

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
    fn parse_devtools_active_port_returns_browser_ws_url() {
        let entry =
            parse_devtools_active_port("9222\n/devtools/browser/test-browser-id\n").unwrap();

        assert_eq!(entry.port, 9222);
        assert_eq!(
            entry.ws_url,
            "ws://127.0.0.1:9222/devtools/browser/test-browser-id"
        );
    }

    #[test]
    fn parse_devtools_active_port_rejects_malformed_payload() {
        assert!(
            parse_devtools_active_port("9222\n/devtools/page/not-a-browser-target\n").is_none()
        );
        assert!(parse_devtools_active_port("not-a-port\n/devtools/browser/test\n").is_none());
    }

    #[test]
    fn json_version_url_normalizes_http_and_ws_endpoints() {
        assert_eq!(
            json_version_url("http://127.0.0.1:9222/devtools/browser/test")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:9222/json/version"
        );
        assert_eq!(
            json_version_url("ws://127.0.0.1:9222/devtools/browser/test")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:9222/json/version"
        );
    }

    #[test]
    fn resolve_dev_browser_connect_endpoint_skips_probing_for_auto_connect() {
        assert_eq!(resolve_dev_browser_connect_endpoint(None).unwrap(), None);
    }

    #[test]
    fn probe_cdp_candidate_marks_local_404_as_poisoned() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });

        let candidate = CdpCandidate {
            endpoint: format!("http://127.0.0.1:{}", addr.port()),
            source: "test".to_string(),
        };
        let outcome = probe_cdp_candidate(&candidate);
        server.join().unwrap();

        match outcome {
            CdpProbeOutcome::Poisoned(detail) => {
                assert!(detail.contains("HTTP 404"));
                assert!(detail.contains(CDP_HALF_STATE_MARKER));
            }
            other => panic!("expected poisoned outcome, got {other:?}"),
        }
    }

    #[test]
    fn build_chatgpt_prepare_script_uses_named_page_and_login_check() {
        let script = build_chatgpt_prepare_script("yoetz-chatgpt-test", "gpt-5-4-pro");

        assert!(script.contains("const PAGE_NAME = \"yoetz-chatgpt-test\";"));
        assert!(script.contains("const MODEL = \"gpt-5-4-pro\";"));
        assert!(script.contains("const page = await browser.getPage(PAGE_NAME);"));
        assert!(script.contains("await page.goto(\"https://chatgpt.com/\");"));
        assert!(script.contains("[data-testid='login-button']"));
        assert!(script.contains(
            "status: !loggedIn ? \"login_required\" : composerReady ? \"ready\" : \"not_ready\""
        ));
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

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
//! - File upload via native filechooser + setFiles(FilePayload)
//! - Persistent named pages across script runs
//! - Daemon-managed browser instances with auto-reconnect
//! - Single script executes batch operations (fewer IPC round-trips)

#[allow(unused_imports)]
use anyhow::{anyhow, Context, Result};
#[allow(unused_imports)]
use serde_json::{json, Value};
#[allow(unused_imports)]
use std::collections::BTreeMap;
use std::env;
use std::fs;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use yoetz_core::paths::home_dir;

/// Cached dev-browser resolution.
static DEV_BROWSER: OnceLock<String> = OnceLock::new();

/// Default timeout for dev-browser scripts in seconds.
const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 30;

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
struct StagedFileGuard {
    path: PathBuf,
}

impl StagedFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for StagedFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// dev-browser tmp directory for file staging.
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

fn npm_prefix_dev_browser_candidates(prefix: &Path, windows: bool) -> Vec<PathBuf> {
    if windows {
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
    }
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
    Err(anyhow!(
        "dev-browser installed successfully, but it is not available in PATH or npm prefix"
    ))
}

/// Run a dev-browser script against a live Chrome instance (auto-connect).
/// Returns the script's stdout output.
pub fn run_script_connect(script: &str, timeout_secs: Option<u64>) -> Result<String> {
    let bin = resolve_dev_browser()?;
    let timeout = timeout_secs.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS);

    let output = Command::new(&bin)
        .args(["--connect", "--timeout", &timeout.to_string()])
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

/// Run a dev-browser script with a managed (non-connect) browser instance.
/// Uses the named browser instance for isolation.
#[allow(dead_code)]
pub fn run_script_managed(
    script: &str,
    browser_name: &str,
    timeout_secs: Option<u64>,
) -> Result<String> {
    let bin = resolve_dev_browser()?;
    let timeout = timeout_secs.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_SECS);

    let output = Command::new(&bin)
        .args(["--browser", browser_name, "--timeout", &timeout.to_string()])
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

/// Stage a file into dev-browser's tmp directory so scripts can read it
/// via `readFile(name)`.
pub fn stage_file(name: &str, content: &str) -> Result<PathBuf> {
    let tmp_dir = dev_browser_tmp_dir();
    fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("create dev-browser tmp dir: {}", tmp_dir.display()))?;
    let path = tmp_dir.join(name);
    fs::write(&path, content).with_context(|| format!("write staged file: {}", path.display()))?;
    set_staged_file_permissions(&path)?;
    Ok(path)
}

/// Stage a binary file into dev-browser's tmp directory.
#[allow(dead_code)]
pub fn stage_file_bytes(name: &str, content: &[u8]) -> Result<PathBuf> {
    let tmp_dir = dev_browser_tmp_dir();
    fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("create dev-browser tmp dir: {}", tmp_dir.display()))?;
    let path = tmp_dir.join(name);
    fs::write(&path, content).with_context(|| format!("write staged file: {}", path.display()))?;
    set_staged_file_permissions(&path)?;
    Ok(path)
}

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

/// List all browser tabs visible to dev-browser (via --connect).
pub fn list_tabs() -> Result<Vec<TabInfo>> {
    let script = r#"
const pages = await browser.listPages();
console.log(JSON.stringify(pages));
"#;
    let stdout = run_script_connect(script, Some(10))?;
    let tabs: Vec<TabInfo> =
        serde_json::from_str(stdout.trim()).context("parse dev-browser listPages")?;
    Ok(tabs)
}

/// Tab information from dev-browser.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub name: Option<String>,
}

/// Check if Chrome is reachable and dev-browser can connect to it.
/// Uses a short first probe; retries once with a longer timeout only when the
/// first failure looks like a timeout (slow CDP handshake with many tabs).
pub fn check_connection() -> Result<()> {
    let script = r#"
const pages = await browser.listPages();
console.log("ok:" + pages.length);
"#;
    match run_script_connect(script, Some(10)) {
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
            let stdout = run_script_connect(script, Some(45))
                .context("dev-browser connection check failed after retry")?;
            if stdout.trim().starts_with("ok:") {
                Ok(())
            } else {
                Err(anyhow!("dev-browser connection check failed: {stdout}"))
            }
        }
    }
}

/// Find a ChatGPT tab in the connected browser.
/// Returns the tab ID of the best candidate.
#[allow(dead_code)]
pub fn find_chatgpt_tab() -> Result<Option<String>> {
    let tabs = list_tabs()?;
    let chatgpt_tab = tabs
        .iter()
        .find(|t| t.url.contains("chatgpt.com") && !t.url.contains("/c/"))
        .or_else(|| tabs.iter().find(|t| t.url.contains("chatgpt.com")));
    Ok(chatgpt_tab.map(|t| t.id.clone()))
}

/// Check authentication status on ChatGPT in the connected browser.
#[allow(dead_code)]
pub fn check_chatgpt_auth() -> Result<bool> {
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
    let stdout = run_script_connect(script, Some(30))?;
    let result: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("check_chatgpt_auth: malformed script output: {stdout}"))?;
    result["authenticated"]
        .as_bool()
        .ok_or_else(|| anyhow!("check_chatgpt_auth: missing 'authenticated' field in: {stdout}"))
}

/// Ensure the connected Chrome session can reach ChatGPT.
/// Only checks dev-browser daemon connectivity — opening a separate page
/// to verify auth causes interference with the recipe's named page.
/// If not authenticated, the recipe will fail with a clear error.
pub fn ensure_chatgpt_auth() -> Result<()> {
    check_connection().context(
        "dev-browser cannot connect to Chrome. Enable remote debugging: chrome://inspect/#remote-debugging",
    )?;
    // Skip page-level auth check — it opens an anonymous page to chatgpt.com
    // which interferes with the recipe's subsequent named page session.
    Ok(())
}

/// Ensure the connected Chrome session can reach an authenticated ChatGPT page.
/// Opens a temporary page — use only when not immediately followed by a recipe.
#[allow(dead_code)]
pub fn ensure_chatgpt_auth_with_page_check() -> Result<()> {
    check_connection().context(
        "dev-browser cannot connect to Chrome. Enable remote debugging: chrome://inspect/#remote-debugging",
    )?;

    if check_chatgpt_auth()? {
        return Ok(());
    }

    Err(anyhow!(
        "chatgpt login required in the attached Chrome session. Log in there and try again."
    ))
}

/// Context for running a ChatGPT recipe via dev-browser.
pub struct DevBrowserRecipeContext {
    /// Path to the bundle file on disk (will be staged to dev-browser tmp).
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

fn chatgpt_script_timeout_secs(poll_timeout_ms: u64) -> u64 {
    poll_timeout_ms.div_ceil(1000) + 60
}

fn build_chatgpt_recipe_script(ctx: &DevBrowserRecipeContext, staged_name: Option<&str>) -> String {
    let model_json = serde_json::to_string(&ctx.model).unwrap();
    let prompt_json = serde_json::to_string(&ctx.prompt).unwrap();
    // Only embed bundle text in the script when paste mode is active.
    // In attachment mode the content is staged to a file and read via
    // readFile() — embedding it would bloat the script (500KB+) and crash
    // the QuickJS WASM sandbox.
    let bundle_text_for_script = if ctx.paste_mode {
        ctx.bundle_text.as_deref().unwrap_or("")
    } else {
        ""
    };
    let bundle_text_json = serde_json::to_string(bundle_text_for_script).unwrap();
    let staged_name_json = serde_json::to_string(&staged_name.unwrap_or("")).unwrap();

    format!(
        r##"
const MODEL = {model_json};
const PROMPT = {prompt_json};
const BUNDLE_TEXT = {bundle_text_json};
const STAGED_FILE = {staged_name_json};
const PASTE_MODE = {paste_mode};
const DISABLE_EXTENDED = {disable_extended};
const POLL_TIMEOUT_MS = {poll_timeout_ms};
const POLL_INTERVAL_MS = {poll_interval_ms};
const ALLOW_EMPTY_RESPONSE = {allow_empty_response};
const warnings = [];
const COMPOSER_READY_TIMEOUT_MS = 15000;
const SEND_READY_TIMEOUT_MS = 30000;
const SEND_READY_INTERVAL_MS = 1000;

function warningSuffix() {{
    return warnings.length ? " warnings=" + warnings.join(" | ") : "";
}}

async function waitForComposerReady(page) {{
    try {{
        await page.locator('#prompt-textarea').first().waitFor({{ state: 'visible', timeout: COMPOSER_READY_TIMEOUT_MS }});
    }} catch (_) {{
        await page.locator('[role="textbox"]').first().waitFor({{ state: 'visible', timeout: COMPOSER_READY_TIMEOUT_MS }});
    }}
}}

async function getComposerLocator(page) {{
    const prompt = page.locator('#prompt-textarea').first();
    if (await prompt.count() > 0) {{
        return prompt;
    }}
    return page.locator('[role="textbox"]').first();
}}

async function collectComposerDiagnostics(page) {{
    return await page.evaluate(() => {{
        const prompt =
            document.querySelector('#prompt-textarea') ||
            document.querySelector('[role="textbox"]');
        const sendBtn = document.querySelector("button[data-testid='send-button']");
        return {{
            url: location.href,
            promptLength: (prompt ? (prompt.textContent || "") : "").replace(/\s+/g, " ").trim().length,
            attachmentCount: document.querySelectorAll(
                'img[alt*="attachment" i], [data-testid*="attachment" i], [aria-label*="attachment" i]'
            ).length,
            assistantCount: document.querySelectorAll('[data-message-author-role="assistant"]').length,
            sendState: !sendBtn ? "missing" : sendBtn.disabled ? "disabled" : "enabled",
        }};
    }});
}}

function formatDiagnostics(diag) {{
    return "url=" + diag.url +
        ", prompt_length=" + diag.promptLength +
        ", attachment_tiles=" + diag.attachmentCount +
        ", assistant_count=" + diag.assistantCount +
        ", send=" + diag.sendState;
}}

async function waitForSendButtonReady(page) {{
    const sendBtn = page.locator('button[data-testid="send-button"]').first();
    const deadline = Date.now() + SEND_READY_TIMEOUT_MS;
    let lastState = "missing";
    while (Date.now() < deadline) {{
        if (await sendBtn.count() > 0) {{
            lastState = (await sendBtn.isEnabled()) ? "enabled" : "disabled";
            if (lastState === "enabled") {{
                return;
            }}
        }} else {{
            lastState = "missing";
        }}
        await page.waitForTimeout(SEND_READY_INTERVAL_MS);
    }}
    const diagnostics = await collectComposerDiagnostics(page);
    throw new Error(
        "send button " + lastState +
        " after waiting for ChatGPT to finish processing the prompt/attachment (" +
        formatDiagnostics(diagnostics) + ")" +
        warningSuffix()
    );
}}

async function ensureFreshConversation(page) {{
    for (let attempt = 0; attempt < 2; attempt++) {{
        await page.goto("https://chatgpt.com/?_yoetz=" + Date.now());
        await waitForComposerReady(page);

        if (page.url().includes("/c/")) {{
            const newChatBtn = page.locator(
                'a[href="/"], button[data-testid="create-new-chat-button"], nav a[class*="new"]'
            ).first();
            if (await newChatBtn.count() > 0) {{
                await newChatBtn.click();
                await waitForComposerReady(page);
            }} else {{
                await page.goto("https://chatgpt.com/?_yoetz=" + Date.now());
                await waitForComposerReady(page);
            }}
        }}

        const assistantCount = await page.evaluate(() => {{
            return document.querySelectorAll('[data-message-author-role="assistant"]').length;
        }});
        if (assistantCount === 0) {{
            return;
        }}
    }}

    const diagnostics = await collectComposerDiagnostics(page);
    throw new Error(
        "failed to reach a clean ChatGPT conversation before sending (" +
        formatDiagnostics(diagnostics) + ")" +
        warningSuffix()
    );
}}

// --- Step 1: Navigate to fresh ChatGPT conversation ---
// Use browser.newPage() for a clean anonymous page. Named pages accumulate
// stale ChatGPT state (cookies/localStorage) that disables the send button.
const page = await browser.newPage();
await ensureFreshConversation(page);

// --- Step 2: Select model if provided ---
// Model selection is strict when explicitly requested.
if (MODEL) {{
    try {{
        const modelBtn = page.locator(
            '[data-testid="model-switcher-dropdown-button"], button[aria-label="Model selector"]'
        ).first();
        await modelBtn.waitFor({{ state: 'visible', timeout: 5000 }});
        await modelBtn.click({{ timeout: 5000 }});

        const slug = MODEL.toLowerCase();
        const byTestId = page.locator(`[data-testid="model-switcher-${{slug}}"]`).first();
        if (await byTestId.count() > 0) {{
            await byTestId.waitFor({{ state: 'visible', timeout: 5000 }});
            await byTestId.click({{ timeout: 5000 }});
        }} else {{
            const names = {{"gpt-5-4-pro":"pro","gpt-5-4-thinking":"thinking","gpt-5-3":"instant","pro":"pro","thinking":"thinking","instant":"instant"}};
            const target = names[slug] || slug;
            const menuItem = page.locator('[role="menuitem"]').filter({{ hasText: new RegExp(target, 'i') }}).first();
            await menuItem.waitFor({{ state: 'visible', timeout: 5000 }});
            await menuItem.click({{ timeout: 5000 }});
        }}
        await page.waitForTimeout(500);
    }} catch (e) {{
        const detail = e && typeof e.message === 'string' ? e.message : String(e);
        await page.keyboard.press('Escape').catch(() => {{}});
        await page.waitForTimeout(300);
        throw new Error("model selection failed (" + detail + ")" + warningSuffix());
    }}
}}

// --- Step 3: Disable Extended Pro if requested ---
if (DISABLE_EXTENDED) {{
    const extLocators = [
        page.locator('button[aria-label*="click to remove"][aria-label*="Extended"]').first(),
        page.locator('button[aria-label*="remove"][aria-label*="Extended"]').first(),
        page.getByRole('button', {{ name: /extended.*remove|remove.*extended/i }}).first(),
    ];
    let extendedDisabled = false;
    for (const extBtn of extLocators) {{
        if (await extBtn.count() > 0) {{
            await extBtn.click();
            await page.waitForTimeout(500);
            extendedDisabled = true;
            break;
        }}
    }}
    if (!extendedDisabled) {{
        warnings.push("extended disable requested but toggle not found");
    }}
}}

// --- Step 4: Deliver content (paste or file attachment) ---
if (PASTE_MODE) {{
    // Paste bundle text + prompt via keyboard.type — Playwright fill() and
    // execCommand both bypass ProseMirror React state, leaving send disabled.
    const composer = await getComposerLocator(page);
    await composer.click();
    await page.keyboard.type(PROMPT + "\n\n" + BUNDLE_TEXT);
    await page.waitForTimeout(500);
}} else if (STAGED_FILE) {{
    // Upload via ChatGPT's native filechooser flow. DataTransfer hacks
    // bypass React state and leave the send button disabled.
    // Path: click + button → "Add photos & files" → filechooser →
    // setFiles with FilePayload. Buffer must be constructed from a
    // Uint8Array (not a string — QuickJS Buffer.from(string) is base64).
    const fileContent = await readFile(STAGED_FILE);
    const utf8Bytes = [];
    for (const char of fileContent) {{
        const code = char.codePointAt(0);
        if (code <= 0x7F) utf8Bytes.push(code);
        else if (code <= 0x7FF) utf8Bytes.push(0xC0|(code>>6), 0x80|(code&0x3F));
        else if (code <= 0xFFFF) utf8Bytes.push(0xE0|(code>>12), 0x80|((code>>6)&0x3F), 0x80|(code&0x3F));
        else utf8Bytes.push(0xF0|(code>>18), 0x80|((code>>12)&0x3F), 0x80|((code>>6)&0x3F), 0x80|(code&0x3F));
    }}

    await page.locator('button[data-testid="composer-plus-btn"]').click();
    const addFilesMenuItem = page.getByRole('menuitem', {{ name: /add photos & files/i }}).first();
    await addFilesMenuItem.waitFor({{ state: 'visible', timeout: 5000 }});
    const [fileChooser] = await Promise.all([
        page.waitForEvent('filechooser', {{ timeout: 10000 }}),
        addFilesMenuItem.click(),
    ]);
    await fileChooser.setFiles({{
        name: "bundle.md",
        mimeType: "text/plain",
        buffer: Buffer.from(new Uint8Array(utf8Bytes)),
    }});

    // Wait for upload to start before typing into the composer.
    await page.waitForTimeout(5000);

    // Type prompt via keyboard (Playwright fill()/execCommand bypass
    // ProseMirror React state, leaving send button disabled).
    const composer = await getComposerLocator(page);
    await composer.click();
    await page.keyboard.type(PROMPT, {{ delay: 5 }});
    await page.waitForTimeout(500);
}} else {{
    // No bundle — just type the prompt
    const composer = await getComposerLocator(page);
    await composer.click();
    await page.keyboard.type(PROMPT, {{ delay: 5 }});
    await page.waitForTimeout(500);
}}

// ChatGPT keeps the send button disabled while it finishes attachment
// processing. Large bundles can take several more seconds after prompt entry.
await waitForSendButtonReady(page);

const ASSISTANT_COUNT_BEFORE_SEND = await page.evaluate(() => {{
    return document.querySelectorAll('[data-message-author-role="assistant"]').length;
}});

// --- Step 5: Click send ---
const sendBtn = page.locator('button[data-testid="send-button"]').first();
await sendBtn.click();
// After clicking send, ChatGPT navigates (SPA) from / to /c/UUID.
// Wait for navigation to settle before starting poll loop.
await page.waitForURL('**/c/**', {{ timeout: 10000 }}).catch(() => {{}});

// --- Step 6: Poll for response completion ---
// Checks: new assistant response appeared, text is stable, send idle, no thinking indicator.
const pollStart = Date.now();
let completed = false;
let pollError = null;
let idleFingerprint = null;
let lastPollState = null;

while (Date.now() - pollStart < POLL_TIMEOUT_MS) {{
    await page.waitForTimeout(POLL_INTERVAL_MS);

    const pollState = await page.evaluate((baselineCount) => {{
        // Scope error detection to actual error UI elements (toasts, alerts,
        // error containers) — scanning document.body.innerText would match
        // false positives from conversation text or page chrome.
        const errEl = document.querySelector(
            '[class*="error-toast"], [data-testid*="error"], [role="alert"]'
        );
        const errText = errEl ? errEl.innerText.substring(0, 200).toLowerCase() : "";
        const errorMarkers = ["network error", "something went wrong", "error generating",
                              "could not process", "rate limit", "too many requests",
                              "attachment failed", "upload failed"];
        const detectedError = errorMarkers.find(m => errText.includes(m)) || null;
        if (detectedError) {{
            return {{
                error: detectedError,
                url: location.href,
                sendState: "unknown",
                hasStopButton: false,
                hasThinkingIndicator: false,
                composerReady: false,
                plusButtonReady: false,
                newAssistantCount: 0,
                assistantText: ""
            }};
        }}
        const sendBtn = document.querySelector("button[data-testid='send-button']");
        const stopBtn = document.querySelector("button[data-testid='stop-button'], button[aria-label*='Stop']");
        const thinking = document.querySelector(
            '.result-thinking, [class*="result-thinking"], [data-testid*="thinking"]'
        );
        const composer =
            document.querySelector('#prompt-textarea') ||
            document.querySelector('[role="textbox"]');
        const plusBtn = document.querySelector('button[data-testid="composer-plus-btn"]');
        const assistantMessages = Array.from(
            document.querySelectorAll('[data-message-author-role="assistant"]')
        );
        const newAssistantMessages = assistantMessages.slice(baselineCount);
        const assistantText = newAssistantMessages
            .map((m) => m.innerText)
            .join('\n---\n')
            .trim();
        return {{
            error: null,
            url: location.href,
            sendState: !sendBtn ? "missing" : sendBtn.disabled ? "disabled" : "enabled",
            hasStopButton: Boolean(stopBtn),
            hasThinkingIndicator: Boolean(thinking),
            composerReady: Boolean(composer),
            plusButtonReady: Boolean(plusBtn),
            newAssistantCount: newAssistantMessages.length,
            assistantText
        }};
    }}, ASSISTANT_COUNT_BEFORE_SEND);
    lastPollState = {{
        url: pollState.url,
        sendState: pollState.sendState,
        hasStopButton: pollState.hasStopButton,
        hasThinkingIndicator: pollState.hasThinkingIndicator,
        composerReady: pollState.composerReady,
        plusButtonReady: pollState.plusButtonReady,
        newAssistantCount: pollState.newAssistantCount,
        assistantTextLength: pollState.assistantText.length,
    }};
    if (pollState.error) {{
        pollError = "ChatGPT error: " + pollState.error + warningSuffix();
        break;
    }}

    const composerIdle =
        pollState.sendState === "enabled" ||
        (pollState.sendState === "missing" && pollState.composerReady && pollState.plusButtonReady);
    const idle = composerIdle &&
        !pollState.hasStopButton &&
        !pollState.hasThinkingIndicator;
    const hasResponse = pollState.newAssistantCount > 0 &&
        (ALLOW_EMPTY_RESPONSE || pollState.assistantText.length > 0);

    if (idle && hasResponse) {{
        const fingerprint = `${{pollState.newAssistantCount}}:${{pollState.assistantText}}`;
        if (idleFingerprint === fingerprint) {{
            completed = true;
            break;
        }}
        idleFingerprint = fingerprint;
    }} else {{
        idleFingerprint = null;
    }}
}}

// --- Step 7: Extract response ---
const responseState = await page.evaluate((baselineCount) => {{
    const assistantMessages = Array.from(
        document.querySelectorAll('[data-message-author-role="assistant"]')
    );
    const newAssistantMessages = assistantMessages.slice(baselineCount);
    const responseText = newAssistantMessages.map((m) => m.innerText).join('\n---\n').trim();
    return {{
        newAssistantCount: newAssistantMessages.length,
        responseText
    }};
}}, ASSISTANT_COUNT_BEFORE_SEND);

let status = pollError ? "error" : (completed ? "ok" : "timeout");
let error = pollError || null;
if (status === "timeout") {{
    const diagnostics = await collectComposerDiagnostics(page);
    error =
        "ChatGPT response timed out after " + (Date.now() - pollStart) + "ms (" +
        "last_state=" + JSON.stringify(lastPollState || {{}}) + ", " +
        formatDiagnostics(diagnostics) + ")" +
        warningSuffix();
}} else if (status === "ok" && responseState.newAssistantCount === 0) {{
    status = "error";
    error = "ChatGPT did not produce a new assistant response" + warningSuffix();
}} else if (status === "ok" && !ALLOW_EMPTY_RESPONSE && responseState.responseText.length === 0) {{
    status = "error";
    error = "ChatGPT returned an empty response" + warningSuffix();
}}

const result = {{
    status,
    error,
    elapsed_ms: Date.now() - pollStart,
    response_length: responseState.responseText.length,
    response: responseState.responseText,
    warnings,
    assistant_message_count_before_send: ASSISTANT_COUNT_BEFORE_SEND,
    new_assistant_message_count: responseState.newAssistantCount,
}};

console.log(JSON.stringify(result));
"##,
        model_json = model_json,
        prompt_json = prompt_json,
        bundle_text_json = bundle_text_json,
        staged_name_json = staged_name_json,
        paste_mode = ctx.paste_mode,
        disable_extended = ctx.disable_extended,
        poll_timeout_ms = ctx.poll_settings.timeout_ms,
        poll_interval_ms = ctx.poll_settings.interval_ms,
        allow_empty_response = ctx.allow_empty_response,
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
/// This replaces the YAML-based chatgpt.yaml recipe with a single Playwright
/// script that handles navigation, model selection, file upload (via
/// filechooser + keyboard.type), prompt entry, and response polling.
pub fn run_chatgpt_recipe(ctx: &DevBrowserRecipeContext) -> Result<String> {
    // Stage the bundle file if provided (for attachment mode).
    // Use a unique prefix (PID + timestamp) to avoid collisions between
    // concurrent recipe runs.
    let unique_prefix = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let mut staged_file_guard = None;
    let staged_name = if !ctx.paste_mode {
        if let Some(bundle_path) = &ctx.bundle_path {
            let content = fs::read_to_string(bundle_path)
                .with_context(|| format!("read bundle: {}", bundle_path.display()))?;
            let base_name = bundle_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("bundle.txt");
            let name = format!("{unique_prefix}_{base_name}");
            let path = stage_file(&name, &content)?;
            staged_file_guard = Some(StagedFileGuard::new(path));
            Some(name)
        } else if let Some(text) = &ctx.bundle_text {
            let name = format!("{unique_prefix}_bundle.md");
            let path = stage_file(&name, text)?;
            staged_file_guard = Some(StagedFileGuard::new(path));
            Some(name)
        } else {
            None
        }
    } else {
        None
    };

    let script = build_chatgpt_recipe_script(ctx, staged_name.as_deref());

    let stdout = run_script_connect(
        &script,
        Some(chatgpt_script_timeout_secs(ctx.poll_settings.timeout_ms)),
    )?;
    drop(staged_file_guard);

    let (response, warnings) = parse_chatgpt_recipe_result(&stdout, ctx.poll_settings.timeout_ms)?;
    for warning in warnings {
        eprintln!("warn: {warning}");
    }

    Ok(response)
}

/// Run a generic dev-browser recipe from a YAML recipe file.
/// Converts each YAML step into equivalent Playwright JS and executes
/// as a single script.
#[allow(dead_code)]
pub fn run_yaml_recipe(
    recipe_steps: &[(String, Option<Vec<String>>)],
    vars: &BTreeMap<String, String>,
    bundle_path: Option<&Path>,
    timeout_secs: Option<u64>,
) -> Result<String> {
    let mut script_parts = Vec::new();
    let mut staged_files = Vec::new();

    // Open a named page for persistence
    script_parts.push("const page = await browser.getPage('yoetz-recipe');".to_string());

    for (action, args) in recipe_steps {
        let args_ref = args.as_deref().unwrap_or_default();
        match action.as_str() {
            "eval" => {
                if let Some(code) = args_ref.first() {
                    let mut interpolated = code.clone();
                    for (key, value) in vars {
                        let json_needle = format!("{{{{{key}|json}}}}");
                        interpolated = interpolated
                            .replace(&json_needle, &serde_json::to_string(value).unwrap());
                        let needle = format!("{{{{{key}}}}}");
                        interpolated = interpolated.replace(&needle, value);
                    }
                    script_parts.push(format!(
                        "await page.evaluate(() => {{ {} }});",
                        interpolated
                    ));
                }
            }
            "snapshot" => {
                script_parts.push(
                    "const snap = await page.evaluate(() => document.body.innerText); console.log(snap);".to_string()
                );
            }
            "upload" => {
                if args_ref.len() >= 2 {
                    if let Some(bp) = bundle_path {
                        let content = fs::read_to_string(bp)?;
                        let name = bp
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("upload.txt");
                        let path = stage_file(name, &content)?;
                        staged_files.push(StagedFileGuard::new(path));
                        let name_json = serde_json::to_string(name).unwrap();
                        script_parts.push(format!(
                            r#"{{
    const content = await readFile({name_json});
    await page.evaluate((args) => {{
        const [c, n] = args;
        const f = new File([c], n, {{ type: "text/plain" }});
        const dt = new DataTransfer();
        dt.items.add(f);
        const input = document.querySelector({selector_json});
        if (input) {{ input.files = dt.files; input.dispatchEvent(new Event('change', {{ bubbles: true }})); }}
    }}, [content, {name_json}]);
}}"#,
                            name_json = name_json,
                            selector_json = serde_json::to_string(&args_ref[0]).unwrap(),
                        ));
                    }
                }
            }
            _ => {
                // For unknown actions, try as page method call
                eprintln!("warn: unknown recipe action '{action}', skipping");
            }
        }
    }

    let full_script = script_parts.join("\n");
    run_script_connect(&full_script, timeout_secs)
}

/// Take a screenshot of the current page via dev-browser.
#[allow(dead_code)]
pub fn take_screenshot(name: &str) -> Result<PathBuf> {
    let name_json = serde_json::to_string(name).unwrap();
    let script = format!(
        r#"
const pages = await browser.listPages();
const chatgpt = pages.find(p => p.url.includes("chatgpt.com"));
if (chatgpt) {{
    const page = await browser.getPage(chatgpt.id);
    const buf = await page.screenshot();
    const path = await saveScreenshot(buf, {name_json});
    console.log(path);
}} else {{
    const page = await browser.newPage();
    const buf = await page.screenshot();
    const path = await saveScreenshot(buf, {name_json});
    console.log(path);
}}
"#,
    );
    let stdout = run_script_connect(&script, Some(15))?;
    Ok(PathBuf::from(stdout.trim()))
}

/// Get the text content of the current ChatGPT page.
#[allow(dead_code)]
pub fn get_chatgpt_page_text() -> Result<String> {
    let script = r#"
const pages = await browser.listPages();
const chatgpt = pages.find(p => p.url.includes("chatgpt.com"));
if (chatgpt) {
    const page = await browser.getPage(chatgpt.id);
    const text = await page.evaluate(() => document.body.innerText);
    console.log(text);
} else {
    console.log("");
}
"#;
    run_script_connect(script, Some(15))
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

    #[test]
    fn test_stage_file_bytes() {
        let data = b"binary content \x00\x01\x02";
        let path = stage_file_bytes("test_binary.bin", data).unwrap();
        assert!(path.exists());
        let content = fs::read(&path).unwrap();
        assert_eq!(content, data);
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
    fn build_chatgpt_recipe_script_uses_poll_settings_and_stable_idle_detection() {
        let script = build_chatgpt_recipe_script(
            &DevBrowserRecipeContext {
                prompt: "test prompt".to_string(),
                poll_settings: ChatgptPollSettings {
                    timeout_ms: 900_000,
                    interval_ms: 45_000,
                },
                ..Default::default()
            },
            None,
        );

        assert!(script.contains("const POLL_TIMEOUT_MS = 900000;"));
        assert!(script.contains("const POLL_INTERVAL_MS = 45000;"));
        assert!(script.contains("const ALLOW_EMPTY_RESPONSE = false;"));
        assert!(script.contains("const warnings = [];"));
        assert!(script.contains("const COMPOSER_READY_TIMEOUT_MS = 15000;"));
        assert!(script.contains("const SEND_READY_TIMEOUT_MS = 30000;"));
        assert!(script.contains("const SEND_READY_INTERVAL_MS = 1000;"));
        assert!(script.contains("function warningSuffix()"));
        assert!(script.contains("async function waitForComposerReady(page)"));
        assert!(script.contains("async function ensureFreshConversation(page)"));
        assert!(script.contains("async function waitForSendButtonReady(page)"));
        assert!(script.contains("const ASSISTANT_COUNT_BEFORE_SEND = await page.evaluate"));
        assert!(script.contains("let idleFingerprint = null;"));
        assert!(script.contains("let lastPollState = null;"));
        assert!(script.contains(
            ".result-thinking, [class*=\"result-thinking\"], [data-testid*=\"thinking\"]"
        ));
        assert!(script.contains("idleFingerprint === fingerprint"));
        assert!(!script.contains("await page.waitForTimeout(2000);\n        completed = true;"));
        assert!(script.contains(
            "\" after waiting for ChatGPT to finish processing the prompt/attachment (\""
        ));
        assert!(script.contains("newAssistantMessages = assistantMessages.slice(baselineCount)"));
        assert!(script.contains("ChatGPT returned an empty response"));
        assert!(script.contains("await ensureFreshConversation(page);"));
        assert!(script
            .contains("await page.waitForURL('**/c/**', { timeout: 10000 }).catch(() => {});"));
        // Error detection must be scoped to error UI elements, not full page body.
        // Scanning document.body.innerText causes false positives when conversation
        // text or page chrome contains error-like phrases.
        assert!(
            script.contains(r#"document.querySelector("#),
            "error detection should use querySelector on specific error elements"
        );
        assert!(
            script.contains("[role=\"alert\"]"),
            "error detection should check role=alert elements"
        );
        assert!(
            !script.contains("document.body.innerText.toLowerCase();\n        const errors"),
            "error detection must NOT scan full page body text"
        );
        // File upload uses native filechooser + setFiles(FilePayload).
        // Buffer must come from Uint8Array (not string — QuickJS base64).
        assert!(
            script.contains("waitForEvent('filechooser'")
                || script.contains("waitForEvent(\"filechooser\""),
            "file upload should use filechooser event"
        );
        assert!(
            script.contains("setFiles"),
            "file upload should use setFiles with FilePayload"
        );
        assert!(
            script.contains(
                "const addFilesMenuItem = page.getByRole('menuitem', { name: /add photos & files/i }).first();"
            ),
            "upload menu selection should resolve a role+name menu item"
        );
        assert!(
            script.contains("await addFilesMenuItem.waitFor({ state: 'visible', timeout: 5000 });"),
            "upload menu item should be awaited before click"
        );
        assert!(
            script.contains("addFilesMenuItem.click()"),
            "upload menu selection should click the awaited menu item"
        );
        assert!(
            script.contains("Buffer.from(new Uint8Array("),
            "Buffer must be constructed from Uint8Array, not string"
        );
        assert!(
            script.contains("keyboard.type"),
            "text input should use keyboard.type (not fill/execCommand)"
        );
        assert!(
            script.contains("await waitForSendButtonReady(page);"),
            "script should wait for ChatGPT to re-enable send after attachment processing"
        );
        assert!(
            script.contains("await modelBtn.waitFor({ state: 'visible', timeout: 5000 });"),
            "model selector button should use a 5s wait timeout"
        );
        assert!(
            script.contains("await menuItem.waitFor({ state: 'visible', timeout: 5000 });"),
            "menu item search should use a 5s wait timeout"
        );
        assert!(
            !script.contains("warnings.push(\"model selection failed (\" + detail + \"), using current model\");"),
            "explicit model selection should no longer degrade to a warning"
        );
        assert!(
            script.contains("warnings,"),
            "recipe result should include script warnings"
        );
        assert!(
            !script.contains("console.error(\"warn: model selection failed"),
            "model selection warnings should not be written to stderr and lost"
        );
        assert!(
            !script.contains("bodyText.includes(\"pro thinking\")"),
            "thinking detection should not rely on English body text"
        );
        assert!(
            script.contains(
                "sendState: !sendBtn ? \"missing\" : sendBtn.disabled ? \"disabled\" : \"enabled\""
            ),
            "poll state should expose explicit send state"
        );
        assert!(
            script.contains("pollState.sendState === \"enabled\""),
            "poll completion should key off the explicit send state"
        );
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
        assert_eq!(
            unix,
            vec![
                PathBuf::from("/prefix/bin/dev-browser"),
                PathBuf::from("/prefix/dev-browser"),
            ]
        );

        let windows = npm_prefix_dev_browser_candidates(Path::new(r"C:\npm"), true);
        assert_eq!(
            windows,
            vec![
                PathBuf::from(r"C:\npm/dev-browser.cmd"),
                PathBuf::from(r"C:\npm/dev-browser.exe"),
                PathBuf::from(r"C:\npm/dev-browser"),
            ]
        );
    }
}

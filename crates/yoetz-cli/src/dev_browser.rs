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
//! - File upload via readFile + DataTransfer (no filesystem access needed)
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
    Command::new(bin)
        .arg("--version")
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
    Ok(None)
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

    if !command_is_available("dev-browser") {
        return Err(anyhow!(
            "dev-browser installed successfully, but it is not available in PATH"
        ));
    }
    let _ = DEV_BROWSER.set("dev-browser".to_string());
    eprintln!("info: dev-browser installed successfully");
    Ok(())
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
pub fn check_connection() -> Result<()> {
    let script = r#"
const pages = await browser.listPages();
console.log("ok:" + pages.length);
"#;
    let stdout = run_script_connect(script, Some(10))?;
    if stdout.trim().starts_with("ok:") {
        Ok(())
    } else {
        Err(anyhow!("dev-browser connection check failed: {stdout}"))
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
    const text = await page.evaluate(() => document.body.innerText.toLowerCase());
    const authenticated = text.includes("new chat") || text.includes("send a message") || text.includes("ask anything");
    console.log(JSON.stringify({ authenticated }));
} finally {
    await page.close().catch(() => {});
}
"#;
    let stdout = run_script_connect(script, Some(30))?;
    let result: Value = serde_json::from_str(stdout.trim()).unwrap_or(json!({}));
    Ok(result["authenticated"].as_bool().unwrap_or(false))
}

/// Ensure the connected Chrome session can reach an authenticated ChatGPT page.
pub fn ensure_chatgpt_auth() -> Result<()> {
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
    let bundle_text_json =
        serde_json::to_string(&ctx.bundle_text.as_deref().unwrap_or("")).unwrap();
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

// --- Step 1: Navigate to fresh ChatGPT conversation ---
const page = await browser.newPage();
await page.goto("https://chatgpt.com/?_yoetz=" + Date.now());
await page.waitForTimeout(4000);

// Safety: if we landed on an existing conversation, force new chat
const currentUrl = page.url();
if (currentUrl.includes("/c/")) {{
    const newChatBtn = page.locator(
        'a[href="/"], button[data-testid="create-new-chat-button"], nav a[class*="new"]'
    ).first();
    if (await newChatBtn.count() > 0) {{
        await newChatBtn.click();
        await page.waitForTimeout(2000);
    }} else {{
        await page.goto("https://chatgpt.com/?_yoetz=" + Date.now());
        await page.waitForTimeout(3000);
    }}
}}

// --- Step 2: Select model if provided ---
if (MODEL) {{
    // Open model selector dropdown with Radix-compatible event sequence
    const modelBtn = page.locator(
        '[data-testid="model-switcher-dropdown-button"], button[aria-label="Model selector"]'
    ).first();
    if (await modelBtn.count() > 0) {{
        await modelBtn.click();
        await page.waitForTimeout(1200);

        // Try data-testid first, then text search
        const slug = MODEL.toLowerCase();
        const byTestId = page.locator(`[data-testid="model-switcher-${{slug}}"]`);
        if (await byTestId.count() > 0) {{
            await byTestId.click();
        }} else {{
            const names = {{"gpt-5-4-pro":"pro","gpt-5-4-thinking":"thinking","gpt-5-3":"instant","pro":"pro","thinking":"thinking","instant":"instant"}};
            const target = names[slug] || slug;
            const menuItem = page.locator('[role="menuitem"]').filter({{ hasText: new RegExp(target, 'i') }}).first();
            if (await menuItem.count() > 0) {{
                await menuItem.click();
            }} else {{
                throw new Error("model '" + slug + "' not found in dropdown");
            }}
        }}
        await page.waitForTimeout(500);
    }}
}}

// --- Step 3: Disable Extended Pro if requested ---
if (DISABLE_EXTENDED) {{
    const extBtn = page.locator('button[aria-label*="click to remove"][aria-label*="Extended"]');
    if (await extBtn.count() > 0) {{
        await extBtn.click();
        await page.waitForTimeout(500);
    }}
}}

// --- Step 4: Deliver content (paste or file attachment) ---
if (PASTE_MODE) {{
    // Paste bundle text + prompt directly into textarea
    const textarea = page.locator('#prompt-textarea');
    await textarea.click();
    const combined = PROMPT + "\n\n" + BUNDLE_TEXT;
    await textarea.fill(combined);
    await page.waitForTimeout(500);
}} else if (STAGED_FILE) {{
    // Upload file via DataTransfer API (works from dev-browser sandbox)
    const fileContent = await readFile(STAGED_FILE);

    // Trigger the file input via DataTransfer — this works even without
    // opening the attachment menu, because the hidden input is always in DOM.
    const uploaded = await page.evaluate((args) => {{
        const [content, fileName] = args;
        const file = new File([content], fileName, {{ type: "text/plain" }});
        const dataTransfer = new DataTransfer();
        dataTransfer.items.add(file);
        const input = document.getElementById('upload-files');
        if (!input) return "input_missing";
        input.files = dataTransfer.files;
        input.dispatchEvent(new Event('change', {{ bubbles: true }}));
        return "ok";
    }}, [fileContent, STAGED_FILE]);

    if (uploaded !== "ok") {{
        throw new Error("file upload input (#upload-files) not found on page");
    }}

    // Poll for upload completion on the specific attachment tile.
    let uploadDone = false;
    for (let i = 0; i < 30; i++) {{
        await page.waitForTimeout(1000);
        const done = await page.evaluate((fileName) => {{
            const tiles = Array.from(
                document.querySelectorAll('[class*="file"], [data-testid*="file"], [class*="attachment"]')
            );
            const tile = tiles.find((el) => (el.textContent || "").includes(fileName))
                || tiles.find((el) => el.querySelector('[class*="animate-spin"]'))
                || null;
            if (!tile) return "waiting";
            const spinner = tile.querySelector('[class*="animate-spin"]');
            if (!spinner) return "done";
            const target = spinner.parentElement || spinner;
            const hidden = getComputedStyle(target).display === 'none';
            return hidden ? "done" : "uploading";
        }}, STAGED_FILE);
        if (done === "done") {{ uploadDone = true; break; }}
    }}
    if (!uploadDone) {{
        throw new Error("file upload timed out after 30s — attachment may not be ready");
    }}

    // Type the prompt
    const textarea = page.locator('#prompt-textarea');
    await textarea.click();
    await textarea.fill(PROMPT);
    await page.waitForTimeout(500);
}} else {{
    // No bundle — just type the prompt
    const textarea = page.locator('#prompt-textarea');
    await textarea.click();
    await textarea.fill(PROMPT);
    await page.waitForTimeout(500);
}}

const ASSISTANT_COUNT_BEFORE_SEND = await page.evaluate(() => {{
    return document.querySelectorAll('[data-message-author-role="assistant"]').length;
}});

// --- Step 5: Click send ---
const sendState = await page.evaluate(() => {{
    const btn = document.querySelector("button[data-testid='send-button']");
    if (!btn) return "missing";
    return btn.disabled ? "disabled" : "enabled";
}});
if (sendState === "missing") {{
    throw new Error("send button not found");
}}
if (sendState === "disabled") {{
    throw new Error("send button disabled — text may not have been injected");
}}
const sendBtn = page.locator('button[data-testid="send-button"]');
await sendBtn.click();

// --- Step 6: Poll for response completion ---
// Checks: new assistant response appeared, text is stable, send idle, no thinking indicator.
const pollStart = Date.now();
let completed = false;
let pollError = null;
let idleFingerprint = null;

while (Date.now() - pollStart < POLL_TIMEOUT_MS) {{
    await page.waitForTimeout(POLL_INTERVAL_MS);

    const pollState = await page.evaluate(() => {{
        const text = document.body.innerText.toLowerCase();
        const errors = ["network error", "something went wrong", "an error occurred",
                        "could not process", "rate limit", "too many requests"];
        for (const e of errors) {{
            if (text.includes(e)) {{
                return {{
                    error: e,
                    sendIdle: false,
                    hasStopButton: false,
                    hasThinkingIndicator: false,
                    assistantText: ""
                }};
            }}
        }}
        const sendBtn = document.querySelector("button[data-testid='send-button']");
        const stopBtn = document.querySelector("button[data-testid='stop-button'], button[aria-label*='Stop']");
        const thinking = document.querySelector(
            '.result-thinking, [class*="result-thinking"], [data-testid*="thinking"]'
        );
        const assistantMessages = Array.from(
            document.querySelectorAll('[data-message-author-role="assistant"]')
        );
        const newAssistantMessages = assistantMessages.slice(ASSISTANT_COUNT_BEFORE_SEND);
        const assistantText = newAssistantMessages
            .map((m) => m.innerText)
            .join('\n---\n')
            .trim();
        return {{
            error: null,
            sendIdle: !sendBtn || !sendBtn.disabled,
            hasStopButton: Boolean(stopBtn),
            hasThinkingIndicator: Boolean(thinking) || text.includes("pro thinking"),
            newAssistantCount: newAssistantMessages.length,
            assistantText
        }};
    }});
    if (pollState.error) {{
        pollError = "ChatGPT error: " + pollState.error;
        break;
    }}

    const idle = pollState.sendIdle &&
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
const responseState = await page.evaluate(() => {{
    const assistantMessages = Array.from(
        document.querySelectorAll('[data-message-author-role="assistant"]')
    );
    const newAssistantMessages = assistantMessages.slice(ASSISTANT_COUNT_BEFORE_SEND);
    const responseText = newAssistantMessages.map((m) => m.innerText).join('\n---\n').trim();
    return {{
        newAssistantCount: newAssistantMessages.length,
        responseText
    }};
}});

let status = pollError ? "error" : (completed ? "ok" : "timeout");
let error = pollError || null;
if (status === "ok" && responseState.newAssistantCount === 0) {{
    status = "error";
    error = "ChatGPT did not produce a new assistant response";
}} else if (status === "ok" && !ALLOW_EMPTY_RESPONSE && responseState.responseText.length === 0) {{
    status = "error";
    error = "ChatGPT returned an empty response";
}}

const result = {{
    status,
    error,
    elapsed_ms: Date.now() - pollStart,
    response_length: responseState.responseText.length,
    response: responseState.responseText,
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

/// Run the ChatGPT recipe via dev-browser.
///
/// This replaces the YAML-based chatgpt.yaml recipe with a single Playwright
/// script that handles navigation, model selection, file upload (via DataTransfer),
/// prompt entry, and response polling.
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

    // Parse and return the response
    let result: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("parse chatgpt recipe result: {stdout}"))?;

    if result["status"] == "error" {
        let err_msg = result["error"].as_str().unwrap_or("unknown error");
        return Err(anyhow!("ChatGPT error: {err_msg}"));
    }

    if result["status"] == "timeout" {
        return Err(anyhow!(
            "ChatGPT response timed out after {}ms",
            ctx.poll_settings.timeout_ms
        ));
    }

    Ok(result["response"].as_str().unwrap_or("").to_string())
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
        assert!(script.contains("const ASSISTANT_COUNT_BEFORE_SEND = await page.evaluate"));
        assert!(script.contains("let idleFingerprint = null;"));
        assert!(script.contains(
            ".result-thinking, [class*=\"result-thinking\"], [data-testid*=\"thinking\"]"
        ));
        assert!(script.contains("idleFingerprint === fingerprint"));
        assert!(!script.contains("await page.waitForTimeout(2000);\n        completed = true;"));
        assert!(script.contains("send button disabled — text may not have been injected"));
        assert!(script.contains(
            "newAssistantMessages = assistantMessages.slice(ASSISTANT_COUNT_BEFORE_SEND)"
        ));
        assert!(script.contains("ChatGPT returned an empty response"));
        assert!(script.contains("getComputedStyle"));
        assert!(!script.contains("parent.style.display"));
    }
}

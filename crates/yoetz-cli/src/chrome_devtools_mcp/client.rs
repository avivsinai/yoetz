//! Direct Chrome live-attach client used by the historical
//! `chrome_devtools_mcp` transport module.

use anyhow::{anyhow, bail, Context, Result};
use headless_chrome::{
    browser::tab::element::Element, protocol::cdp::Target::CreateTarget, Browser, Tab,
};
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

const DEFAULT_LOCAL_CDP_ENDPOINT: &str = "http://127.0.0.1:9222";
// `headless_chrome::Browser::connect_with_timeout` uses this as the transport's
// idle lifetime, not just the initial dial budget. ChatGPT Pro reviews can sit
// quiet for longer than 45s while the model thinks, so keep the attached CDP
// session alive for long-running recipes.
const CDP_SESSION_IDLE_TIMEOUT_SECS: u64 = 60 * 60;
const DISCOVERY_HTTP_TIMEOUT_MS: u64 = 750;

pub struct CdpMcpClient {
    browser: Browser,
    selected_tab: Mutex<Option<Arc<Tab>>>,
}

#[derive(Debug, Clone)]
pub struct NewPageResult {
    pub page_id: String,
}

#[derive(Debug, Clone)]
pub struct NavigateResult {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub raw: Value,
}

impl Snapshot {
    pub fn find_uid_by_role(&self, role: &str, name: &str) -> Option<String> {
        let wanted_role = role.trim();
        let wanted_name = name.trim();
        walk_snapshot(&self.raw, &mut |node| {
            let node_role = node.get("role").and_then(Value::as_str)?;
            if node_role != wanted_role {
                return None;
            }

            if !wanted_name.is_empty() {
                let node_name = node.get("name").and_then(Value::as_str)?;
                if node_name != wanted_name {
                    return None;
                }
            }

            node.get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
    }

    pub fn find_uid_by_text(&self, text: &str) -> Option<String> {
        let wanted = text.trim().to_ascii_lowercase();
        if wanted.is_empty() {
            return None;
        }

        walk_snapshot(&self.raw, &mut |node| {
            if node_contains_text(node, &wanted) {
                node.get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            } else {
                None
            }
        })
    }

    pub fn count_by_role(&self, role: &str) -> usize {
        count_nodes(&self.raw, role)
    }

    pub fn find_file_input_uid(&self) -> Option<String> {
        walk_snapshot(&self.raw, &mut |node| {
            let tag = node.get("tag").and_then(Value::as_str)?;
            let input_type = node.get("type").and_then(Value::as_str)?;
            if tag == "input" && input_type == "file" {
                node.get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            } else {
                None
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct WaitForResult {
    pub matched_text: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RunningChromeTarget {
    pub ws_endpoint: String,
    pub source_path: PathBuf,
    pub browser_name: String,
    pub chatgpt_tab_count: usize,
    pub modified_at: Option<SystemTime>,
}

impl RunningChromeTarget {
    pub fn has_chatgpt_tab(&self) -> bool {
        self.chatgpt_tab_count > 0
    }

    pub fn summary(&self) -> String {
        if self.chatgpt_tab_count > 0 {
            format!(
                "{} at {} (chatgpt tabs: {})",
                self.browser_name,
                self.source_path.display(),
                self.chatgpt_tab_count
            )
        } else {
            format!("{} at {}", self.browser_name, self.source_path.display())
        }
    }
}

impl CdpMcpClient {
    pub async fn connect_to_running_chrome(cdp_endpoint: Option<&str>) -> Result<Self> {
        let ws_endpoint = resolve_connect_ws_endpoint(cdp_endpoint)?;
        let browser = Browser::connect_with_timeout(
            ws_endpoint.clone(),
            Duration::from_secs(CDP_SESSION_IDLE_TIMEOUT_SECS),
        )
        .with_context(|| format!("connecting to Chrome websocket `{ws_endpoint}` failed"))?;

        Ok(Self {
            browser,
            selected_tab: Mutex::new(None),
        })
    }

    pub async fn new_page(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
    ) -> Result<NewPageResult> {
        let tab = self
            .browser
            .new_tab_with_options(create_target(url, background))
            .with_context(|| format!("creating a new Chrome page for `{url}` failed"))?;
        configure_tab_timeout(&tab, timeout_ms);
        tab.wait_until_navigated()
            .with_context(|| format!("waiting for Chrome page `{url}` to finish navigating"))?;
        self.set_selected_tab(tab.clone());

        Ok(NewPageResult {
            page_id: tab.get_target_id().to_string(),
        })
    }

    pub async fn navigate_page(&self, url: &str, timeout_ms: u64) -> Result<NavigateResult> {
        let tab = self.selected_tab()?;
        configure_tab_timeout(&tab, timeout_ms);
        tab.navigate_to(url)
            .with_context(|| format!("navigating selected Chrome page to `{url}` failed"))?
            .wait_until_navigated()
            .with_context(|| format!("waiting for Chrome page `{url}` to finish navigating"))?;

        Ok(NavigateResult {
            url: url.to_owned(),
        })
    }

    pub async fn take_snapshot(&self, verbose: bool) -> Result<Snapshot> {
        let tab = self.selected_tab()?;
        let snapshot = evaluate_json_payload(&tab, &build_snapshot_script(verbose), false)
            .context("building Chrome DOM snapshot failed")?;
        Ok(Snapshot { raw: snapshot })
    }

    pub async fn click(&self, uid: &str, double_click: bool) -> Result<()> {
        let tab = self.selected_tab()?;
        let element = find_snapshot_element(&tab, uid)?;
        element
            .click()
            .with_context(|| format!("clicking snapshot element `{uid}` failed"))?;
        if double_click {
            element
                .click()
                .with_context(|| format!("double-clicking snapshot element `{uid}` failed"))?;
        }
        Ok(())
    }

    pub async fn type_text(&self, text: &str, submit_key: Option<&str>) -> Result<()> {
        let tab = self.selected_tab()?;
        tab.type_str(text)
            .context("typing into selected Chrome page failed")?;
        if let Some(submit_key) = submit_key {
            tab.press_key(submit_key)
                .with_context(|| format!("pressing `{submit_key}` failed"))?;
        }
        Ok(())
    }

    pub async fn upload_file(&self, uid: &str, file_path: &Path) -> Result<()> {
        let tab = self.selected_tab()?;
        let file_path = file_path
            .canonicalize()
            .with_context(|| format!("resolving upload path `{}` failed", file_path.display()))?;
        let file_path = file_path.to_str().ok_or_else(|| {
            anyhow!(
                "upload_file path is not valid UTF-8: {}",
                file_path.display()
            )
        })?;

        let _ = tab.set_file_chooser_dialog_interception(true, None);
        if let Ok(element) = find_snapshot_element(&tab, uid) {
            if element.tag_name.eq_ignore_ascii_case("input")
                && element.get_attribute_value("type")?.as_deref() == Some("file")
            {
                let result = inject_files_on_input(&tab, &element, file_path, uid);
                let _ = tab.set_file_chooser_dialog_interception(false, None);
                return result;
            }

            let _ = element.click();
        }

        let input = tab
            .find_element("input[type='file']")
            .context("no file input became available after clicking the upload affordance")?;
        let result = inject_files_on_input(&tab, &input, file_path, uid);
        let _ = tab.set_file_chooser_dialog_interception(false, None);
        result
    }

    pub async fn wait_for(&self, text: &[&str], timeout_ms: u64) -> Result<WaitForResult> {
        if text.is_empty() {
            bail!("wait_for requires at least one text hint");
        }

        let tab = self.selected_tab()?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let wanted = text
            .iter()
            .map(|value| ((*value).to_owned(), value.to_ascii_lowercase()))
            .collect::<Vec<_>>();

        loop {
            let page_text = read_page_text(&tab)?.to_ascii_lowercase();
            if let Some((matched, _)) = wanted
                .iter()
                .find(|(_, needle)| !needle.is_empty() && page_text.contains(needle))
            {
                return Ok(WaitForResult {
                    matched_text: matched.clone(),
                });
            }

            if Instant::now() >= deadline {
                bail!(
                    "did not find any of the requested text hints within {timeout_ms}ms: {}",
                    text.join(", ")
                );
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn evaluate_script(&self, function: &str, args: Vec<Value>) -> Result<Value> {
        let tab = self.selected_tab()?;
        let uid_args = normalize_uid_args(args)?;
        let expression = build_evaluate_script_expression(function, &uid_args)?;
        let response = evaluate_json_payload(&tab, &expression, true)?;
        Ok(response.get("value").cloned().unwrap_or(Value::Null))
    }

    fn set_selected_tab(&self, tab: Arc<Tab>) {
        let mut guard = self.selected_tab.lock().unwrap();
        *guard = Some(tab);
    }

    fn selected_tab(&self) -> Result<Arc<Tab>> {
        self.selected_tab
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .context("no Chrome page is currently selected; call `new_page` first")
    }
}

fn resolve_connect_ws_endpoint(cdp_endpoint: Option<&str>) -> Result<String> {
    match cdp_endpoint {
        Some(endpoint) => {
            let parsed = Url::parse(endpoint)
                .with_context(|| format!("invalid Chrome CDP endpoint `{endpoint}`"))?;
            resolve_browser_websocket(&parsed)
        }
        None => {
            if let Some(ws_endpoint) = resolve_any_devtools_active_port_ws() {
                return Ok(ws_endpoint);
            }
            let parsed = Url::parse(DEFAULT_LOCAL_CDP_ENDPOINT)
                .expect("DEFAULT_LOCAL_CDP_ENDPOINT must be a valid URL");
            resolve_browser_websocket(&parsed)
        }
    }
}

pub fn discover_running_chrome_targets() -> Vec<RunningChromeTarget> {
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();

    for source_path in devtools_active_port_candidates() {
        let Ok(contents) = std::fs::read_to_string(&source_path) else {
            continue;
        };
        let Some(ws_endpoint) = parse_devtools_active_port(&contents, None) else {
            continue;
        };
        if !seen.insert(ws_endpoint.clone()) {
            continue;
        }

        let modified_at = std::fs::metadata(&source_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        let browser_name = fetch_browser_name(&ws_endpoint)
            .unwrap_or_else(|| browser_name_from_source_path(&source_path));
        let chatgpt_tab_count = fetch_chatgpt_tab_count(&ws_endpoint).unwrap_or(0);

        targets.push(RunningChromeTarget {
            ws_endpoint,
            source_path,
            browser_name,
            chatgpt_tab_count,
            modified_at,
        });
    }

    targets
}

fn create_target(url: &str, background: bool) -> CreateTarget {
    CreateTarget {
        url: url.to_owned(),
        left: None,
        top: None,
        width: None,
        height: None,
        window_state: None,
        browser_context_id: None,
        enable_begin_frame_control: None,
        new_window: None,
        background: Some(background),
        for_tab: None,
        hidden: None,
    }
}

fn configure_tab_timeout(tab: &Arc<Tab>, timeout_ms: u64) {
    tab.set_default_timeout(Duration::from_millis(timeout_ms.max(1)));
}

fn inject_files_on_input(
    tab: &Arc<Tab>,
    input: &Element<'_>,
    file_path: &str,
    uid: &str,
) -> Result<()> {
    match input.set_input_files(&[file_path]) {
        Ok(_) => Ok(()),
        Err(primary) => {
            let secondary = tab
                .handle_file_chooser(vec![file_path.to_owned()], input.node_id)
                .err();
            match secondary {
                None => Ok(()),
                Some(secondary) => Err(anyhow!(
                    "setting files on input `{uid}` failed via set_input_files ({primary:#}) and handle_file_chooser ({secondary:#})"
                )),
            }
        }
    }
}

fn resolve_browser_websocket(parsed: &Url) -> Result<String> {
    match parsed.scheme() {
        "ws" | "wss" => Ok(parsed.as_str().to_owned()),
        _ => {
            if let Some(ws_endpoint) = resolve_devtools_active_port_ws(parsed) {
                return Ok(ws_endpoint);
            }

            resolve_browser_websocket_via_json_version(parsed)
        }
    }
}

fn resolve_browser_websocket_via_json_version(endpoint: &Url) -> Result<String> {
    let version_url = browser_version_url(endpoint)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building /json/version HTTP client failed")?;
    let payload = client
        .get(version_url.clone())
        .send()
        .with_context(|| format!("requesting `{version_url}` failed"))?
        .error_for_status()
        .with_context(|| format!("Chrome rejected `{version_url}`"))?
        .json::<Value>()
        .with_context(|| format!("parsing `{version_url}` as JSON failed"))?;

    browser_websocket_from_json_version_payload(&payload)
        .with_context(|| format!("`{version_url}` did not expose a valid browser websocket URL"))
}

fn browser_version_url(endpoint: &Url) -> Result<Url> {
    let mut version_url = endpoint.clone();
    version_url.set_path("/json/version");
    version_url.set_query(None);
    version_url.set_fragment(None);
    Ok(version_url)
}

fn browser_websocket_from_json_version_payload(payload: &Value) -> Result<String> {
    let ws_endpoint = payload
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .context("missing `webSocketDebuggerUrl` in /json/version payload")?;
    if ws_endpoint.starts_with("ws://") || ws_endpoint.starts_with("wss://") {
        Ok(ws_endpoint.to_owned())
    } else {
        bail!("`webSocketDebuggerUrl` was not a websocket URL: {ws_endpoint}");
    }
}

fn resolve_devtools_active_port_ws(endpoint: &Url) -> Option<String> {
    if !matches!(endpoint.scheme(), "http" | "https") || !is_localhost_host(endpoint) {
        return None;
    }

    let expected_port = endpoint.port_or_known_default()?;
    devtools_active_port_candidates()
        .into_iter()
        .find_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|contents| parse_devtools_active_port(&contents, Some(expected_port)))
        })
}

fn resolve_any_devtools_active_port_ws() -> Option<String> {
    devtools_active_port_candidates()
        .into_iter()
        .find_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|contents| parse_devtools_active_port(&contents, None))
        })
}

fn fetch_browser_name(ws_endpoint: &str) -> Option<String> {
    let payload = local_http_json::<DevtoolsVersionPayload>(ws_endpoint, "/json/version")?;
    payload.browser.map(|browser| {
        browser
            .split('/')
            .next()
            .unwrap_or(browser.as_str())
            .to_string()
    })
}

fn fetch_chatgpt_tab_count(ws_endpoint: &str) -> Option<usize> {
    let entries = local_http_json::<Vec<DevtoolsTargetListEntry>>(ws_endpoint, "/json/list")?;
    Some(
        entries
            .iter()
            .filter(|entry| entry.target_type.as_deref() == Some("page"))
            .filter(|entry| {
                entry.url.as_deref().is_some_and(|url| {
                    is_chatgpt_url(url) || is_chatgpt_title(entry.title.as_deref())
                })
            })
            .count(),
    )
}

fn local_http_json<T: DeserializeOwned>(ws_endpoint: &str, path: &str) -> Option<T> {
    let url = browser_json_endpoint_from_ws(ws_endpoint, path)?;
    if url.scheme() != "http" {
        return None;
    }
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    let request_path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let mut stream = TcpStream::connect((host, port)).ok()?;
    let timeout = Some(Duration::from_millis(DISCOVERY_HTTP_TIMEOUT_MS));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let request =
        format!("GET {request_path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    parse_http_json_response(&response)
}

fn parse_http_json_response<T: DeserializeOwned>(response: &str) -> Option<T> {
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return None;
    }
    let (_, body) = response.split_once("\r\n\r\n")?;
    serde_json::from_str(body).ok()
}

fn browser_json_endpoint_from_ws(ws_endpoint: &str, path: &str) -> Option<Url> {
    let mut url = Url::parse(ws_endpoint).ok()?;
    match url.scheme() {
        "ws" => {
            let _ = url.set_scheme("http");
        }
        "wss" => {
            let _ = url.set_scheme("https");
        }
        _ => return None,
    }
    url.set_path(path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

fn is_localhost_host(endpoint: &Url) -> bool {
    matches!(
        endpoint.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1")
    )
}

fn devtools_active_port_candidates() -> Vec<PathBuf> {
    let home = home_dir_candidates();
    match std::env::consts::OS {
        "macos" => home
            .into_iter()
            .flat_map(|home| {
                [
                    home.join("Library/Application Support/Google/Chrome/DevToolsActivePort"),
                    home.join(
                        "Library/Application Support/Google/Chrome Canary/DevToolsActivePort",
                    ),
                    home.join("Library/Application Support/Chromium/DevToolsActivePort"),
                    home.join(
                        "Library/Application Support/BraveSoftware/Brave-Browser/DevToolsActivePort",
                    ),
                ]
            })
            .collect(),
        "windows" => home
            .into_iter()
            .flat_map(|home| {
                [
                    home.join("AppData/Local/Google/Chrome/User Data/DevToolsActivePort"),
                    home.join(
                        "AppData/Local/Google/Chrome Beta/User Data/DevToolsActivePort",
                    ),
                    home.join("AppData/Local/Google/Chrome SxS/User Data/DevToolsActivePort"),
                    home.join("AppData/Local/Chromium/User Data/DevToolsActivePort"),
                    home.join(
                        "AppData/Local/BraveSoftware/Brave-Browser/User Data/DevToolsActivePort",
                    ),
                ]
            })
            .collect(),
        _ => home
            .into_iter()
            .flat_map(|home| {
                [
                    home.join(".config/google-chrome/DevToolsActivePort"),
                    home.join(".config/chromium/DevToolsActivePort"),
                    home.join(".config/google-chrome-beta/DevToolsActivePort"),
                    home.join(".config/google-chrome-unstable/DevToolsActivePort"),
                    home.join(".config/BraveSoftware/Brave-Browser/DevToolsActivePort"),
                ]
            })
            .collect(),
    }
}

fn home_dir_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let path = PathBuf::from(profile);
        if !dirs.iter().any(|existing| existing == &path) {
            dirs.push(path);
        }
    }
    dirs
}

fn browser_name_from_source_path(source_path: &Path) -> String {
    let path = source_path.to_string_lossy().to_lowercase();
    if path.contains("chrome canary") || path.contains("google-chrome-unstable") {
        "Chrome Canary".to_string()
    } else if path.contains("chrome beta") || path.contains("google-chrome-beta") {
        "Chrome Beta".to_string()
    } else if path.contains("brave-browser") {
        "Brave".to_string()
    } else if path.contains("chromium") {
        "Chromium".to_string()
    } else {
        "Chrome".to_string()
    }
}

fn is_chatgpt_url(url: &str) -> bool {
    let haystack = url.to_ascii_lowercase();
    haystack.contains("chatgpt.com") || haystack.contains("chat.openai.com")
}

fn is_chatgpt_title(title: Option<&str>) -> bool {
    title
        .map(|value| value.to_ascii_lowercase().contains("chatgpt"))
        .unwrap_or(false)
}

fn parse_devtools_active_port(contents: &str, expected_port: Option<u16>) -> Option<String> {
    let lines = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let port = lines.first()?.parse::<u16>().ok()?;
    let web_socket_path = *lines.get(1)?;

    if expected_port.is_some_and(|expected| port != expected)
        || !web_socket_path.starts_with("/devtools/browser/")
    {
        return None;
    }

    Some(format!("ws://127.0.0.1:{port}{web_socket_path}"))
}

#[derive(Debug, Deserialize)]
struct DevtoolsVersionPayload {
    #[serde(rename = "Browser")]
    browser: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DevtoolsTargetListEntry {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "type")]
    target_type: Option<String>,
}

fn find_snapshot_element<'a>(tab: &'a Arc<Tab>, uid: &str) -> Result<Element<'a>> {
    tab.find_element(&query_selector_for_uid(uid))
        .with_context(|| format!("snapshot element `{uid}` is no longer present in the DOM"))
}

fn query_selector_for_uid(uid: &str) -> String {
    let safe_uid = uid.replace('\\', "\\\\").replace('"', "\\\"");
    format!("[data-yoetz-snapshot-id=\"{safe_uid}\"]")
}

fn evaluate_value(tab: &Arc<Tab>, expression: &str, await_promise: bool) -> Result<Value> {
    let remote = tab
        .evaluate(expression, await_promise)
        .with_context(|| format!("evaluating Chrome script failed:\n{expression}"))?;

    if let Some(value) = remote.value {
        return Ok(value);
    }

    if let Some(description) = remote.description {
        return Ok(Value::String(description));
    }

    bail!("Chrome evaluation returned no serializable value");
}

fn evaluate_json_payload(tab: &Arc<Tab>, expression: &str, await_promise: bool) -> Result<Value> {
    let raw = evaluate_value(tab, expression, await_promise)?;
    let json_text = raw
        .as_str()
        .context("Chrome evaluation did not return a JSON string payload")?;
    serde_json::from_str(json_text)
        .with_context(|| format!("Chrome evaluation returned invalid JSON: {json_text}"))
}

fn read_page_text(tab: &Arc<Tab>) -> Result<String> {
    let value = evaluate_value(
        tab,
        r#"(() => {
  const title = document.title || "";
  const body = document.body?.innerText || document.documentElement?.innerText || "";
  return `${title}\n${body}`.trim();
})()"#,
        false,
    )?;

    value
        .as_str()
        .map(ToOwned::to_owned)
        .context("Chrome page text evaluation did not return a string")
}

fn build_snapshot_script(verbose: bool) -> String {
    let max_text_chars = if verbose { 4_000 } else { 400 };
    format!(
        r#"(() => {{
  const maxTextChars = {max_text_chars};
  let nextId = Number(window.__yoetzSnapshotNextId || 1);

  const clip = (value) => {{
    if (typeof value !== "string") return "";
    const normalized = value.replace(/\s+/g, " ").trim();
    return normalized.length > maxTextChars
      ? normalized.slice(0, maxTextChars)
      : normalized;
  }};

  const labelledText = (el) => {{
    const ids = (el.getAttribute("aria-labelledby") || "")
      .split(/\s+/)
      .map((value) => value.trim())
      .filter(Boolean);
    if (ids.length === 0) return "";
    return clip(
      ids
        .map((id) => document.getElementById(id)?.innerText || "")
        .filter(Boolean)
        .join(" ")
    );
  }};

  const detectRole = (el) => {{
    const explicit = el.getAttribute("role");
    if (explicit) return explicit;

    const tag = el.tagName.toLowerCase();
    const type = (el.getAttribute("type") || "").toLowerCase();
    if (tag === "button") return "button";
    if (tag === "a" && el.hasAttribute("href")) return "link";
    if (tag === "textarea") return "textbox";
    if (tag === "input") {{
      if (type === "button" || type === "submit" || type === "reset") return "button";
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      return "textbox";
    }}
    if (tag === "select") return "combobox";
    if (tag === "option") return "option";
    if (tag === "ul" || tag === "ol") return "list";
    if (tag === "li") return "listitem";
    if (tag === "img") return "img";
    if (tag === "dialog") return "dialog";
    if (tag === "nav") return "navigation";
    if (tag === "main") return "main";
    if (tag === "article") return "article";
    if (el.getAttribute("contenteditable") === "true") return "textbox";
    if (el.getAttribute("data-message-author-role") === "assistant") return "article";
    return "";
  }};

  const elementText = (el) => {{
    const tag = el.tagName.toLowerCase();
    if (tag === "input" || tag === "textarea" || tag === "select") {{
      return clip(el.value || "");
    }}
    return clip(el.innerText || el.textContent || "");
  }};

  const inferredName = (el) => {{
    return (
      clip(el.getAttribute("aria-label") || "") ||
      labelledText(el) ||
      clip(el.getAttribute("title") || "") ||
      clip(el.getAttribute("placeholder") || "") ||
      clip(el.getAttribute("alt") || "") ||
      elementText(el)
    );
  }};

    const shouldInclude = (el) => {{
    if (!(el instanceof Element)) return false;
    const tag = el.tagName.toLowerCase();
    const role = detectRole(el);
    const name = inferredName(el);
    const text = elementText(el);
    return Boolean(
      el === document.activeElement ||
      role ||
      name ||
      text ||
      el.hasAttribute("data-message-author-role") ||
      el.matches("input[type='file']") ||
      el.matches("button, a[href], input, textarea, select, option, summary, label, [contenteditable='true']")
    );
  }};

  const ensureId = (el) => {{
    if (!el.dataset.yoetzSnapshotId) {{
      el.dataset.yoetzSnapshotId = `yoetz-${{nextId++}}`;
    }}
    return el.dataset.yoetzSnapshotId;
  }};

  const children = [];
  for (const el of Array.from(document.querySelectorAll("*"))) {{
    if (children.length >= 1200) break;
    if (!shouldInclude(el)) continue;

    const role = detectRole(el) || "generic";
    const name = inferredName(el);
    const text = elementText(el);
    const description = clip(el.getAttribute("data-message-author-role") || "");
    const node = {{
      id: ensureId(el),
      role,
      tag: el.tagName.toLowerCase(),
    }};
    if (name) node.name = name;
    if (description) node.description = description;
    if (text) node.text = text;
    const type = clip(el.getAttribute("type") || "");
    if (type) node.type = type;
    children.push(node);
  }}

  window.__yoetzSnapshotNextId = nextId;
  return JSON.stringify({{
    id: "root",
    role: "root_web_area",
    name: document.title || "",
    children,
  }});
}})()"#
    )
}

fn build_evaluate_script_expression(function: &str, uid_args: &[String]) -> Result<String> {
    let args_json = serde_json::to_string(uid_args).context("serializing evaluate_script args")?;
    Ok(format!(
        r#"(async () => {{
  const fn = ({function});
  const argIds = {args_json};
  const args = argIds.map((id) => document.querySelector(`[data-yoetz-snapshot-id="${{id}}"]`));
  const result = await fn(...args);
  return JSON.stringify({{ value: result === undefined ? null : result }});
}})()"#
    ))
}

fn normalize_uid_args(args: Vec<Value>) -> Result<Vec<String>> {
    args.into_iter()
        .enumerate()
        .map(|(idx, value)| match value {
            Value::String(uid) => Ok(uid),
            other => {
                bail!("evaluate_script args must be snapshot uid strings; arg {idx} was {other}")
            }
        })
        .collect()
}

fn walk_snapshot<T, F>(node: &Value, visit: &mut F) -> Option<T>
where
    F: FnMut(&serde_json::Map<String, Value>) -> Option<T>,
{
    let object = node.as_object()?;
    if let Some(found) = visit(object) {
        return Some(found);
    }

    if let Some(children) = object.get("children").and_then(Value::as_array) {
        for child in children {
            if let Some(found) = walk_snapshot(child, visit) {
                return Some(found);
            }
        }
    }

    None
}

fn count_nodes(node: &Value, role: &str) -> usize {
    let Some(object) = node.as_object() else {
        return 0;
    };

    let mut count = usize::from(object.get("role").and_then(Value::as_str) == Some(role));
    if let Some(children) = object.get("children").and_then(Value::as_array) {
        count += children
            .iter()
            .map(|child| count_nodes(child, role))
            .sum::<usize>();
    }
    count
}

fn node_contains_text(node: &serde_json::Map<String, Value>, wanted: &str) -> bool {
    node.iter().any(|(key, value)| match value {
        Value::String(text) => {
            key != "id" && key != "role" && text.to_ascii_lowercase().contains(wanted)
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot_fixture() -> Snapshot {
        Snapshot {
            raw: json!({
                "id": "root",
                "role": "root_web_area",
                "name": "ChatGPT",
                "children": [
                    {
                        "id": "composer",
                        "role": "textbox",
                        "name": "Message ChatGPT"
                    },
                    {
                        "id": "upload",
                        "role": "button",
                        "name": "Upload files and more"
                    },
                    {
                        "id": "assistant-list",
                        "role": "list",
                        "children": [
                            {
                                "id": "assistant-1",
                                "role": "listitem",
                                "name": "First answer"
                            },
                            {
                                "id": "assistant-2",
                                "role": "listitem",
                                "description": "Regenerate"
                            }
                        ]
                    }
                ]
            }),
        }
    }

    #[test]
    fn snapshot_find_uid_by_role_uses_structured_id() {
        let snapshot = snapshot_fixture();
        assert_eq!(
            snapshot.find_uid_by_role("textbox", "Message ChatGPT"),
            Some("composer".to_owned())
        );
        assert_eq!(
            snapshot.find_uid_by_role("button", "Upload files and more"),
            Some("upload".to_owned())
        );
    }

    #[test]
    fn snapshot_find_uid_by_text_searches_all_string_fields() {
        let snapshot = snapshot_fixture();
        assert_eq!(
            snapshot.find_uid_by_text("regenerate"),
            Some("assistant-2".to_owned())
        );
        assert_eq!(
            snapshot.find_uid_by_text("first answer"),
            Some("assistant-1".to_owned())
        );
    }

    #[test]
    fn snapshot_count_by_role_counts_nested_nodes() {
        let snapshot = snapshot_fixture();
        assert_eq!(snapshot.count_by_role("listitem"), 2);
        assert_eq!(snapshot.count_by_role("button"), 1);
    }

    #[test]
    fn evaluate_script_args_must_be_uid_strings() {
        let err = normalize_uid_args(vec![Value::String("uid-1".to_owned()), json!(1)])
            .expect_err("non-string args should be rejected");
        assert!(err.to_string().contains("snapshot uid strings"));
    }

    #[test]
    fn build_evaluate_script_expression_embeds_uid_array() {
        let expression = build_evaluate_script_expression(
            "() => true",
            &["yoetz-1".to_owned(), "yoetz-2".to_owned()],
        )
        .expect("expression should build");
        assert!(expression.contains(r#"const argIds = ["yoetz-1","yoetz-2"];"#));
        assert!(expression.contains("const fn = (() => true);"));
    }

    #[test]
    fn browser_websocket_from_json_version_reads_ws_url() {
        let payload = json!({
            "Browser": "Chrome/147.0.0.0",
            "webSocketDebuggerUrl": "ws://127.0.0.1:9222/devtools/browser/abc"
        });
        assert_eq!(
            browser_websocket_from_json_version_payload(&payload).expect("ws url should parse"),
            "ws://127.0.0.1:9222/devtools/browser/abc"
        );
    }

    #[test]
    fn browser_websocket_from_json_version_rejects_missing_url() {
        let payload = json!({ "Browser": "Chrome/147.0.0.0" });
        let err = browser_websocket_from_json_version_payload(&payload)
            .expect_err("missing websocket url should fail");
        assert!(err.to_string().contains("webSocketDebuggerUrl"));
    }

    #[test]
    fn parse_devtools_active_port_returns_browser_websocket() {
        let parsed =
            parse_devtools_active_port("9222\n/devtools/browser/from-active-port\n", Some(9222));
        assert_eq!(
            parsed.as_deref(),
            Some("ws://127.0.0.1:9222/devtools/browser/from-active-port")
        );
    }

    #[test]
    fn parse_devtools_active_port_rejects_mismatched_port_or_path() {
        assert!(parse_devtools_active_port("9333\n/devtools/browser/x\n", Some(9222)).is_none());
        assert!(parse_devtools_active_port("9222\n/devtools/page/x\n", Some(9222)).is_none());
    }

    #[test]
    fn parse_devtools_active_port_allows_auto_discovery_without_expected_port() {
        let parsed = parse_devtools_active_port("9333\n/devtools/browser/from-active-port\n", None);
        assert_eq!(
            parsed.as_deref(),
            Some("ws://127.0.0.1:9333/devtools/browser/from-active-port")
        );
    }

    #[test]
    fn browser_name_from_source_path_prefers_family_specific_labels() {
        assert_eq!(
            browser_name_from_source_path(Path::new(
                "/Users/test/Library/Application Support/Google/Chrome Canary/DevToolsActivePort"
            )),
            "Chrome Canary"
        );
        assert_eq!(
            browser_name_from_source_path(Path::new(
                "/Users/test/Library/Application Support/BraveSoftware/Brave-Browser/DevToolsActivePort"
            )),
            "Brave"
        );
        assert_eq!(
            browser_name_from_source_path(Path::new(
                "/home/test/.config/chromium/DevToolsActivePort"
            )),
            "Chromium"
        );
    }

    #[test]
    fn chatgpt_matchers_cover_url_and_title() {
        assert!(is_chatgpt_url("https://chatgpt.com/c/123"));
        assert!(is_chatgpt_url("https://chat.openai.com/"));
        assert!(!is_chatgpt_url("https://example.com/"));
        assert!(is_chatgpt_title(Some("ChatGPT - New chat")));
        assert!(!is_chatgpt_title(Some("Docs")));
    }
}

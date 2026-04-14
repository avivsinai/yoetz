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
pub struct ReusedPageResult {
    pub page_id: String,
    pub url: String,
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

    pub async fn reuse_chatgpt_page(&self, timeout_ms: u64) -> Result<ReusedPageResult> {
        let candidates = self.chatgpt_tabs();
        let tab = select_chatgpt_reuse_tab(candidates, timeout_ms)?;
        configure_tab_timeout(&tab, timeout_ms);
        let _ = tab.activate();
        let _ = tab.bring_to_front();
        self.set_selected_tab(tab.clone());

        Ok(ReusedPageResult {
            page_id: tab.get_target_id().to_string(),
            url: tab.get_url(),
        })
    }

    pub async fn select_chatgpt_page_for_probe(
        &self,
        timeout_ms: u64,
    ) -> Result<Option<ReusedPageResult>> {
        let candidates = self.chatgpt_tabs();
        if candidates.is_empty() {
            return Ok(None);
        }

        let tab = select_chatgpt_probe_tab(candidates, timeout_ms);
        configure_tab_timeout(&tab, timeout_ms);
        self.set_selected_tab(tab.clone());

        Ok(Some(ReusedPageResult {
            page_id: tab.get_target_id().to_string(),
            url: tab.get_url(),
        }))
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

    pub fn close_selected_page(&self, fire_unload: bool) -> Result<()> {
        let tab = self.selected_tab()?;
        tab.close(fire_unload)
            .context("closing selected Chrome page failed")?;
        Ok(())
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

    fn chatgpt_tabs(&self) -> Vec<Arc<Tab>> {
        self.browser.register_missing_tabs();
        self.browser
            .get_tabs()
            .lock()
            .unwrap()
            .iter()
            .filter(|tab| is_chatgpt_url(&tab.get_url()))
            .cloned()
            .collect::<Vec<_>>()
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
        let Some(target) = describe_running_chrome_target(&ws_endpoint, &source_path, modified_at)
        else {
            continue;
        };

        targets.push(target);
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

fn select_chatgpt_reuse_tab(candidates: Vec<Arc<Tab>>, timeout_ms: u64) -> Result<Arc<Tab>> {
    match candidates.len() {
        0 => bail!("thread=reuse requires an existing ChatGPT tab, but none are currently open"),
        1 => Ok(candidates.into_iter().next().expect("single candidate")),
        _ => {
            let probes = candidates
                .iter()
                .map(|tab| probe_chatgpt_tab(tab, timeout_ms))
                .collect::<Vec<_>>();
            let index = choose_chatgpt_reuse_probe(&probes)?;
            Ok(candidates
                .into_iter()
                .nth(index)
                .expect("chosen probe index should map to a candidate"))
        }
    }
}

fn select_chatgpt_probe_tab(candidates: Vec<Arc<Tab>>, timeout_ms: u64) -> Arc<Tab> {
    if candidates.len() == 1 {
        return candidates.into_iter().next().expect("single candidate");
    }

    let probes = candidates
        .iter()
        .map(|tab| probe_chatgpt_tab(tab, timeout_ms))
        .collect::<Vec<_>>();
    let index = choose_chatgpt_probe_index(&probes);
    candidates
        .into_iter()
        .nth(index)
        .expect("chosen probe index should map to a candidate")
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ChatgptTabProbe {
    url: String,
    visible: bool,
    has_focus: bool,
    is_generating: bool,
}

fn choose_chatgpt_reuse_probe(probes: &[ChatgptTabProbe]) -> Result<usize> {
    if probes.is_empty() {
        bail!("no ChatGPT tabs were available for reuse");
    }

    let available = probes
        .iter()
        .enumerate()
        .filter(|(_, probe)| !probe.is_generating)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if available.is_empty() {
        bail!(
            "thread=reuse found ChatGPT tabs, but all of them are still generating. \
             Wait for the current run to finish or use `--var thread=fresh`."
        );
    }

    let focused = probes
        .iter()
        .enumerate()
        .filter(|(index, probe)| probe.has_focus && available.contains(index))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if focused.len() == 1 {
        return Ok(focused[0]);
    }

    let visible = probes
        .iter()
        .enumerate()
        .filter(|(index, probe)| probe.visible && available.contains(index))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if visible.len() == 1 {
        return Ok(visible[0]);
    }

    if available.len() == 1 {
        return Ok(available[0]);
    }

    let rendered = probes
        .iter()
        .enumerate()
        .map(|(index, probe)| {
            format!(
                "#{index}: visible={}, focus={}, generating={}, url={}",
                probe.visible, probe.has_focus, probe.is_generating, probe.url
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "thread=reuse found multiple ChatGPT tabs but could not identify a unique active tab. \
         Keep only one ChatGPT tab visible or use `--var thread=fresh`. Candidates: {rendered}"
    );
}

fn choose_chatgpt_probe_index(probes: &[ChatgptTabProbe]) -> usize {
    let focused = probes
        .iter()
        .enumerate()
        .filter(|(_, probe)| probe.has_focus)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if focused.len() == 1 {
        return focused[0];
    }

    let visible = probes
        .iter()
        .enumerate()
        .filter(|(_, probe)| probe.visible)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if visible.len() == 1 {
        return visible[0];
    }

    0
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ChatgptTabProbePayload {
    #[serde(default)]
    url: String,
    #[serde(default)]
    visible: bool,
    #[serde(default)]
    has_focus: bool,
    #[serde(default)]
    is_generating: bool,
}

fn probe_chatgpt_tab(tab: &Arc<Tab>, timeout_ms: u64) -> ChatgptTabProbe {
    configure_tab_timeout(tab, timeout_ms);
    let fallback = ChatgptTabProbe {
        url: tab.get_url(),
        visible: false,
        has_focus: false,
        is_generating: false,
    };
    let payload = evaluate_json_payload(
        tab,
        r#"(() => JSON.stringify({
  url: window.location.href || "",
  visible: document.visibilityState === "visible",
  has_focus: typeof document.hasFocus === "function" ? !!document.hasFocus() : false,
  is_generating:
    !!document.querySelector("[data-message-author-role='assistant'].result-streaming, .result-streaming") ||
    !!document.querySelector("[data-testid='stop-button'], button[aria-label*='Stop']")
}))()"#,
        false,
    )
    .ok()
    .and_then(|value| serde_json::from_value::<ChatgptTabProbePayload>(value).ok());

    payload
        .map(|payload| ChatgptTabProbe {
            url: if payload.url.is_empty() {
                fallback.url.clone()
            } else {
                payload.url
            },
            visible: payload.visible,
            has_focus: payload.has_focus,
            is_generating: payload.is_generating,
        })
        .unwrap_or(fallback)
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
    let version_url_for_thread = version_url.clone();
    let payload = std::thread::spawn(move || -> Result<Value> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("building /json/version HTTP client failed")?;
        client
            .get(version_url_for_thread.clone())
            .send()
            .with_context(|| format!("requesting `{version_url_for_thread}` failed"))?
            .error_for_status()
            .with_context(|| format!("Chrome rejected `{version_url_for_thread}`"))?
            .json::<Value>()
            .with_context(|| format!("parsing `{version_url_for_thread}` as JSON failed"))
    })
    .join()
    .map_err(|_| anyhow!("requesting `{version_url}` panicked"))??;

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
            let contents = std::fs::read_to_string(&path).ok()?;
            let ws_endpoint = parse_devtools_active_port(&contents, None)?;
            let modified_at = std::fs::metadata(&path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            describe_running_chrome_target(&ws_endpoint, &path, modified_at)
                .map(|target| target.ws_endpoint)
        })
}

fn describe_running_chrome_target(
    ws_endpoint: &str,
    source_path: &Path,
    modified_at: Option<SystemTime>,
) -> Option<RunningChromeTarget> {
    let version = local_http_json::<DevtoolsVersionPayload>(ws_endpoint, "/json/version")?;
    let browser_name = version
        .browser
        .as_deref()
        .and_then(|browser| browser.split('/').next())
        .filter(|browser| !browser.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| browser_name_from_source_path(source_path));
    let chatgpt_tab_count = fetch_chatgpt_tab_count(ws_endpoint).unwrap_or(0);
    Some(RunningChromeTarget {
        ws_endpoint: ws_endpoint.to_string(),
        source_path: source_path.to_path_buf(),
        browser_name,
        chatgpt_tab_count,
        modified_at,
    })
}

fn fetch_chatgpt_tab_count(ws_endpoint: &str) -> Option<usize> {
    let entries = local_http_json::<Vec<DevtoolsTargetListEntry>>(ws_endpoint, "/json/list")?;
    Some(
        entries
            .iter()
            .filter(|entry| entry.target_type.as_deref() == Some("page"))
            .filter(|entry| {
                entry.url.as_deref().is_some_and(is_chatgpt_url)
                    || (entry.url.as_deref().is_none() && is_chatgpt_title(entry.title.as_deref()))
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
        .map(|value| value.trim().to_ascii_lowercase())
        .map(|value| {
            value == "chatgpt" || value.starts_with("chatgpt ") || value.starts_with("chatgpt -")
        })
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
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    };
    use tempfile::tempdir;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        #[allow(unsafe_code)]
        fn set<K>(key: &'static str, value: K) -> Self
        where
            K: AsRef<std::ffi::OsStr>,
        {
            let original = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            match &self.original {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn devtools_active_port_test_paths(home: &Path) -> (PathBuf, PathBuf) {
        match std::env::consts::OS {
            "macos" => (
                home.join("Library/Application Support/Google/Chrome/DevToolsActivePort"),
                home.join("Library/Application Support/Google/Chrome Canary/DevToolsActivePort"),
            ),
            "windows" => (
                home.join("AppData/Local/Google/Chrome/User Data/DevToolsActivePort"),
                home.join("AppData/Local/Google/Chrome SxS/User Data/DevToolsActivePort"),
            ),
            _ => (
                home.join(".config/google-chrome/DevToolsActivePort"),
                home.join(".config/chromium/DevToolsActivePort"),
            ),
        }
    }

    fn write_devtools_active_port(path: &Path, port: u16, suffix: &str) {
        fs::create_dir_all(path.parent().expect("candidate must have parent")).unwrap();
        fs::write(path, format!("{port}\n/devtools/browser/{suffix}\n")).unwrap();
    }

    fn spawn_fake_devtools_server() -> (u16, Arc<AtomicBool>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener
            .set_nonblocking(true)
            .expect("listener should accept nonblocking mode");
        let port = listener.local_addr().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_signal = shutdown.clone();
        let handle = thread::spawn(move || {
            while !shutdown_signal.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 2048];
                        let _ = stream.read(&mut request);
                        let request = String::from_utf8_lossy(&request);
                        let body = if request.starts_with("GET /json/version ") {
                            r#"{"Browser":"Chrome/147.0.0.0"}"#
                        } else if request.starts_with("GET /json/list ") {
                            r#"[{"type":"page","url":"https://chatgpt.com/c/123","title":"ChatGPT - New chat"}]"#
                        } else {
                            ""
                        };
                        let response = if body.is_empty() {
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                .to_string()
                        } else {
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            )
                        };
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        (port, shutdown, handle)
    }

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
    fn choose_chatgpt_reuse_probe_prefers_unique_focus() {
        let probes = vec![
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/old".to_string(),
                visible: false,
                has_focus: false,
                is_generating: false,
            },
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/current".to_string(),
                visible: true,
                has_focus: true,
                is_generating: false,
            },
        ];

        assert_eq!(choose_chatgpt_reuse_probe(&probes).unwrap(), 1);
    }

    #[test]
    fn choose_chatgpt_reuse_probe_prefers_unique_visible_when_focus_missing() {
        let probes = vec![
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/old".to_string(),
                visible: false,
                has_focus: false,
                is_generating: false,
            },
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/current".to_string(),
                visible: true,
                has_focus: false,
                is_generating: false,
            },
        ];

        assert_eq!(choose_chatgpt_reuse_probe(&probes).unwrap(), 1);
    }

    #[test]
    fn choose_chatgpt_reuse_probe_rejects_ambiguous_tabs() {
        let probes = vec![
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/1".to_string(),
                visible: false,
                has_focus: false,
                is_generating: false,
            },
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/2".to_string(),
                visible: false,
                has_focus: false,
                is_generating: false,
            },
        ];

        let err = choose_chatgpt_reuse_probe(&probes).unwrap_err();
        assert!(err.to_string().contains("multiple ChatGPT tabs"));
        assert!(err.to_string().contains("thread=fresh"));
    }

    #[test]
    fn choose_chatgpt_probe_index_prefers_unique_focus() {
        let probes = vec![
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/old".to_string(),
                visible: false,
                has_focus: false,
                is_generating: false,
            },
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/current".to_string(),
                visible: true,
                has_focus: true,
                is_generating: true,
            },
        ];

        assert_eq!(choose_chatgpt_probe_index(&probes), 1);
    }

    #[test]
    fn choose_chatgpt_probe_index_falls_back_to_first_candidate() {
        let probes = vec![
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/1".to_string(),
                visible: false,
                has_focus: false,
                is_generating: false,
            },
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/2".to_string(),
                visible: false,
                has_focus: false,
                is_generating: true,
            },
        ];

        assert_eq!(choose_chatgpt_probe_index(&probes), 0);
    }

    #[test]
    fn choose_chatgpt_reuse_probe_skips_busy_tab() {
        let probes = vec![
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/busy".to_string(),
                visible: true,
                has_focus: true,
                is_generating: true,
            },
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/idle".to_string(),
                visible: false,
                has_focus: false,
                is_generating: false,
            },
        ];

        assert_eq!(choose_chatgpt_reuse_probe(&probes).unwrap(), 1);
    }

    #[test]
    fn choose_chatgpt_reuse_probe_rejects_all_busy_tabs() {
        let probes = vec![
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/1".to_string(),
                visible: true,
                has_focus: true,
                is_generating: true,
            },
            ChatgptTabProbe {
                url: "https://chatgpt.com/c/2".to_string(),
                visible: false,
                has_focus: false,
                is_generating: true,
            },
        ];

        let err = choose_chatgpt_reuse_probe(&probes).unwrap_err();
        assert!(err.to_string().contains("still generating"));
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
    fn chatgpt_matchers_cover_url_and_title_fallback() {
        assert!(is_chatgpt_url("https://chatgpt.com/c/123"));
        assert!(is_chatgpt_url("https://chat.openai.com/"));
        assert!(!is_chatgpt_url("https://example.com/"));
        assert!(is_chatgpt_title(Some("ChatGPT - New chat")));
        assert!(is_chatgpt_title(Some("ChatGPT Workspace")));
        assert!(!is_chatgpt_title(Some("Notes about ChatGPT pricing")));
        assert!(!is_chatgpt_title(Some("Docs")));
    }

    #[test]
    #[allow(unsafe_code)]
    fn discover_running_chrome_targets_skips_unhealthy_active_port_files() {
        let home = tempdir().unwrap();
        let (stale_path, healthy_path) = devtools_active_port_test_paths(home.path());
        let (port, shutdown, handle) = spawn_fake_devtools_server();
        write_devtools_active_port(&stale_path, port + 1, "stale");
        write_devtools_active_port(&healthy_path, port, "healthy");

        let _home = EnvVarGuard::set("HOME", home.path().as_os_str());
        let _profile = EnvVarGuard::set("USERPROFILE", home.path().as_os_str());

        let targets = discover_running_chrome_targets();

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].source_path, healthy_path);
        assert_eq!(targets[0].browser_name, "Chrome");
        assert_eq!(targets[0].chatgpt_tab_count, 1);
    }
}

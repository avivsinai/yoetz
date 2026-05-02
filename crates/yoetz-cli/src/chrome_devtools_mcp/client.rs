//! Direct Chrome live-attach client used by the historical
//! `chrome_devtools_mcp` transport module.

use anyhow::{anyhow, bail, Context, Result};
use headless_chrome::{
    browser::tab::element::Element,
    protocol::cdp::Target::{CreateTarget, TargetInfo},
    Browser, Tab,
};
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Command,
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

/// Environment flag that opts into verbose CDP error rendering. When unset we
/// redact tab samples and inferred emails from error output so a casual error
/// does not leak a picture of what the user has open in Chrome (review
/// finding #9).
pub const YOETZ_DEBUG_CDP_ENV: &str = "YOETZ_DEBUG_CDP";

/// Environment flag that opts into trusting CDP endpoints that redirect to a
/// different host/port. Default is localhost-only: the websocket reported by
/// `/json/version` must live on the same host:port we probed, otherwise we
/// refuse to attach (review finding #5 — CDP discovery must not follow
/// arbitrary cross-host redirects).
pub const YOETZ_CDP_ALLOW_REMOTE_ENV: &str = "YOETZ_CDP_ALLOW_REMOTE";

pub(crate) fn cdp_remote_redirects_allowed() -> bool {
    std::env::var(YOETZ_CDP_ALLOW_REMOTE_ENV)
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

pub(crate) fn is_loopback_host(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let host = host.trim().to_ascii_lowercase();
    matches!(
        host.as_str(),
        "localhost" | "127.0.0.1" | "[::1]" | "::1" | "0.0.0.0"
    )
}

fn cdp_debug_enabled() -> bool {
    std::env::var(YOETZ_DEBUG_CDP_ENV)
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

/// Origins where a signed-in email naturally shows up in tab titles or URLs.
/// All other tabs are ignored when inferring which Chrome profile hosts which
/// email, so an unrelated or adversarially-named tab cannot steer
/// `profile_email` matching (review finding #9 — profile_email spoofing).
const EMAIL_INFERENCE_TRUSTED_ORIGINS: &[&str] = &[
    "accounts.google.com",
    "myaccount.google.com",
    "mail.google.com",
    "calendar.google.com",
    "auth.openai.com",
    "chatgpt.com",
    "platform.openai.com",
];

pub struct CdpMcpClient {
    browser: Browser,
    selected_tab: Mutex<Option<Arc<Tab>>>,
    ws_endpoint: String,
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

    /// Find the uid of a file input that has been explicitly scoped to the
    /// composer by [`crate::chatgpt_web::build_scope_composer_file_input_function`].
    /// Returns `None` if no such input is marked — callers must keep polling
    /// rather than falling back to the first page-wide file input, which could
    /// be an unrelated hidden control (review finding #10).
    pub fn find_marked_file_input_uid(&self, marker: &str) -> Option<String> {
        if marker.is_empty() {
            return None;
        }
        walk_snapshot(&self.raw, &mut |node| {
            let tag = node.get("tag").and_then(Value::as_str)?;
            let input_type = node.get("type").and_then(Value::as_str)?;
            let name = node.get("name").and_then(Value::as_str).unwrap_or("");
            if tag == "input" && input_type == "file" && name == marker {
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
    pub page_target_count: usize,
    pub page_samples: Vec<String>,
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
        } else if !self.page_samples.is_empty() {
            format!(
                "{} at {} (chatgpt tabs: 0; pages: {}; sample tabs: {})",
                self.browser_name,
                self.source_path.display(),
                self.page_target_count,
                self.page_samples.join(", ")
            )
        } else if self.page_target_count > 0 {
            format!(
                "{} at {} (chatgpt tabs: 0; pages: {})",
                self.browser_name,
                self.source_path.display(),
                self.page_target_count
            )
        } else {
            format!(
                "{} at {} (chatgpt tabs: 0; no page targets)",
                self.browser_name,
                self.source_path.display()
            )
        }
    }
}

pub fn browser_id_from_ws_endpoint(endpoint: &str) -> Option<String> {
    endpoint
        .split("/devtools/browser/")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DevtoolsActivePortFile {
    pub path: PathBuf,
    pub ws_endpoint: Option<String>,
    pub modified_at: Option<SystemTime>,
    pub healthy: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ChromiumProcessSummary {
    pub pid: u32,
    pub browser_name: String,
    pub command: String,
    pub has_remote_debugging: bool,
    pub user_data_dir: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserContextSummary {
    pub id: Option<String>,
    pub inferred_emails: Vec<String>,
    pub chatgpt_tab_count: usize,
    pub page_target_count: usize,
    pub sample_tabs: Vec<String>,
}

impl BrowserContextSummary {
    fn matches_email(&self, requested_email: &str) -> bool {
        let wanted = requested_email.trim().to_ascii_lowercase();
        self.inferred_emails.iter().any(|email| email == &wanted)
    }

    fn render_for_error(&self) -> String {
        let label = self.id.as_deref().unwrap_or("<default-context>");
        let verbose = cdp_debug_enabled();
        let emails = if !verbose {
            if self.inferred_emails.is_empty() {
                "none".to_string()
            } else {
                format!(
                    "{} inferred (set {YOETZ_DEBUG_CDP_ENV}=1 to reveal)",
                    self.inferred_emails.len()
                )
            }
        } else if self.inferred_emails.is_empty() {
            "unknown-email".to_string()
        } else {
            self.inferred_emails.join(", ")
        };
        let base = format!(
            "{label} [emails: {emails}; chatgpt tabs: {}; pages: {}",
            self.chatgpt_tab_count, self.page_target_count
        );
        if verbose && !self.sample_tabs.is_empty() {
            format!("{base}; sample tabs: {}]", self.sample_tabs.join(", "))
        } else {
            format!("{base}]")
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct BrowserContextPage {
    browser_context_id: Option<String>,
    title: String,
    url: String,
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
            ws_endpoint,
        })
    }

    pub fn ws_endpoint(&self) -> &str {
        &self.ws_endpoint
    }

    pub async fn new_page(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
        browser_context_id: Option<&str>,
    ) -> Result<NewPageResult> {
        let tab = match self.browser.new_tab_with_options(create_target(
            url,
            background,
            browser_context_id,
        )) {
            Ok(tab) => tab,
            Err(err) if browser_context_id.is_some() && is_missing_browser_context_error(&err) => {
                let browser_context_id = browser_context_id.expect("checked is_some");
                match self
                    .open_page_via_existing_context_tab(
                        url,
                        background,
                        timeout_ms,
                        Some(browser_context_id),
                    )
                    .await
                {
                    Ok(tab) => tab,
                    Err(open_err) => self
                        .reuse_existing_context_tab(
                            url,
                            background,
                            timeout_ms,
                            browser_context_id,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Chrome rejected browser_context_id `{browser_context_id}` for direct target creation, and opening a new tab from an existing page also failed"
                            )
                        })
                        .context(open_err)?,
                }
            }
            Err(err) => {
                let err = anyhow!(err);
                let err = if should_classify_external_create_target_block(&err, browser_context_id)
                {
                    err.context(external_create_target_block_guidance(url))
                } else {
                    err
                };
                return Err(err)
                    .with_context(|| format!("creating a new Chrome page for `{url}` failed"));
            }
        };
        configure_tab_timeout(&tab, timeout_ms);
        tab.wait_until_navigated()
            .with_context(|| format!("waiting for Chrome page `{url}` to finish navigating"))?;
        self.set_selected_tab(tab.clone());

        Ok(NewPageResult {
            page_id: tab.get_target_id().to_string(),
        })
    }

    pub async fn open_page_via_existing_tab(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
        browser_context_id: Option<&str>,
    ) -> Result<NewPageResult> {
        let tab = self
            .open_page_via_existing_context_tab(url, background, timeout_ms, browser_context_id)
            .await?;
        configure_tab_timeout(&tab, timeout_ms);
        tab.wait_until_navigated()
            .with_context(|| format!("waiting for Chrome page `{url}` to finish navigating"))?;
        self.set_selected_tab(tab.clone());

        Ok(NewPageResult {
            page_id: tab.get_target_id().to_string(),
        })
    }

    pub async fn open_chatgpt_page_via_existing_anchor(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
        browser_context_id: Option<&str>,
    ) -> Result<NewPageResult> {
        let page_targets = self
            .wait_for_recovery_anchor_candidates(browser_context_id, timeout_ms.min(3_000))
            .await?;
        let chatgpt_anchor_target = page_targets
            .iter()
            .find(|target| is_chatgpt_url(&target.url))
            .cloned();
        let safe_anchor_target = page_targets
            .iter()
            .filter_map(|target| context_tab_reuse_rank(&target.url).map(|rank| (rank, target)))
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, target)| target.clone());
        let strategy = choose_chatgpt_recovery_strategy(
            chatgpt_anchor_target.is_some(),
            safe_anchor_target
                .as_ref()
                .map(|target| target.url.as_str()),
            browser_context_id,
            url,
        )?;
        let tab = match strategy {
            ChatgptRecoveryStrategy::ReuseExistingChatgptTab => {
                let anchor =
                    chatgpt_anchor_target.expect("strategy requires a ChatGPT anchor target");
                eprintln!(
                    "warn: {YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB_ENV}=1 — navigating existing ChatGPT tab `{}` to `{url}`",
                    anchor.url
                );
                self.reuse_page_target(&anchor, url, background, timeout_ms)
                    .await?
            }
            ChatgptRecoveryStrategy::ExistingChatgptAnchor => {
                let anchor =
                    chatgpt_anchor_target.expect("strategy requires a ChatGPT anchor target");
                self.open_page_via_anchor_target(
                    &anchor,
                    url,
                    background,
                    timeout_ms,
                    browser_context_id,
                    "existing ChatGPT tab",
                )
                .await?
            }
            ChatgptRecoveryStrategy::ExistingSafeAnchor => {
                let anchor = safe_anchor_target.expect("strategy requires a safe anchor target");
                match self
                    .open_page_via_anchor_target(
                        &anchor,
                        url,
                        background,
                        timeout_ms,
                        browser_context_id,
                        "yoetz-safe tab",
                    )
                    .await
                {
                    Ok(tab) => tab,
                    Err(open_err) => {
                        if context_tab_navigation_reuse_rank(&anchor.url).is_some() {
                            self.reuse_page_target(&anchor, url, background, timeout_ms)
                                .await
                                .context(open_err)?
                        } else {
                            return Err(open_err.context(
                                "refusing to navigate an existing yoetz-owned ChatGPT tab as a recovery fallback",
                            ));
                        }
                    }
                }
            }
            ChatgptRecoveryStrategy::AnyUserPageAnchor => {
                // Operator opted in via YOETZ_ALLOW_USER_TAB_ANCHOR. Use the
                // first page-type tab in the context as the `window.open`
                // anchor. We DO NOT navigate the anchor; we only run a
                // `window.open(marked_url, '_blank')` in it, which spawns a
                // sibling yoetz-owned tab.
                let anchor = page_targets.into_iter().next().with_context(|| {
                    format!(
                        "YOETZ_ALLOW_USER_TAB_ANCHOR was set, but {} still has no existing page-type tabs to use as a `window.open` anchor for `{url}`",
                        browser_context_label(browser_context_id)
                    )
                })?;
                eprintln!(
                    "warn: YOETZ_ALLOW_USER_TAB_ANCHOR=1 — using user tab `{}` as window.open anchor (anchor is not navigated; yoetz only spawns a sibling tab from it)",
                    anchor.url
                );
                self.open_page_via_anchor_target(
                    &anchor,
                    url,
                    background,
                    timeout_ms,
                    browser_context_id,
                    "opt-in user tab",
                )
                .await?
            }
        };
        configure_tab_timeout(&tab, timeout_ms);
        tab.wait_until_navigated()
            .with_context(|| format!("waiting for Chrome page `{url}` to finish navigating"))?;
        self.set_selected_tab(tab.clone());

        Ok(NewPageResult {
            page_id: tab.get_target_id().to_string(),
        })
    }

    pub async fn select_chatgpt_page_for_probe(
        &self,
        timeout_ms: u64,
        browser_context_id: Option<&str>,
        preferred_run_id: Option<&str>,
    ) -> Result<Option<ReusedPageResult>> {
        let candidates = if let Some(preferred_run_id) = preferred_run_id {
            self.chatgpt_page_targets(browser_context_id)?
                .into_iter()
                .filter(|target| {
                    yoetz_run_id_from_url(&target.url).as_deref() == Some(preferred_run_id)
                })
                .collect::<Vec<_>>()
        } else if mutate_existing_chatgpt_tab_allowed() {
            // Explicit operator opt-in: auth probing may attach to an existing
            // user-owned ChatGPT tab instead of requiring a yoetz-owned probe
            // tab. This avoids fighting Chrome's new-target restrictions on
            // logged-in default profiles.
            self.chatgpt_page_targets(browser_context_id)?
        } else {
            // Default: only reuse a ChatGPT tab that yoetz itself stamped with
            // `?_yoetz=<run-id>`. Touching a user-owned ChatGPT conversation —
            // even just for a read-only probe — violates the fresh-tab contract
            // and can surface yoetz's automation on an in-flight chat.
            self.chatgpt_page_targets(browser_context_id)?
                .into_iter()
                .filter(|target| is_yoetz_owned_url(&target.url))
                .collect::<Vec<_>>()
        };
        if candidates.is_empty() {
            return Ok(None);
        }

        let tab = self.select_chatgpt_probe_target(candidates, timeout_ms)?;
        if let Some(preferred_run_id) = preferred_run_id {
            self.retire_duplicate_chatgpt_probe_tabs(
                browser_context_id,
                preferred_run_id,
                tab.get_target_id(),
            );
        }
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

        // The uid lookup failed, meaning the scoped snapshot target is no
        // longer in the DOM. Limit the fallback to the composer-scoped marker
        // (`title="yoetz-upload-target"`) so we never inject the bundle into
        // an unrelated hidden file input (review finding #10).
        let marker_selector = format!(
            "input[type='file'][title='{}']",
            crate::chatgpt_web::COMPOSER_FILE_INPUT_MARKER
        );
        let input = tab.find_element(&marker_selector).with_context(|| {
            format!(
                "no composer-scoped file input (`{marker_selector}`) was available after clicking the upload affordance"
            )
        })?;
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

    pub fn select_page_target(&self, target_id: &str, timeout_ms: u64) -> Result<()> {
        let tab = self.attach_page_target(target_id)?;
        configure_tab_timeout(&tab, timeout_ms);
        self.set_selected_tab(tab);
        Ok(())
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

    pub fn browser_context_summaries(&self) -> Result<Vec<BrowserContextSummary>> {
        Ok(summarize_browser_contexts(&self.browser_context_pages()?))
    }

    pub fn resolve_browser_context_id(
        &self,
        explicit_context_id: Option<&str>,
        requested_email: Option<&str>,
    ) -> Result<Option<String>> {
        let explicit_context_id = explicit_context_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let requested_email = requested_email
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if explicit_context_id.is_none() && requested_email.is_none() {
            return Ok(None);
        }

        let contexts = self.browser_context_summaries()?;
        if let Some(context_id) = explicit_context_id {
            let selected = contexts
                .iter()
                .find(|context| context.id.as_deref() == Some(context_id))
                .with_context(|| {
                    format!(
                        "browser_context_id `{context_id}` did not match any live Chrome browser context. Available contexts: {}",
                        render_browser_contexts_for_error(&contexts)
                    )
                })?;
            if let Some(requested_email) = requested_email {
                if !selected.matches_email(requested_email) {
                    bail!(
                        "browser_context_id `{context_id}` did not match profile_email `{requested_email}`. Selected context: {}",
                        selected.render_for_error()
                    );
                }
            }
            return Ok(Some(context_id.to_string()));
        }

        self.resolve_browser_context_id_for_email(&contexts, requested_email)
    }

    fn resolve_browser_context_id_for_email(
        &self,
        contexts: &[BrowserContextSummary],
        requested_email: Option<&str>,
    ) -> Result<Option<String>> {
        let Some(requested_email) = requested_email
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };

        let matches = contexts
            .iter()
            .filter(|context| context.matches_email(requested_email))
            .collect::<Vec<_>>();
        match matches.len() {
            0 => bail!(
                "profile_email `{requested_email}` did not match any live Chrome browser context. Available contexts: {}",
                render_browser_contexts_for_error(contexts)
            ),
            1 => Ok(matches[0].id.clone()),
            _ => bail!(
                "profile_email `{requested_email}` matched multiple live Chrome browser contexts. Matching contexts: {}",
                matches
                    .iter()
                    .map(|context| context.render_for_error())
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        }
    }

    fn browser_context_pages(&self) -> Result<Vec<BrowserContextPage>> {
        Ok(self
            .browser
            .get_page_targets()?
            .into_iter()
            .map(browser_context_page_from_target_info)
            .collect::<Vec<_>>())
    }

    fn page_targets_in_optional_browser_context(
        &self,
        browser_context_id: Option<&str>,
    ) -> Result<Vec<TargetInfo>> {
        Ok(self
            .browser
            .get_page_targets()?
            .into_iter()
            .filter(|target| {
                browser_context_id
                    .is_none_or(|expected| target.browser_context_id.as_deref() == Some(expected))
            })
            .collect::<Vec<_>>())
    }

    async fn wait_for_recovery_anchor_candidates(
        &self,
        browser_context_id: Option<&str>,
        timeout_ms: u64,
    ) -> Result<Vec<TargetInfo>> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
        let mut last_count = None;
        loop {
            let page_targets = self.page_targets_in_optional_browser_context(browser_context_id)?;
            if cdp_debug_enabled() && last_count != Some(page_targets.len()) {
                eprintln!(
                    "info: chrome-devtools-mcp recovery anchor scan in {}: page_targets={}",
                    browser_context_label(browser_context_id),
                    page_targets.len(),
                );
                last_count = Some(page_targets.len());
            }
            if !page_targets.is_empty() || Instant::now() >= deadline {
                return Ok(page_targets);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    fn select_reusable_optional_context_target(
        &self,
        browser_context_id: Option<&str>,
    ) -> Result<Option<TargetInfo>> {
        Ok(self
            .page_targets_in_optional_browser_context(browser_context_id)?
            .into_iter()
            .filter_map(|target| context_tab_reuse_rank(&target.url).map(|rank| (rank, target)))
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, target)| target))
    }

    fn select_navigable_optional_context_target(
        &self,
        browser_context_id: Option<&str>,
    ) -> Result<Option<TargetInfo>> {
        Ok(self
            .page_targets_in_optional_browser_context(browser_context_id)?
            .into_iter()
            .filter_map(|target| {
                context_tab_navigation_reuse_rank(&target.url).map(|rank| (rank, target))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, target)| target))
    }

    fn attach_page_target(&self, target_id: &str) -> Result<Arc<Tab>> {
        self.browser
            .attach_to_page_target(target_id)?
            .with_context(|| format!("page target `{target_id}` disappeared before attach"))
    }

    async fn open_page_via_anchor_target(
        &self,
        anchor_target: &TargetInfo,
        url: &str,
        background: bool,
        timeout_ms: u64,
        browser_context_id: Option<&str>,
        anchor_label: &str,
    ) -> Result<Arc<Tab>> {
        let before = self
            .page_targets_in_optional_browser_context(browser_context_id)?
            .into_iter()
            .map(|target| target.target_id)
            .collect::<BTreeSet<_>>();
        let anchor = self.attach_page_target(&anchor_target.target_id)?;
        configure_tab_timeout(&anchor, timeout_ms);
        anchor
            .evaluate(&build_window_open_expression(url)?, false)
            .with_context(|| {
                format!("opening a new Chrome page for `{url}` from {anchor_label} failed")
            })?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let maybe_target = self
                .page_targets_in_optional_browser_context(browser_context_id)?
                .into_iter()
                .find(|target| {
                    !before.contains(&target.target_id)
                        && (target.url.is_empty()
                            || target.url == "about:blank"
                            || target.url.starts_with(url))
                });
            if let Some(target) = maybe_target {
                let tab = self.attach_page_target(&target.target_id)?;
                if !background {
                    let _ = tab.activate();
                    let _ = tab.bring_to_front();
                }
                return Ok(tab);
            }

            if Instant::now() >= deadline {
                bail!(
                    "no new tab for `{url}` appeared in {} within {timeout_ms}ms",
                    browser_context_label(browser_context_id)
                );
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn open_page_via_existing_context_tab(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
        browser_context_id: Option<&str>,
    ) -> Result<Arc<Tab>> {
        // Never execute our `window.open(...)` script inside a tab we don't
        // own. Running JS on an arbitrary user tab (Meet, Gmail, etc.) even
        // for a benign side-effect is a leak of yoetz's automation surface
        // into the user's private browsing context. Restrict the anchor to
        // blank / yoetz-owned tabs — the same surface that
        // `context_tab_reuse_rank` deems safe to touch.
        let anchor = self
            .select_reusable_optional_context_target(browser_context_id)?
            .with_context(|| {
                format!(
                    "Chrome rejected {} for direct target creation, and there is no yoetz-safe (blank or yoetz-owned) tab in that context to open `{url}` from without touching user tabs",
                    browser_context_label(browser_context_id)
                )
            })?;
        self.open_page_via_anchor_target(
            &anchor,
            url,
            background,
            timeout_ms,
            browser_context_id,
            &browser_context_label(browser_context_id),
        )
        .await
        .with_context(|| {
            format!(
                "Chrome rejected {} for direct target creation",
                browser_context_label(browser_context_id)
            )
        })
    }

    async fn reuse_existing_context_tab(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
        browser_context_id: &str,
    ) -> Result<Arc<Tab>> {
        self.reuse_existing_optional_context_tab(url, background, timeout_ms, Some(browser_context_id))
            .await
            .with_context(|| {
                format!(
                    "Chrome rejected browser_context_id `{browser_context_id}` for direct target creation"
                )
            })
    }

    async fn reuse_page_target(
        &self,
        target: &TargetInfo,
        url: &str,
        background: bool,
        timeout_ms: u64,
    ) -> Result<Arc<Tab>> {
        let tab = self.attach_page_target(&target.target_id)?;
        configure_tab_timeout(&tab, timeout_ms);
        if !background {
            let _ = tab.activate();
            let _ = tab.bring_to_front();
        }
        tab.navigate_to(url)
            .with_context(|| {
                format!(
                    "navigating existing page target `{}` to `{url}` failed",
                    target.target_id
                )
            })?
            .wait_until_navigated()
            .with_context(|| {
                format!(
                    "waiting for page target `{}` to finish navigating to `{url}` failed",
                    target.target_id
                )
            })?;
        Ok(tab)
    }

    fn chatgpt_page_targets(&self, browser_context_id: Option<&str>) -> Result<Vec<TargetInfo>> {
        Ok(self
            .page_targets_in_optional_browser_context(browser_context_id)?
            .iter()
            .filter(|target| is_chatgpt_url(&target.url))
            .cloned()
            .collect::<Vec<_>>())
    }

    fn select_chatgpt_probe_target(
        &self,
        candidates: Vec<TargetInfo>,
        timeout_ms: u64,
    ) -> Result<Arc<Tab>> {
        if candidates.len() == 1 {
            return self.attach_page_target(&candidates[0].target_id);
        }

        let probes = candidates
            .iter()
            .map(|target| self.probe_chatgpt_target(target, timeout_ms))
            .collect::<Vec<_>>();
        let index = choose_chatgpt_probe_index(&probes);
        self.attach_page_target(&candidates[index].target_id)
    }

    fn retire_duplicate_chatgpt_probe_tabs(
        &self,
        browser_context_id: Option<&str>,
        preferred_run_id: &str,
        keep_target_id: &str,
    ) {
        let page_targets = self
            .chatgpt_page_targets(browser_context_id)
            .unwrap_or_default();
        let duplicate_targets = duplicate_chatgpt_probe_target_ids_to_retire(
            &page_targets,
            preferred_run_id,
            keep_target_id,
        );
        for target in duplicate_targets {
            if let Ok(tab) = self.attach_page_target(&target) {
                let _ = tab.close(true);
            }
        }
    }

    fn probe_chatgpt_target(&self, target: &TargetInfo, timeout_ms: u64) -> ChatgptTabProbe {
        self.attach_page_target(&target.target_id)
            .map(|tab| probe_chatgpt_tab(&tab, timeout_ms))
            .unwrap_or_else(|_| ChatgptTabProbe {
                url: target.url.clone(),
                visible: false,
                has_focus: false,
                is_generating: false,
            })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ChatgptRecoveryStrategy {
    ReuseExistingChatgptTab,
    ExistingChatgptAnchor,
    ExistingSafeAnchor,
    /// Caller opted in via YOETZ_ALLOW_USER_TAB_ANCHOR. Pick any existing
    /// page as the `window.open` anchor — the anchor itself is not
    /// navigated; we only run `window.open(marked_url, '_blank')` on it to
    /// spawn a sibling tab.
    AnyUserPageAnchor,
}

pub(crate) fn is_external_create_target_block_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("target.createtarget")
        && message.contains("chrome://inspect/#remote-debugging")
        && message.contains("new-tab creation")
}

fn should_classify_external_create_target_block(
    err: &anyhow::Error,
    browser_context_id: Option<&str>,
) -> bool {
    browser_context_id.is_none() && is_closed_cdp_transport_error(err)
}

/// Environment flag that opts into using any existing user tab as the
/// `window.open` anchor for the ChatGPT recovery flow. Default: off.
///
/// The anchor is never navigated away from; yoetz only runs
/// `window.open(marked_url, '_blank')` inside it to spawn a fresh
/// yoetz-owned tab. But that still executes a one-off JS call inside a tab
/// yoetz does not own — which we refuse by default (review finding #8 and
/// the Meet-tab near-miss). Set this env var when the operator has
/// explicitly consented to yoetz touching arbitrary tabs on this run.
pub const YOETZ_ALLOW_USER_TAB_ANCHOR_ENV: &str = "YOETZ_ALLOW_USER_TAB_ANCHOR";

pub(crate) fn user_tab_anchor_allowed() -> bool {
    std::env::var(YOETZ_ALLOW_USER_TAB_ANCHOR_ENV)
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

/// Stronger opt-in than YOETZ_ALLOW_USER_TAB_ANCHOR: allow yoetz to directly
/// navigate an existing ChatGPT tab instead of insisting on opening a fresh
/// sibling tab. This is useful on locked-down default-profile Chrome builds
/// where new target creation is the main failure mode, but it mutates the
/// user's current ChatGPT page and therefore remains opt-in.
pub const YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB_ENV: &str =
    "YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB";

fn mutate_existing_chatgpt_tab_allowed() -> bool {
    std::env::var(YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB_ENV)
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn choose_chatgpt_recovery_strategy(
    has_chatgpt_anchor: bool,
    safe_anchor_url: Option<&str>,
    browser_context_id: Option<&str>,
    url: &str,
) -> Result<ChatgptRecoveryStrategy> {
    if has_chatgpt_anchor && mutate_existing_chatgpt_tab_allowed() {
        return Ok(ChatgptRecoveryStrategy::ReuseExistingChatgptTab);
    }
    if safe_anchor_url.is_some_and(|value| context_tab_reuse_rank(value).is_some()) {
        return Ok(ChatgptRecoveryStrategy::ExistingSafeAnchor);
    }
    if has_chatgpt_anchor && user_tab_anchor_allowed() {
        return Ok(ChatgptRecoveryStrategy::ExistingChatgptAnchor);
    }
    if user_tab_anchor_allowed() {
        // Caller opted in via YOETZ_ALLOW_USER_TAB_ANCHOR=1. Use any existing
        // page as the `window.open` anchor — the anchor is still not
        // navigated; we just spawn a sibling tab from it. Marked user-opt-in
        // so the default still refuses to touch non-yoetz tabs.
        return Ok(ChatgptRecoveryStrategy::AnyUserPageAnchor);
    }
    bail!(
        "Chrome rejected {} for direct target creation, and there is no existing ChatGPT tab or yoetz-safe (blank or yoetz-owned) tab in that context to open `{url}` from without touching user tabs. Set {}=1 to explicitly allow yoetz to use any existing tab as the window.open anchor (the anchor itself is not navigated).",
        browser_context_label(browser_context_id),
        YOETZ_ALLOW_USER_TAB_ANCHOR_ENV
    );
}

pub fn is_closed_cdp_transport_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("underlying connection is closed")
        || message.contains("connectionclosed")
        || message.contains("received shutdown message")
}

fn external_create_target_block_guidance(url: &str) -> String {
    format!(
        "Chrome's default-profile CDP endpoint likely rejected external `Target.createTarget` while opening `{url}`. Chrome 146+/147 can allow attach/read operations but close the session on new-tab creation for untrusted clients. First, open chrome://inspect/#remote-debugging, refresh Discover network targets (or Open dedicated DevTools for Node), and retry. If Chrome still closes the session, launch Chrome with `--remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug` and pass `--cdp`, or use Chrome for Testing."
    )
}

impl CdpMcpClient {
    async fn reuse_existing_optional_context_tab(
        &self,
        url: &str,
        background: bool,
        timeout_ms: u64,
        browser_context_id: Option<&str>,
    ) -> Result<Arc<Tab>> {
        let target = self
            .select_navigable_optional_context_target(browser_context_id)?
            .with_context(|| {
                format!(
                    "there is no reusable blank or new-tab page in {} to navigate to `{url}`",
                    browser_context_label(browser_context_id)
                )
            })?;
        self.reuse_page_target(&target, url, background, timeout_ms)
            .await
            .with_context(|| {
                format!(
                    "navigating an existing Chrome tab to `{url}` inside {} failed",
                    browser_context_label(browser_context_id)
                )
            })
    }
}

fn browser_context_label(browser_context_id: Option<&str>) -> String {
    browser_context_id
        .map(|id| format!("browser_context_id `{id}`"))
        .unwrap_or_else(|| "the default browser context".to_string())
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

pub fn discover_devtools_active_port_files() -> Vec<DevtoolsActivePortFile> {
    let mut files = Vec::new();
    for path in devtools_active_port_candidates() {
        if let Some(file) = devtools_active_port_file_from_path(&path) {
            files.push(file);
        }
    }
    files
}

pub fn discover_local_chromium_processes() -> Vec<ChromiumProcessSummary> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let Ok(output) = Command::new("ps")
            .args(["axww", "-o", "pid=,command="])
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .filter_map(parse_local_chromium_process_line)
            .collect()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

fn create_target(url: &str, background: bool, browser_context_id: Option<&str>) -> CreateTarget {
    CreateTarget {
        url: url.to_owned(),
        left: None,
        top: None,
        width: None,
        height: None,
        window_state: None,
        browser_context_id: browser_context_id.map(str::to_owned),
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

fn duplicate_chatgpt_probe_target_ids_to_retire(
    targets: &[TargetInfo],
    preferred_run_id: &str,
    keep_target_id: &str,
) -> Vec<String> {
    targets
        .iter()
        .filter(|target| {
            yoetz_run_id_from_url(&target.url).as_deref() == Some(preferred_run_id)
                && target.target_id != keep_target_id
        })
        .map(|target| target.target_id.clone())
        .collect()
}

pub(crate) fn infer_email_hints(title: &str, url: &str) -> Vec<String> {
    // Spoofing guard: only trust emails that appear on origins where the
    // signed-in user's own email naturally surfaces. Every other tab — news
    // sites, social feeds, chat apps — can carry arbitrary `foo@bar.com`
    // tokens that must not influence `profile_email` matching (review
    // finding #9).
    if !is_trusted_email_origin(url) {
        return Vec::new();
    }
    let mut hints = BTreeSet::new();
    for source in [title, url] {
        for token in source.split(email_split_char) {
            let trimmed = trim_email_token(token);
            if looks_like_email(trimmed) {
                hints.insert(trimmed.to_ascii_lowercase());
            }
        }
    }
    hints.into_iter().collect()
}

pub(crate) fn is_trusted_email_origin(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return false;
    };
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    EMAIL_INFERENCE_TRUSTED_ORIGINS
        .iter()
        .any(|trusted| host == *trusted || host.ends_with(&format!(".{trusted}")))
}

fn summarize_browser_contexts(pages: &[BrowserContextPage]) -> Vec<BrowserContextSummary> {
    let mut grouped = BTreeMap::<Option<String>, Vec<&BrowserContextPage>>::new();
    for page in pages {
        grouped
            .entry(page.browser_context_id.clone())
            .or_default()
            .push(page);
    }

    grouped
        .into_iter()
        .map(|(id, pages)| {
            let inferred_emails = pages
                .iter()
                .flat_map(|page| infer_email_hints(&page.title, &page.url))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let chatgpt_tab_count = pages
                .iter()
                .filter(|page| is_chatgpt_url(&page.url))
                .count();
            let sample_tabs = pages
                .iter()
                .filter_map(|page| summarize_context_page(page))
                .take(4)
                .collect::<Vec<_>>();
            BrowserContextSummary {
                id,
                inferred_emails,
                chatgpt_tab_count,
                page_target_count: pages.len(),
                sample_tabs,
            }
        })
        .collect()
}

fn browser_context_page_from_target_info(info: TargetInfo) -> BrowserContextPage {
    BrowserContextPage {
        browser_context_id: info.browser_context_id,
        title: info.title,
        url: info.url,
    }
}

fn summarize_context_page(page: &BrowserContextPage) -> Option<String> {
    let title = page.title.trim();
    if !title.is_empty() {
        return Some(title.to_string());
    }
    let url = page.url.trim();
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

fn render_browser_contexts_for_error(contexts: &[BrowserContextSummary]) -> String {
    if contexts.is_empty() {
        return "<none>".to_string();
    }
    contexts
        .iter()
        .map(BrowserContextSummary::render_for_error)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn is_missing_browser_context_error(err: &anyhow::Error) -> bool {
    let raw = format!("{err:#}").to_ascii_lowercase();
    raw.contains("failed to find browser context with id")
}

fn build_window_open_expression(url: &str) -> Result<String> {
    Ok(format!(
        "window.open({}, '_blank');",
        serde_json::to_string(url).context("serializing window.open url")?
    ))
}

fn email_split_char(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '<' | '>'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '"'
                | '\''
                | '|'
                | ','
                | ';'
                | '/'
                | '?'
                | '&'
                | '='
                | ':'
        )
}

fn trim_email_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(
            ch,
            '.' | ':' | '!' | '?' | '/' | '\\' | '#' | '&' | '=' | '%'
        )
    })
}

fn looks_like_email(candidate: &str) -> bool {
    let Some((local, domain)) = candidate.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || domain.is_empty()
        || domain.starts_with('.')
        || domain.ends_with('.')
        || !domain.contains('.')
    {
        return false;
    }
    local.chars().all(is_email_local_char) && domain.chars().all(is_email_domain_char)
}

fn is_email_local_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '%' | '+' | '-')
}

fn is_email_domain_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-')
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
        "ws" | "wss" => {
            if !is_loopback_host(parsed.host_str()) && !cdp_remote_redirects_allowed() {
                bail!(
                    "Chrome CDP endpoint `{parsed}` is not on localhost; set {YOETZ_CDP_ALLOW_REMOTE_ENV}=1 to allow remote CDP targets"
                );
            }
            Ok(parsed.as_str().to_owned())
        }
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

    let raw_ws = browser_websocket_from_json_version_payload(&payload)
        .with_context(|| format!("`{version_url}` did not expose a valid browser websocket URL"))?;
    validate_discovered_ws_host(&raw_ws, endpoint).with_context(|| {
        format!("`{version_url}` returned a websocket URL pointing at a different host/port")
    })?;
    Ok(raw_ws)
}

fn validate_discovered_ws_host(ws_endpoint: &str, origin: &Url) -> Result<()> {
    let parsed = Url::parse(ws_endpoint)
        .with_context(|| format!("parsing discovered websocket URL `{ws_endpoint}` failed"))?;
    let origin_host = origin.host_str();
    let origin_port = origin.port_or_known_default();
    let ws_host = parsed.host_str();
    let ws_port = parsed.port_or_known_default();
    if origin_host == ws_host && origin_port == ws_port {
        return Ok(());
    }
    if cdp_remote_redirects_allowed() {
        return Ok(());
    }
    // Default: refuse to follow a CDP discovery that points at a different
    // host/port than the one we originally dialed. Set
    // YOETZ_CDP_ALLOW_REMOTE=1 to opt in (review finding #5).
    bail!(
        "discovered websocket `{ws_endpoint}` is on a different host/port than the requested endpoint `{origin}`; \
         set {YOETZ_CDP_ALLOW_REMOTE_ENV}=1 to allow cross-host CDP redirects"
    )
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
    resolve_any_devtools_active_port_ws_from_candidates(&devtools_active_port_candidates())
}

fn resolve_any_devtools_active_port_ws_from_candidates(candidates: &[PathBuf]) -> Option<String> {
    resolve_any_devtools_active_port_ws_from_candidates_with_approval_probe(
        candidates,
        &devtools_approval_mode_endpoint_is_reachable,
    )
}

fn resolve_any_devtools_active_port_ws_from_candidates_with_approval_probe(
    candidates: &[PathBuf],
    approval_mode_probe: &dyn Fn(&Path, &str) -> bool,
) -> Option<String> {
    let mut entries = candidates
        .iter()
        .filter_map(|path| {
            devtools_active_port_file_from_path_with_approval_probe(path, approval_mode_probe)
        })
        .filter(|file| file.healthy)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        browser_family_priority(&right.path)
            .cmp(&browser_family_priority(&left.path))
            .then_with(|| {
                modified_sort_key(right.modified_at).cmp(&modified_sort_key(left.modified_at))
            })
            .then_with(|| left.path.cmp(&right.path))
    });
    entries.into_iter().find_map(|file| file.ws_endpoint)
}

fn devtools_active_port_file_from_path(path: &Path) -> Option<DevtoolsActivePortFile> {
    devtools_active_port_file_from_path_with_approval_probe(
        path,
        &devtools_approval_mode_endpoint_is_reachable,
    )
}

fn devtools_active_port_file_from_path_with_approval_probe(
    path: &Path,
    approval_mode_probe: &dyn Fn(&Path, &str) -> bool,
) -> Option<DevtoolsActivePortFile> {
    let contents = std::fs::read_to_string(path).ok()?;
    let modified_at = std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    let ws_endpoint = parse_devtools_active_port(&contents, None);
    let healthy = ws_endpoint.as_deref().is_some_and(|endpoint| {
        describe_running_chrome_target(endpoint, path, modified_at).is_some()
            || approval_mode_probe(path, endpoint)
    });
    Some(DevtoolsActivePortFile {
        path: path.to_path_buf(),
        ws_endpoint,
        modified_at,
        healthy,
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
    let target_summary = fetch_target_list_summary(ws_endpoint).unwrap_or_default();
    Some(RunningChromeTarget {
        ws_endpoint: ws_endpoint.to_string(),
        source_path: source_path.to_path_buf(),
        browser_name,
        chatgpt_tab_count: target_summary.chatgpt_tab_count,
        page_target_count: target_summary.page_target_count,
        page_samples: target_summary.page_samples,
        modified_at,
    })
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct TargetListSummary {
    chatgpt_tab_count: usize,
    page_target_count: usize,
    page_samples: Vec<String>,
}

fn fetch_target_list_summary(ws_endpoint: &str) -> Option<TargetListSummary> {
    let entries = local_http_json::<Vec<DevtoolsTargetListEntry>>(ws_endpoint, "/json/list")?;
    let page_entries = entries
        .iter()
        .filter(|entry| entry.target_type.as_deref() == Some("page"))
        .collect::<Vec<_>>();
    let chatgpt_tab_count = page_entries
        .iter()
        .filter(|entry| {
            entry.url.as_deref().is_some_and(is_chatgpt_url)
                || (entry.url.as_deref().is_none() && is_chatgpt_title(entry.title.as_deref()))
        })
        .count();
    let page_samples = page_entries
        .iter()
        .filter_map(|entry| summarize_page_target(entry))
        .take(3)
        .collect::<Vec<_>>();
    Some(TargetListSummary {
        chatgpt_tab_count,
        page_target_count: page_entries.len(),
        page_samples,
    })
}

fn devtools_approval_mode_endpoint_is_reachable(source_path: &Path, ws_endpoint: &str) -> bool {
    let Some((host, port)) = local_devtools_ws_endpoint_parts(ws_endpoint) else {
        return false;
    };
    if !devtools_port_has_chromium_listener(source_path, port) {
        return false;
    }
    let Ok(addrs) = (host.as_str(), port).to_socket_addrs() else {
        return false;
    };
    addrs.into_iter().any(|addr| {
        TcpStream::connect_timeout(&addr, Duration::from_millis(DISCOVERY_HTTP_TIMEOUT_MS)).is_ok()
    })
}

fn local_devtools_ws_endpoint_parts(ws_endpoint: &str) -> Option<(String, u16)> {
    let Ok(url) = Url::parse(ws_endpoint) else {
        return None;
    };
    if !matches!(url.scheme(), "ws" | "wss") || !is_localhost_host(&url) {
        return None;
    }
    let host = url.host_str()?.to_string();
    let port = url.port_or_known_default()?;
    Some((host, port))
}

fn devtools_port_has_chromium_listener(source_path: &Path, port: u16) -> bool {
    let owner_pids = local_tcp_listener_pids(port);
    if owner_pids.is_empty() {
        return false;
    }

    discover_local_chromium_processes()
        .into_iter()
        .any(|process| {
            owner_pids.contains(&process.pid)
                && chromium_process_matches_active_port_path(&process, source_path)
        })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn local_tcp_listener_pids(port: u16) -> BTreeSet<u32> {
    let Ok(output) = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"])
        .output()
    else {
        return BTreeSet::new();
    };
    if !output.status.success() {
        return BTreeSet::new();
    }
    parse_lsof_pid_output(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn local_tcp_listener_pids(_port: u16) -> BTreeSet<u32> {
    BTreeSet::new()
}

fn parse_lsof_pid_output(output: &str) -> BTreeSet<u32> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix('p'))
        .filter_map(|pid| pid.trim().parse::<u32>().ok())
        .collect()
}

fn chromium_process_matches_active_port_path(
    process: &ChromiumProcessSummary,
    source_path: &Path,
) -> bool {
    let Some(user_data_dir) = process.user_data_dir.as_deref() else {
        return true;
    };
    let Some(active_port_dir) = source_path.parent() else {
        return false;
    };
    paths_equivalent(active_port_dir, Path::new(user_data_dir))
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn browser_family_priority(source_path: &Path) -> u8 {
    match browser_name_from_source_path(source_path).as_str() {
        "Chrome" | "Chrome for Testing" => 4,
        "Chrome Beta" => 3,
        "Chrome Canary" => 2,
        "Chromium" => 1,
        _ => 0,
    }
}

fn modified_sort_key(modified_at: Option<SystemTime>) -> u128 {
    modified_at
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn summarize_page_target(entry: &DevtoolsTargetListEntry) -> Option<String> {
    let title = entry.title.as_deref().map(str::trim).unwrap_or("");
    if !title.is_empty() {
        return Some(title.to_string());
    }
    let url = entry.url.as_deref()?.trim();
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

fn parse_local_chromium_process_line(line: &str) -> Option<ChromiumProcessSummary> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let pid = parts.next()?.trim().parse().ok()?;
    let command = parts.next()?.trim().to_string();
    let browser_name = browser_name_from_process_command(&command)?;
    if !is_top_level_chromium_browser_command(&command) {
        return None;
    }
    let has_remote_debugging =
        command.contains("--remote-debugging-port") || command.contains("--remote-debugging-pipe");
    Some(ChromiumProcessSummary {
        pid,
        browser_name,
        user_data_dir: extract_flag_value(&command, "--user-data-dir"),
        command,
        has_remote_debugging,
    })
}

fn browser_name_from_process_command(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    if lower.contains("google chrome.app/contents/macos/google chrome")
        || lower == "google chrome"
        || lower.ends_with("/google chrome")
    {
        Some("Chrome".to_string())
    } else if lower.contains("google chrome beta.app/contents/macos/google chrome beta")
        || lower == "google chrome beta"
        || lower.ends_with("/google chrome beta")
    {
        Some("Chrome Beta".to_string())
    } else if lower.contains("google chrome canary.app/contents/macos/google chrome canary")
        || lower == "google chrome canary"
        || lower.ends_with("/google chrome canary")
    {
        Some("Chrome Canary".to_string())
    } else if lower
        .contains("google chrome for testing.app/contents/macos/google chrome for testing")
        || lower == "google chrome for testing"
        || lower.ends_with("/google chrome for testing")
    {
        Some("Chrome for Testing".to_string())
    } else if lower.contains("brave browser.app/contents/macos/brave browser")
        || lower == "brave-browser"
        || lower.ends_with("/brave-browser")
    {
        Some("Brave".to_string())
    } else if lower.contains("microsoft edge.app/contents/macos/microsoft edge")
        || lower == "microsoft-edge"
        || lower.ends_with("/microsoft-edge")
        || lower == "msedge"
    {
        Some("Edge".to_string())
    } else if lower.contains("arc.app/contents/macos/arc")
        || lower == "arc"
        || lower.ends_with("/arc")
    {
        Some("Arc".to_string())
    } else if lower.contains("chromium.app/contents/macos/chromium")
        || lower == "chromium"
        || lower.ends_with("/chromium")
    {
        Some("Chromium".to_string())
    } else {
        None
    }
}

fn is_top_level_chromium_browser_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    !lower.contains("helper")
        && !lower.contains("crashpad")
        && !lower.contains("chrome-native-host")
        && !lower.contains("chrome-devtools-mcp")
        && !lower.contains("browserextensionhelper")
        && !lower.contains("--type=")
}

fn extract_flag_value(command: &str, flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    let start = command.find(&prefix)? + prefix.len();
    let remainder = &command[start..];
    if remainder.is_empty() {
        return Some(String::new());
    }
    let value = if let Some(quote) = remainder
        .chars()
        .next()
        .filter(|ch| *ch == '"' || *ch == '\'')
    {
        let quoted = &remainder[quote.len_utf8()..];
        let end = quoted.find(quote).unwrap_or(quoted.len());
        &quoted[..end]
    } else {
        let end = remainder.find(" --").unwrap_or(remainder.len());
        remainder[..end].trim_end()
    };
    Some(value.trim_matches('"').trim_matches('\'').to_string())
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
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    match std::env::consts::OS {
        "macos" => {
            for home in home {
                for path in [
                    home.join("Library/Application Support/Google/Chrome/DevToolsActivePort"),
                    home.join("Library/Application Support/Google/Chrome Canary/DevToolsActivePort"),
                    home.join("Library/Application Support/Google/Chrome Beta/DevToolsActivePort"),
                    home.join("Library/Application Support/Google/Chrome for Testing/DevToolsActivePort"),
                    home.join("Library/Application Support/Chromium/DevToolsActivePort"),
                    home.join("Library/Application Support/BraveSoftware/Brave-Browser/DevToolsActivePort"),
                    home.join("Library/Application Support/Microsoft Edge/DevToolsActivePort"),
                    home.join("Library/Application Support/Arc/DevToolsActivePort"),
                ] {
                    push_devtools_active_port_candidate(path, &mut candidates, &mut seen);
                }
                for root in [
                    home.join("Library/Application Support/Google"),
                    home.join("Library/Application Support/Chromium"),
                    home.join("Library/Application Support/BraveSoftware"),
                    home.join("Library/Application Support/Microsoft Edge"),
                    home.join("Library/Application Support/Arc"),
                ] {
                    collect_devtools_active_ports(&root, 4, &mut candidates, &mut seen);
                }
            }
        }
        "windows" => {
            for home in home {
                for path in [
                    home.join("AppData/Local/Google/Chrome/User Data/DevToolsActivePort"),
                    home.join("AppData/Local/Google/Chrome Beta/User Data/DevToolsActivePort"),
                    home.join("AppData/Local/Google/Chrome SxS/User Data/DevToolsActivePort"),
                    home.join(
                        "AppData/Local/Google/Chrome for Testing/User Data/DevToolsActivePort",
                    ),
                    home.join("AppData/Local/Chromium/User Data/DevToolsActivePort"),
                    home.join(
                        "AppData/Local/BraveSoftware/Brave-Browser/User Data/DevToolsActivePort",
                    ),
                    home.join("AppData/Local/Microsoft/Edge/User Data/DevToolsActivePort"),
                ] {
                    push_devtools_active_port_candidate(path, &mut candidates, &mut seen);
                }
                for root in [
                    home.join("AppData/Local/Google"),
                    home.join("AppData/Local/Chromium"),
                    home.join("AppData/Local/BraveSoftware"),
                    home.join("AppData/Local/Microsoft"),
                ] {
                    collect_devtools_active_ports(&root, 4, &mut candidates, &mut seen);
                }
            }
        }
        _ => {
            for home in home {
                for path in [
                    home.join(".config/google-chrome/DevToolsActivePort"),
                    home.join(".config/chromium/DevToolsActivePort"),
                    home.join(".config/google-chrome-beta/DevToolsActivePort"),
                    home.join(".config/google-chrome-unstable/DevToolsActivePort"),
                    home.join(".config/google-chrome-for-testing/DevToolsActivePort"),
                    home.join(".config/BraveSoftware/Brave-Browser/DevToolsActivePort"),
                    home.join(".config/microsoft-edge/DevToolsActivePort"),
                    home.join(".config/Arc/DevToolsActivePort"),
                ] {
                    push_devtools_active_port_candidate(path, &mut candidates, &mut seen);
                }
                for root in [
                    home.join(".config/google-chrome"),
                    home.join(".config/chromium"),
                    home.join(".config/BraveSoftware"),
                    home.join(".config/microsoft-edge"),
                    home.join(".config/Arc"),
                ] {
                    collect_devtools_active_ports(&root, 4, &mut candidates, &mut seen);
                }
            }
        }
    }
    candidates
}

fn push_devtools_active_port_candidate(
    path: PathBuf,
    candidates: &mut Vec<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
) {
    if seen.insert(path.clone()) {
        candidates.push(path);
    }
}

fn collect_devtools_active_ports(
    root: &Path,
    depth_remaining: usize,
    candidates: &mut Vec<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
) {
    if depth_remaining == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name == "DevToolsActivePort")
        {
            push_devtools_active_port_candidate(path, candidates, seen);
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_devtools_active_ports(&path, depth_remaining - 1, candidates, seen);
        }
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
    } else if path.contains("chrome for testing") || path.contains("google-chrome-for-testing") {
        "Chrome for Testing".to_string()
    } else if path.contains("chrome beta") || path.contains("google-chrome-beta") {
        "Chrome Beta".to_string()
    } else if path.contains("microsoft edge") || path.contains("msedge") {
        "Edge".to_string()
    } else if path.contains("/arc/") {
        "Arc".to_string()
    } else if path.contains("brave-browser") {
        "Brave".to_string()
    } else if path.contains("chromium") {
        "Chromium".to_string()
    } else {
        "Chrome".to_string()
    }
}

fn is_chatgpt_url(url: &str) -> bool {
    let parsed = match Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    matches!(
        parsed
            .host_str()
            .map(|host| host.to_ascii_lowercase())
            .as_deref(),
        Some("chatgpt.com") | Some("chat.openai.com")
    )
}

fn yoetz_run_id_from_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    parsed
        .query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("_yoetz"))
        .map(|(_, value)| value.into_owned())
}

/// A yoetz-owned tab is one whose URL carries the `?_yoetz=<run-id>` or
/// `&_yoetz=<run-id>` marker yoetz stamps on every fresh tab it creates.
/// yoetz MUST NOT `evaluate`, `navigate`, or otherwise mutate any tab that
/// does not pass this check — doing so risks leaking automation into a user
/// conversation (see review finding #8 and the Meet-tab near-miss).
fn is_yoetz_owned_url(url: &str) -> bool {
    yoetz_run_id_from_url(url).is_some()
}

fn context_tab_reuse_rank(url: &str) -> Option<u8> {
    let normalized = url.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "about:blank" {
        Some(0)
    } else if normalized.starts_with("chrome://newtab")
        || normalized.starts_with("chrome://new-tab-page")
    {
        Some(1)
    } else if is_chatgpt_url(&normalized) {
        // Yoetz-owned ChatGPT tabs are safe for `window.open(...)` anchors,
        // but they are deliberately ranked behind disposable blank/new-tab
        // pages so recovery preserves per-run tab isolation when possible.
        if is_yoetz_owned_url(&normalized) {
            Some(2)
        } else {
            None
        }
    } else {
        None
    }
}

fn context_tab_navigation_reuse_rank(url: &str) -> Option<u8> {
    if is_chatgpt_url(url) {
        None
    } else {
        context_tab_reuse_rank(url)
    }
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

  document
    .querySelectorAll("[data-yoetz-snapshot-id]")
    .forEach((el) => el.removeAttribute("data-yoetz-snapshot-id"));

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
        spawn_http_devtools_server(true)
    }

    fn spawn_approval_mode_devtools_server() -> (u16, Arc<AtomicBool>, thread::JoinHandle<()>) {
        spawn_http_devtools_server(false)
    }

    fn spawn_http_devtools_server(
        expose_json: bool,
    ) -> (u16, Arc<AtomicBool>, thread::JoinHandle<()>) {
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
                        let body = if expose_json && request.starts_with("GET /json/version ") {
                            r#"{"Browser":"Chrome/147.0.0.0"}"#
                        } else if expose_json && request.starts_with("GET /json/list ") {
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

    fn spawn_stalled_websocket_server() -> (u16, Arc<AtomicBool>, thread::JoinHandle<()>) {
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
                    Ok((_stream, _)) => {
                        while !shutdown_signal.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(10));
                        }
                        break;
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
    fn infer_email_hints_extracts_emails_from_titles_and_urls() {
        let hints = infer_email_hints(
            "Inbox (6,096) - aviv.s@taboola.com - Taboola Mail",
            "https://mail.google.com/mail/u/0/#inbox",
        );
        assert_eq!(hints, vec!["aviv.s@taboola.com".to_string()]);

        let from_url = infer_email_hints(
            "Account chooser",
            "https://accounts.google.com/AccountChooser?Email=avivsinai@gmail.com",
        );
        assert_eq!(from_url, vec!["avivsinai@gmail.com".to_string()]);
    }

    #[test]
    fn summarize_browser_contexts_groups_tabs_by_context_and_email() {
        let contexts = summarize_browser_contexts(&[
            BrowserContextPage {
                browser_context_id: Some("ctx-personal".to_string()),
                title: "Inbox (43,617) - avivsinai@gmail.com - Gmail".to_string(),
                url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
            },
            BrowserContextPage {
                browser_context_id: Some("ctx-personal".to_string()),
                title: "Personal Pro OK".to_string(),
                url: "https://chatgpt.com/c/1".to_string(),
            },
            BrowserContextPage {
                browser_context_id: Some("ctx-work".to_string()),
                title: "Inbox (6,096) - aviv.s@taboola.com - Taboola Mail".to_string(),
                url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
            },
        ]);

        assert_eq!(contexts.len(), 2);
        assert!(contexts.iter().any(|context| {
            context.id.as_deref() == Some("ctx-personal")
                && context.inferred_emails == vec!["avivsinai@gmail.com".to_string()]
                && context.chatgpt_tab_count == 1
        }));
        assert!(contexts.iter().any(|context| {
            context.id.as_deref() == Some("ctx-work")
                && context.inferred_emails == vec!["aviv.s@taboola.com".to_string()]
                && context.chatgpt_tab_count == 0
        }));
    }

    #[test]
    fn browser_context_page_from_target_info_preserves_context_title_and_url() {
        let page = browser_context_page_from_target_info(TargetInfo {
            target_id: "target-1".to_string(),
            Type: "page".to_string(),
            title: "Inbox (43,617) - avivsinai@gmail.com - Gmail".to_string(),
            url: "https://mail.google.com/mail/u/0/#inbox".to_string(),
            attached: false,
            opener_id: None,
            can_access_opener: false,
            opener_frame_id: None,
            parent_frame_id: None,
            browser_context_id: Some("ctx-personal".to_string()),
            subtype: None,
        });

        assert_eq!(page.browser_context_id.as_deref(), Some("ctx-personal"));
        assert_eq!(page.title, "Inbox (43,617) - avivsinai@gmail.com - Gmail");
        assert_eq!(page.url, "https://mail.google.com/mail/u/0/#inbox");
    }

    #[test]
    #[serial_test::serial]
    fn render_browser_contexts_for_error_redacts_details_by_default() {
        // In the default (non-debug) mode, inferred emails and sample tab
        // titles must not leak into routine error strings (review finding #9).
        let previous = std::env::var(YOETZ_DEBUG_CDP_ENV).ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(YOETZ_DEBUG_CDP_ENV);
        }
        let rendered = render_browser_contexts_for_error(&[BrowserContextSummary {
            id: Some("ctx-work".to_string()),
            inferred_emails: vec!["aviv.s@taboola.com".to_string()],
            chatgpt_tab_count: 1,
            page_target_count: 3,
            sample_tabs: vec!["Taboola Mail".to_string(), "ChatGPT".to_string()],
        }]);
        assert!(rendered.contains("ctx-work"));
        assert!(
            !rendered.contains("aviv.s@taboola.com"),
            "default error rendering must not leak inferred emails; got: {rendered}"
        );
        assert!(
            !rendered.contains("Taboola Mail"),
            "default error rendering must not leak sample tab titles; got: {rendered}"
        );
        assert!(rendered.contains("1 inferred"));
        if let Some(value) = previous {
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var(YOETZ_DEBUG_CDP_ENV, value);
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn render_browser_contexts_for_error_verbose_under_debug_flag() {
        // With the debug flag set (via the YOETZ_DEBUG_CDP env var or the CLI
        // `--debug` wiring), full details are allowed so operators can still
        // diagnose profile mismatches.
        let previous = std::env::var(YOETZ_DEBUG_CDP_ENV).ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(YOETZ_DEBUG_CDP_ENV, "1");
        }
        let rendered = render_browser_contexts_for_error(&[BrowserContextSummary {
            id: Some("ctx-work".to_string()),
            inferred_emails: vec!["aviv.s@taboola.com".to_string()],
            chatgpt_tab_count: 1,
            page_target_count: 3,
            sample_tabs: vec!["Taboola Mail".to_string(), "ChatGPT".to_string()],
        }]);
        assert!(rendered.contains("ctx-work"));
        assert!(rendered.contains("aviv.s@taboola.com"));
        assert!(rendered.contains("Taboola Mail"));
        match previous {
            Some(value) => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var(YOETZ_DEBUG_CDP_ENV, value);
                }
            }
            None => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::remove_var(YOETZ_DEBUG_CDP_ENV);
                }
            }
        }
    }

    #[test]
    fn is_trusted_email_origin_accepts_known_identity_providers() {
        assert!(is_trusted_email_origin(
            "https://mail.google.com/mail/u/0/#inbox"
        ));
        assert!(is_trusted_email_origin(
            "https://accounts.google.com/AccountChooser?Email=x@y.com"
        ));
        assert!(is_trusted_email_origin("https://chatgpt.com/settings"));
    }

    #[test]
    fn is_trusted_email_origin_rejects_arbitrary_tabs() {
        assert!(!is_trusted_email_origin("https://news.ycombinator.com/"));
        assert!(!is_trusted_email_origin(
            "https://attacker.example.com/?Email=a@b.com"
        ));
        assert!(!is_trusted_email_origin("about:blank"));
        assert!(!is_trusted_email_origin(""));
        // Domain-name trick: `.fake-accounts.google.com.evil.com` must not be
        // accepted as a trusted subdomain.
        assert!(!is_trusted_email_origin(
            "https://accounts.google.com.evil.com/"
        ));
    }

    #[test]
    fn infer_email_hints_ignores_untrusted_origins() {
        let hints = infer_email_hints(
            "Look at attacker@example.com",
            "https://social.example/news",
        );
        assert!(
            hints.is_empty(),
            "untrusted origins must not emit email hints"
        );
    }

    #[test]
    fn create_target_preserves_browser_context_id() {
        let request = create_target("https://chatgpt.com", false, Some("ctx-work"));
        assert_eq!(request.url, "https://chatgpt.com");
        assert_eq!(request.browser_context_id.as_deref(), Some("ctx-work"));
        assert_eq!(request.new_window, None);
        assert_eq!(request.background, Some(false));
    }

    #[test]
    fn external_create_target_block_error_is_detected() {
        let err = anyhow!(
            "creating a new Chrome page for `about:blank` failed: Chrome's default-profile CDP endpoint likely rejected external `Target.createTarget` while opening `about:blank`. Chrome 146+/147 can allow attach/read operations but close the session on new-tab creation for untrusted clients. First, open chrome://inspect/#remote-debugging, refresh Discover network targets (or Open dedicated DevTools for Node), and retry. If Chrome still closes the session, launch Chrome with `--remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug` and pass `--cdp`, or use Chrome for Testing. Unable to make method calls because underlying connection is closed"
        );
        assert!(is_external_create_target_block_error(&err));
    }

    #[test]
    fn external_create_target_block_error_rejects_generic_inspect_guidance() {
        let err = anyhow!(
            "could not reach chrome's cdp endpoint. enable chrome://inspect/#remote-debugging"
        );
        assert!(!is_external_create_target_block_error(&err));
    }

    #[test]
    fn browser_id_from_ws_endpoint_reads_devtools_suffix() {
        let id = browser_id_from_ws_endpoint(
            "ws://127.0.0.1:9222/devtools/browser/a1cbe996-1406-42e3-93d6-e9e5bcb66f4a",
        );
        assert_eq!(id.as_deref(), Some("a1cbe996-1406-42e3-93d6-e9e5bcb66f4a"));
    }

    #[test]
    fn is_yoetz_owned_url_accepts_marker_and_rejects_unrelated_tabs() {
        // The `?_yoetz=` (or `&_yoetz=`) marker is the single gate that
        // decides whether yoetz may run JS / navigate against a tab. This
        // test exists so the gate stays narrow — user tabs like Meet,
        // Gmail, calendar must all be rejected, even when the tab URL
        // contains suggestive substrings.
        assert!(is_yoetz_owned_url(
            "https://chatgpt.com/?_yoetz=20260419T073330Z_abc"
        ));
        assert!(is_yoetz_owned_url(
            "https://chatgpt.com/c/xyz?foo=bar&_yoetz=run-abc"
        ));
        assert!(is_yoetz_owned_url("HTTPS://Chatgpt.com/?_YOETZ=ABC"));
        assert!(!is_yoetz_owned_url("https://meet.google.com/abc-defg-hij"));
        assert!(!is_yoetz_owned_url(
            "https://mail.google.com/mail/u/0/#inbox"
        ));
        assert!(!is_yoetz_owned_url("https://chatgpt.com/c/abc"));
        // Should not be fooled by an adversarial path that merely contains
        // the substring without the `=` marker (e.g. a note app URL that
        // happens to render the token).
        assert!(!is_yoetz_owned_url("https://notes.example/_yoetz"));
        assert!(!is_yoetz_owned_url("about:blank"));
        assert!(!is_yoetz_owned_url(""));
    }

    #[test]
    fn yoetz_run_id_from_url_extracts_exact_marker_value() {
        assert_eq!(
            yoetz_run_id_from_url("https://chatgpt.com/?_yoetz=run-123").as_deref(),
            Some("run-123")
        );
        assert_eq!(
            yoetz_run_id_from_url("https://chatgpt.com/c/abc?foo=bar&_yoetz=run-456").as_deref(),
            Some("run-456")
        );
        assert_eq!(
            yoetz_run_id_from_url("https://chatgpt.com/c/abc?foo=_yoetz=run-789").as_deref(),
            None
        );
    }

    #[test]
    fn context_tab_reuse_rank_prefers_disposable_tabs_before_yoetz_owned_chatgpt() {
        // Default recovery should prefer disposable pages before touching an
        // existing yoetz-owned ChatGPT tab from another run. Non-yoetz ChatGPT
        // tabs remain completely ineligible.
        assert_eq!(
            context_tab_reuse_rank("https://chatgpt.com/?_yoetz=20260418T073330Z_abc"),
            Some(2)
        );
        assert_eq!(
            context_tab_reuse_rank("https://chatgpt.com/c/xyz?foo=bar&_yoetz=run-abc"),
            Some(2)
        );
        assert_eq!(context_tab_reuse_rank("https://chatgpt.com/c/abc"), None);
        assert_eq!(context_tab_reuse_rank("https://chatgpt.com/"), None);
        assert_eq!(context_tab_reuse_rank("about:blank"), Some(0));
        assert_eq!(context_tab_reuse_rank("chrome://newtab/"), Some(1));
        assert_eq!(context_tab_reuse_rank("https://calendar.google.com"), None);
    }

    #[test]
    fn context_tab_navigation_reuse_rank_rejects_chatgpt_tabs() {
        assert_eq!(
            context_tab_navigation_reuse_rank("https://chatgpt.com/?_yoetz=run-abc"),
            None
        );
        assert_eq!(context_tab_navigation_reuse_rank("about:blank"), Some(0));
        assert_eq!(
            context_tab_navigation_reuse_rank("chrome://newtab/"),
            Some(1)
        );
    }

    #[test]
    fn duplicate_chatgpt_probe_target_ids_to_retire_keeps_only_selected_run_tab() {
        let targets = vec![
            TargetInfo {
                target_id: "target-1".to_string(),
                Type: "page".to_string(),
                title: "ChatGPT".to_string(),
                url: "https://chatgpt.com/?_yoetz=run-abc".to_string(),
                attached: false,
                opener_id: None,
                can_access_opener: false,
                opener_frame_id: None,
                parent_frame_id: None,
                browser_context_id: Some("ctx-personal".to_string()),
                subtype: None,
            },
            TargetInfo {
                target_id: "target-2".to_string(),
                Type: "page".to_string(),
                title: "ChatGPT".to_string(),
                url: "https://chatgpt.com/c/xyz?_yoetz=run-abc".to_string(),
                attached: false,
                opener_id: None,
                can_access_opener: false,
                opener_frame_id: None,
                parent_frame_id: None,
                browser_context_id: Some("ctx-personal".to_string()),
                subtype: None,
            },
            TargetInfo {
                target_id: "target-3".to_string(),
                Type: "page".to_string(),
                title: "ChatGPT".to_string(),
                url: "https://chatgpt.com/?_yoetz=run-other".to_string(),
                attached: false,
                opener_id: None,
                can_access_opener: false,
                opener_frame_id: None,
                parent_frame_id: None,
                browser_context_id: Some("ctx-personal".to_string()),
                subtype: None,
            },
        ];
        assert_eq!(
            duplicate_chatgpt_probe_target_ids_to_retire(&targets, "run-abc", "target-2"),
            vec!["target-1".to_string()]
        );
    }

    #[test]
    fn recovery_prefers_safe_anchor_before_existing_chatgpt_tab() {
        assert_eq!(
            choose_chatgpt_recovery_strategy(
                true,
                Some("about:blank"),
                None,
                "https://chatgpt.com/?_yoetz=test"
            )
            .unwrap(),
            ChatgptRecoveryStrategy::ExistingSafeAnchor
        );
    }

    #[test]
    #[serial_test::serial]
    fn recovery_can_reuse_existing_chatgpt_tab_when_operator_opted_in() {
        let previous = std::env::var(YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB_ENV).ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB_ENV, "1");
        }
        let strategy = choose_chatgpt_recovery_strategy(
            true,
            Some("about:blank"),
            None,
            "https://chatgpt.com/?_yoetz=test",
        )
        .unwrap();
        match previous {
            Some(value) => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var(YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB_ENV, value);
                }
            }
            None => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::remove_var(YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB_ENV);
                }
            }
        }
        assert_eq!(strategy, ChatgptRecoveryStrategy::ReuseExistingChatgptTab);
    }

    #[test]
    #[serial_test::serial]
    fn recovery_uses_existing_chatgpt_anchor_only_with_user_tab_opt_in() {
        let previous = std::env::var(YOETZ_ALLOW_USER_TAB_ANCHOR_ENV).ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(YOETZ_ALLOW_USER_TAB_ANCHOR_ENV, "1");
        }
        let strategy =
            choose_chatgpt_recovery_strategy(true, None, None, "https://chatgpt.com/?_yoetz=test")
                .unwrap();
        match previous {
            Some(value) => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var(YOETZ_ALLOW_USER_TAB_ANCHOR_ENV, value);
                }
            }
            None => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::remove_var(YOETZ_ALLOW_USER_TAB_ANCHOR_ENV);
                }
            }
        }
        assert_eq!(strategy, ChatgptRecoveryStrategy::ExistingChatgptAnchor);
    }

    #[test]
    fn recovery_never_touches_non_yoetz_safe_anchors() {
        let err = choose_chatgpt_recovery_strategy(
            false,
            Some("https://meet.google.com/abc-defg-hij"),
            None,
            "https://chatgpt.com/?_yoetz=test",
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("yoetz-safe"));
        assert!(msg.contains("without touching user tabs"));
    }

    #[test]
    fn missing_browser_context_error_matches_protocol_failure() {
        let err = anyhow!(
            "Method call error -32000: Failed to find browser context with id 643B374AEC2C8D298C82ECA3146A7330"
        );
        assert!(is_missing_browser_context_error(&err));
    }

    #[test]
    fn build_window_open_expression_serializes_url() {
        let expression =
            build_window_open_expression("https://chatgpt.com/?q=taboola&note=\"pro\"").unwrap();
        assert_eq!(
            expression,
            "window.open(\"https://chatgpt.com/?q=taboola&note=\\\"pro\\\"\", '_blank');"
        );
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
    fn find_marked_file_input_uid_only_matches_composer_marker() {
        // Even if a page has several hidden file inputs, only the one the
        // composer-scope helper tagged should be returned — the rest are
        // unrelated (e.g. avatar uploads, hidden dropzones).
        let snapshot = Snapshot {
            raw: json!({
                "id": "root",
                "role": "root_web_area",
                "name": "ChatGPT",
                "children": [
                    {
                        "id": "stray-upload",
                        "role": "textbox",
                        "tag": "input",
                        "type": "file",
                        "name": "avatar"
                    },
                    {
                        "id": "composer-upload",
                        "role": "textbox",
                        "tag": "input",
                        "type": "file",
                        "name": crate::chatgpt_web::COMPOSER_FILE_INPUT_MARKER
                    }
                ]
            }),
        };
        assert_eq!(
            snapshot.find_marked_file_input_uid(crate::chatgpt_web::COMPOSER_FILE_INPUT_MARKER),
            Some("composer-upload".to_owned())
        );
    }

    #[test]
    fn find_marked_file_input_uid_returns_none_when_unmarked() {
        // Without the scope marker, we must refuse to guess — callers should
        // keep polling rather than injecting into a random file input.
        let snapshot = Snapshot {
            raw: json!({
                "id": "root",
                "role": "root_web_area",
                "name": "ChatGPT",
                "children": [
                    {
                        "id": "stray-upload",
                        "role": "textbox",
                        "tag": "input",
                        "type": "file",
                        "name": "avatar"
                    }
                ]
            }),
        };
        assert_eq!(
            snapshot.find_marked_file_input_uid(crate::chatgpt_web::COMPOSER_FILE_INPUT_MARKER),
            None
        );
        assert_eq!(snapshot.find_marked_file_input_uid(""), None);
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
    fn validate_discovered_ws_host_accepts_same_host() {
        let origin = Url::parse("http://127.0.0.1:9222/").unwrap();
        validate_discovered_ws_host("ws://127.0.0.1:9222/devtools/browser/abc", &origin)
            .expect("same host/port must pass");
    }

    #[test]
    #[serial_test::serial]
    fn validate_discovered_ws_host_rejects_cross_host_redirect_by_default() {
        // The websocket URL returned by `/json/version` must live on the same
        // host:port we probed, otherwise we refuse to attach (review finding
        // #5 — CDP discovery must not follow arbitrary cross-host redirects).
        let previous = std::env::var(YOETZ_CDP_ALLOW_REMOTE_ENV).ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(YOETZ_CDP_ALLOW_REMOTE_ENV);
        }
        let origin = Url::parse("http://127.0.0.1:9222/").unwrap();
        let err = validate_discovered_ws_host(
            "ws://attacker.example.com:9222/devtools/browser/abc",
            &origin,
        )
        .expect_err("cross-host redirect must be rejected by default");
        let message = format!("{err:#}");
        assert!(message.contains("different host/port"));
        assert!(message.contains(YOETZ_CDP_ALLOW_REMOTE_ENV));
        if let Some(value) = previous {
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var(YOETZ_CDP_ALLOW_REMOTE_ENV, value);
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn validate_discovered_ws_host_allows_remote_when_opted_in() {
        // Operators can override with YOETZ_CDP_ALLOW_REMOTE=1 for legitimate
        // split-host setups (tunnels, remote Chrome for Testing, etc.).
        let previous = std::env::var(YOETZ_CDP_ALLOW_REMOTE_ENV).ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(YOETZ_CDP_ALLOW_REMOTE_ENV, "1");
        }
        let origin = Url::parse("http://127.0.0.1:9222/").unwrap();
        validate_discovered_ws_host("ws://remote.example.com:9999/devtools/browser/abc", &origin)
            .expect("opt-in should allow cross-host");
        match previous {
            Some(value) => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var(YOETZ_CDP_ALLOW_REMOTE_ENV, value);
                }
            }
            None => {
                #[allow(unsafe_code)]
                unsafe {
                    std::env::remove_var(YOETZ_CDP_ALLOW_REMOTE_ENV);
                }
            }
        }
    }

    #[test]
    fn is_loopback_host_accepts_loopback_aliases() {
        assert!(is_loopback_host(Some("localhost")));
        assert!(is_loopback_host(Some("LocalHost")));
        assert!(is_loopback_host(Some("127.0.0.1")));
        assert!(is_loopback_host(Some("::1")));
        assert!(is_loopback_host(Some("0.0.0.0")));
        assert!(!is_loopback_host(Some("attacker.example.com")));
        assert!(!is_loopback_host(Some("10.0.0.5")));
        assert!(!is_loopback_host(None));
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
    fn devtools_active_port_file_is_healthy_when_approval_mode_hides_json() {
        let dir = tempdir().unwrap();
        let active_port = dir.path().join("Google/Chrome/DevToolsActivePort");
        let (port, shutdown, handle) = spawn_approval_mode_devtools_server();
        write_devtools_active_port(&active_port, port, "approval-mode");

        let file =
            devtools_active_port_file_from_path_with_approval_probe(&active_port, &|_, _| true)
                .unwrap();

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();

        let expected = format!("ws://127.0.0.1:{port}/devtools/browser/approval-mode");
        assert_eq!(file.ws_endpoint.as_deref(), Some(expected.as_str()));
        assert!(
            file.healthy,
            "approval-mode Chrome hides /json endpoints but the browser websocket is still attachable after user approval"
        );
    }

    #[test]
    fn devtools_active_port_file_rejects_non_chromium_tcp_listener() {
        let dir = tempdir().unwrap();
        let active_port = dir.path().join("Google/Chrome/DevToolsActivePort");
        let (port, shutdown, handle) = spawn_approval_mode_devtools_server();
        write_devtools_active_port(&active_port, port, "not-chrome");

        let file = devtools_active_port_file_from_path(&active_port).unwrap();

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();

        let expected = format!("ws://127.0.0.1:{port}/devtools/browser/not-chrome");
        assert_eq!(file.ws_endpoint.as_deref(), Some(expected.as_str()));
        assert!(
            !file.healthy,
            "a stale DevToolsActivePort file must not become healthy just because another localhost process reused the port"
        );
    }

    #[test]
    fn resolve_any_devtools_active_port_ws_accepts_approval_mode_endpoint() {
        let home = tempdir().unwrap();
        let (stale_path, healthy_path) = devtools_active_port_test_paths(home.path());
        let (port, shutdown, handle) = spawn_approval_mode_devtools_server();
        write_devtools_active_port(&stale_path, port + 1, "stale");
        write_devtools_active_port(&healthy_path, port, "approval-mode");

        let resolved = resolve_any_devtools_active_port_ws_from_candidates_with_approval_probe(
            &[stale_path.clone(), healthy_path.clone()],
            &|_, endpoint| endpoint.ends_with("/approval-mode"),
        );

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();

        let expected = format!("ws://127.0.0.1:{port}/devtools/browser/approval-mode");
        assert_eq!(resolved.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn connect_to_running_chrome_times_out_when_websocket_handshake_stalls() {
        let _timeout_guard = EnvVarGuard::set("YOETZ_CDP_WS_HANDSHAKE_TIMEOUT_MS", "200");
        let (port, shutdown, handle) = spawn_stalled_websocket_server();
        let ws_endpoint = format!("ws://127.0.0.1:{port}/devtools/browser/stalled");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let started = Instant::now();
        let err =
            match runtime.block_on(CdpMcpClient::connect_to_running_chrome(Some(&ws_endpoint))) {
                Ok(_) => panic!("stalled websocket handshake should time out"),
                Err(err) => err,
            };
        let elapsed = started.elapsed();

        shutdown.store(true, Ordering::Relaxed);
        let _ = handle.join();

        assert!(
            elapsed < Duration::from_secs(5),
            "expected stalled handshake to fail quickly, took {elapsed:?}"
        );
        let detail = format!("{err:#}");
        assert!(
            detail.contains("connecting to Chrome websocket")
                || detail.contains("timed out connecting to Chrome websocket")
                || detail.contains("Resource temporarily unavailable"),
            "unexpected error detail: {detail}"
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
                "/Users/test/Library/Application Support/Google/Chrome for Testing/DevToolsActivePort"
            )),
            "Chrome for Testing"
        );
        assert_eq!(
            browser_name_from_source_path(Path::new(
                "/Users/test/Library/Application Support/Microsoft Edge/DevToolsActivePort"
            )),
            "Edge"
        );
        assert_eq!(
            browser_name_from_source_path(Path::new(
                "/Users/test/Library/Application Support/Arc/DevToolsActivePort"
            )),
            "Arc"
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
        assert!(!is_chatgpt_url("https://example.com/?ref=chatgpt.com"));
        assert!(!is_chatgpt_url("not-a-url"));
        assert!(!is_chatgpt_url("https://example.com/"));
        assert!(is_chatgpt_title(Some("ChatGPT - New chat")));
        assert!(is_chatgpt_title(Some("ChatGPT Workspace")));
        assert!(!is_chatgpt_title(Some("Notes about ChatGPT pricing")));
        assert!(!is_chatgpt_title(Some("Docs")));
    }

    #[test]
    fn snapshot_script_clears_previous_snapshot_ids_before_reassigning() {
        let script = build_snapshot_script(false);
        assert!(script.contains("querySelectorAll(\"[data-yoetz-snapshot-id]\")"));
        assert!(script.contains("removeAttribute(\"data-yoetz-snapshot-id\")"));
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
        assert_eq!(targets[0].page_target_count, 1);
        assert_eq!(
            targets[0].page_samples,
            vec!["ChatGPT - New chat".to_string()]
        );
    }

    #[test]
    fn parse_local_chromium_process_line_tracks_remote_debugging_and_user_data_dir() {
        let summary = parse_local_chromium_process_line(
            "2706 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome --remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug",
        )
        .unwrap();
        assert_eq!(summary.pid, 2706);
        assert_eq!(summary.browser_name, "Chrome");
        assert!(summary.has_remote_debugging);
        assert_eq!(summary.user_data_dir.as_deref(), Some("/tmp/chrome-debug"));
    }

    #[test]
    fn parse_local_chromium_process_line_preserves_user_data_dir_with_spaces() {
        let summary = parse_local_chromium_process_line(
            "2706 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome --user-data-dir=\"/Users/Test User/chrome debug\" --remote-debugging-port=9222",
        )
        .unwrap();
        assert_eq!(
            summary.user_data_dir.as_deref(),
            Some("/Users/Test User/chrome debug")
        );
    }

    #[test]
    fn parse_local_chromium_process_line_reads_unquoted_path_until_next_flag() {
        let summary = parse_local_chromium_process_line(
            "2706 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome --user-data-dir=/Users/Test User/chrome debug --remote-debugging-port=9222",
        )
        .unwrap();
        assert_eq!(
            summary.user_data_dir.as_deref(),
            Some("/Users/Test User/chrome debug")
        );
    }

    #[test]
    fn parse_local_chromium_process_line_ignores_helper_processes() {
        let helper = parse_local_chromium_process_line(
            "3188 /Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper --type=gpu-process",
        );
        assert!(helper.is_none());
    }
}

//! ChatGPT Pro recipe over chrome-devtools-mcp.
//!
//! ## Approach (revised after Agent Y DOM research)
//!
//! ChatGPT's DOM has three stable anchors that predate any recent redesigns:
//!
//! - `#prompt-textarea` — the ProseMirror composer contenteditable
//! - `[data-testid='send-button']` — the send button (testid has churned
//!   through `fruitjuice-send-button` and back, so we keep a fallback chain)
//! - `[data-message-author-role='assistant']` — the assistant message container
//!
//! None of these have reliable a11y role + accessible name pairs — the names
//! are locale-dependent and the role values vary. So we drive the page via
//! `evaluate_script` using CSS selectors instead of the uid-from-snapshot
//! model. This is still a massive simplification over the old dev-browser
//! recipe because:
//!
//! - No Playwright `connectOverCDP` hang in the live-attach path
//! - No QuickJS sandbox — `evaluate_script` runs plain JS in the page
//! - Every yoetz request opens a fresh yoetz-owned ChatGPT tab marked with a
//!   run-specific `?_yoetz=` query param and matching `window.name`
//! - First-class `upload_file` replaces the macOS clipboard-paste hack
//!
//! The one place we use `chrome-devtools-mcp`'s uid model is `upload_file`:
//! we take a snapshot AFTER clicking the attach button (which lazily mounts
//! the `<input type="file">` element), find the upload input's uid, and call
//! `upload_file` directly against that uid. Everything else is JS.
//!
//! ## Response completion (stable-idle heuristic, ported from v0.2.33)
//!
//! "Regenerate" button matching is unreliable (community reports of it
//! disappearing, moving into kebab menus, absent in Custom GPT / Canvas
//! flows). Instead, we poll `.result-streaming` absence on the last
//! assistant message plus stability of `(messageCount, textLength)` across
//! N consecutive polls. This matches the heuristic yoetz's old Pro Extended
//! auto-poll already uses.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::IsTerminal;
use std::time::Duration;

use super::client::{is_external_create_target_block_error, ChromeCdpClient};
use super::DevtoolsMcpRecipeContext;
use crate::chatgpt_recipe::AnyhowResultExt;
use crate::{chatgpt_recipe, chatgpt_web};

/// Stable-idle polling parameters.
const STABLE_IDLE_CONSECUTIVE_POLLS: u32 = 12;
const EAGER_FILE_INPUT_VERIFY_TIMEOUT_MS: u64 = 15_000;
const WAIT_FOR_COMPOSER_JS_TEMPLATE: &str = r##"
async () => {
  const focusComposer = __YOETZ_FOCUS_COMPOSER__;
  const deadline = Date.now() + 20000;
  const clip = (value, max = 240) =>
    String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
  const readState = () => {
    const composer = document.querySelector("#prompt-textarea, div[contenteditable='true'][role='textbox']");
    const url = window.location.href || "";
    const title = document.title || "";
    const bodyText = clip(document.body?.innerText || "");
    const haystack = `${title} ${bodyText}`.toLowerCase();
    if (composer) {
      if (focusComposer) composer.focus();
      return { status: "ready", url, title, bodyText };
    }
    if (/cloudflare|checking your browser|attention required|security check|just a moment|verify you are human|cf-chl/i.test(haystack)) {
      return { status: "challenge", url, title, bodyText };
    }
    if (
      /log in|login|sign in|sign up|create account|continue with google|continue with microsoft|continue with apple/i.test(haystack) ||
      /auth\.openai\.com|\/auth\/|\/login/.test(url.toLowerCase())
    ) {
      return { status: "login", url, title, bodyText };
    }
    return { status: "pending", url, title, bodyText };
  };

  let state = readState();
  if (state.status !== "pending") return state;

  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 200));
    state = readState();
    if (state.status !== "pending") return state;
  }

  state.status = "timeout";
  return state;
}
"##;

fn build_wait_for_composer_script(focus_composer: bool) -> String {
    WAIT_FOR_COMPOSER_JS_TEMPLATE.replace(
        "__YOETZ_FOCUS_COMPOSER__",
        if focus_composer { "true" } else { "false" },
    )
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChatgptRunResult {
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: chatgpt_recipe::ChatgptModelSelectionStatus,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ModelSelectionOutcome {
    model_used: Option<String>,
    model_selection_status: chatgpt_recipe::ChatgptModelSelectionStatus,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct ResponseBaseline {
    assistant_count: i64,
    assistant_last_len: i64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResponsePollState {
    assistant_count: i64,
    assistant_last_len: i64,
    text: String,
    streaming: bool,
    send_state: ResponseSendState,
    has_stop_button: bool,
    has_thinking_indicator: bool,
    copy_button_count: usize,
    error: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ResponseCompletionVerdict {
    Generating,
    CopyButton,
    Idle,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ResponseSendState {
    Enabled,
    Disabled,
    Missing,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InitialPageOpenMode {
    CreateTarget,
    RecoverViaExistingAnchor,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReconnectPolicy {
    Never,
    OneStandaloneRetry,
}

impl InitialPageOpenMode {
    fn debug_strategy(self) -> &'static str {
        match self {
            Self::CreateTarget => "create_target",
            Self::RecoverViaExistingAnchor => "recover_via_existing_anchor",
        }
    }
}

pub fn retry_initial_page_open_mode() -> InitialPageOpenMode {
    InitialPageOpenMode::RecoverViaExistingAnchor
}

pub fn should_recover_initial_page_open_after_reconnect(err: &anyhow::Error) -> bool {
    should_retry_initial_new_page_after_reconnect(err) || is_external_create_target_block_error(err)
}

/// The full ChatGPT Pro recipe. Returns the assistant's final response text.
pub async fn run(ctx: &DevtoolsMcpRecipeContext) -> Result<ChatgptRunResult> {
    if ctx.bundle_path.is_none() {
        return Err(anyhow!(
            "ChatGPT recipe requires `--bundle`; this transport uploads a file attachment and does not support paste mode"
        ));
    }

    run_attached_recipe_with_reconnect(ctx)
        .await
        .map_err(crate::browser::mark_chatgpt_attached_page_error)
}

async fn run_attached_recipe_with_reconnect(
    ctx: &DevtoolsMcpRecipeContext,
) -> Result<ChatgptRunResult> {
    let client = connect_client(ctx).await?;
    match run_attached_recipe(client, ctx, InitialPageOpenMode::CreateTarget).await {
        Ok(result) => Ok(result),
        Err(err) if should_recover_initial_page_open_after_reconnect(&err) => {
            emit_transport_retry_notice(ctx);
            debug_phase(
                "new_page transport failed during initial CreateTarget; reconnecting once and falling back to existing-anchor recovery",
            );
            let retry_client = connect_client(ctx).await?;
            run_attached_recipe(retry_client, ctx, retry_initial_page_open_mode())
                .await
                .context("retrying ChatGPT page open after reconnect")
                .context(err)
        }
        Err(err) => Err(err),
    }
}

async fn connect_client(ctx: &DevtoolsMcpRecipeContext) -> Result<ChromeCdpClient> {
    connect_client_with_attach_attempt_lock(ctx.cdp_endpoint.as_deref(), ctx.show_approval_guidance)
        .await
}

pub async fn connect_client_with_attach_attempt_lock(
    cdp_endpoint: Option<&str>,
    show_approval_guidance: bool,
) -> Result<ChromeCdpClient> {
    // Step 0: attach directly to the user's running Chrome session.
    // Chrome may show an "Allow remote debugging?" dialog here whenever it
    // treats this as a new external attach request. Serialize only the attach
    // itself so approval prompts do not race across yoetz processes.
    let client = with_attach_attempt_lock(show_approval_guidance, || async {
        ChromeCdpClient::connect_to_running_chrome(cdp_endpoint)
            .await
            .map_err(cdp_attach_hint)
    })
    .await?;
    debug_phase("phase=attach ok");
    Ok(client)
}

async fn run_attached_recipe(
    mut client: ChromeCdpClient,
    ctx: &DevtoolsMcpRecipeContext,
    page_open_mode: InitialPageOpenMode,
) -> Result<ChatgptRunResult> {
    let browser_context_id = client
        .resolve_browser_context_id(
            ctx.browser_context_id.as_deref(),
            ctx.profile_email.as_deref(),
        )
        .with_context(|| match (
            ctx.browser_context_id.as_deref(),
            ctx.profile_email.as_deref(),
        ) {
            (Some(context_id), Some(email)) => format!(
                "resolve Chrome browser context for browser_context_id `{context_id}` / profile_email `{email}`"
            ),
            (Some(context_id), None) => {
                format!("resolve Chrome browser context for browser_context_id `{context_id}`")
            }
            (None, Some(email)) => {
                format!("resolve Chrome browser context for profile_email `{email}`")
            }
            (None, None) => "resolve Chrome browser context".to_string(),
        })?;

    run_attached_recipe_inner(
        &mut client,
        ctx,
        browser_context_id,
        page_open_mode,
        ReconnectPolicy::OneStandaloneRetry,
    )
    .await
}

pub async fn run_with_client(
    client: &mut ChromeCdpClient,
    ctx: &DevtoolsMcpRecipeContext,
    browser_context_id: Option<String>,
) -> Result<ChatgptRunResult> {
    run_with_client_using_page_open_mode(
        client,
        ctx,
        browser_context_id,
        InitialPageOpenMode::CreateTarget,
    )
    .await
}

pub async fn run_with_client_using_page_open_mode(
    client: &mut ChromeCdpClient,
    ctx: &DevtoolsMcpRecipeContext,
    browser_context_id: Option<String>,
    page_open_mode: InitialPageOpenMode,
) -> Result<ChatgptRunResult> {
    run_with_client_using_page_open_mode_and_reconnect_policy(
        client,
        ctx,
        browser_context_id,
        page_open_mode,
        ReconnectPolicy::OneStandaloneRetry,
    )
    .await
}

pub async fn run_with_client_using_page_open_mode_and_reconnect_policy(
    client: &mut ChromeCdpClient,
    ctx: &DevtoolsMcpRecipeContext,
    browser_context_id: Option<String>,
    page_open_mode: InitialPageOpenMode,
    reconnect_policy: ReconnectPolicy,
) -> Result<ChatgptRunResult> {
    run_attached_recipe_inner(
        client,
        ctx,
        browser_context_id,
        page_open_mode,
        reconnect_policy,
    )
    .await
}

async fn run_attached_recipe_inner(
    client: &mut ChromeCdpClient,
    ctx: &DevtoolsMcpRecipeContext,
    browser_context_id: Option<String>,
    page_open_mode: InitialPageOpenMode,
    reconnect_policy: ReconnectPolicy,
) -> Result<ChatgptRunResult> {
    let marked_url = chatgpt_web::mark_chatgpt_url(&ctx.run_id);
    let page_id = open_initial_chatgpt_page(
        client,
        &marked_url,
        browser_context_id.as_deref(),
        page_open_mode,
    )
    .await
    .with_context(|| format!("chrome-devtools-mcp new_page on `{marked_url}`"))?;
    let set_window_name_js = chatgpt_web::build_set_window_name_js(&ctx.run_id);
    client
        .evaluate_script(&set_window_name_js, vec![])
        .await
        .context("mark yoetz-owned ChatGPT tab with window.name")?;

    // Step 2: wait for the composer to mount, then select the best model the
    // user can actually access. `model=auto` actively selects the strongest
    // visible Pro/GPT-5 option; an empty/current model keeps the current UI
    // selection.
    wait_for_composer_ready(client, /* focus_composer */ true).await?;
    let model_selection = maybe_select_model(client, &ctx.model).await?;
    if ctx.disable_extended {
        maybe_disable_extended(client).await?;
    }

    // Step 3: upload the bundle if we have a path.
    //
    // chrome-devtools-mcp's `upload_file` tool takes a uid of an
    // `<input type='file'>` element. That input is LAZILY MOUNTED in
    // chatgpt.com — it's not present until you interact with the attach
    // button. We click the attach button via JS to trigger the mount, then
    // `take_snapshot` to find the input's uid, then `upload_file` against
    // that uid.
    //
    let bundle_path = ctx
        .bundle_path
        .as_ref()
        .context("ChatGPT recipe requires a bundle file path")?;
    try_upload_bundle(client, bundle_path, ctx.upload_timeout_ms)
        .await
        .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Upload)
        .context("upload bundle to ChatGPT")?;

    // Step 4: type the delivery text into the focused composer.
    //
    // `type_text` types into the currently focused element, which we already
    // focused in Step 2.
    let delivery_text = ctx.prompt.clone();

    // Make sure the composer is still focused (upload step may have stolen
    // focus to the file picker).
    let composer_selector_json = chatgpt_web::composer_selector_json();
    let refocus_js = format!(
        r##"
() => {{
  const composer = document.querySelector({composer_selector_json});
  if (composer) composer.focus();
  return !!composer;
}}
"##
    );
    let _ = client
        .evaluate_script(&refocus_js, vec![])
        .await
        .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Send)
        .context("refocus composer after upload")?;

    client
        .type_text(&delivery_text, /* submit_key */ None)
        .await
        .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Send)
        .context("type_text into ChatGPT composer")?;

    let click_send_js = chatgpt_web::build_send_button_click_function();
    let clicked = client
        .evaluate_script(&click_send_js, vec![])
        .await
        .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Send)
        .context("evaluate_script click send button")?;
    if clicked.get("status").and_then(serde_json::Value::as_str) != Some("sent") {
        return Err(anyhow!(
            "could not find an enabled ChatGPT send button. diagnostics={}",
            clicked
                .get("diagnostics")
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        ))
        .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Send);
    }
    let response_baseline = parse_response_baseline(&clicked)
        .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::Send)
        .context("parse ChatGPT response baseline before send")?;

    // Step 6: stable-idle polling for response completion.
    //
    // Heuristic (ported from yoetz v0.2.33 Pro Extended auto-poll):
    // - Absence of `.result-streaming` class on the last assistant message
    // - (messageCount, textLength) unchanged across N consecutive polls
    //
    // This replaces the unreliable "Regenerate" button wait_for. Agent Y
    // research showed "Regenerate" is missing or inconsistently placed in
    // many ChatGPT flows (Custom GPT, Canvas, certain Pro modes).
    let response_text = poll_for_stable_response(
        client,
        ctx,
        &page_id,
        response_baseline,
        ctx.response_timeout_ms,
        ctx.response_poll_interval_ms,
        reconnect_policy,
    )
    .await
    .with_chatgpt_phase(chatgpt_recipe::ChatgptTransportPhase::WaitResponse)
    .context("stable-idle polling for ChatGPT response")?;

    Ok(ChatgptRunResult {
        response: response_text,
        model_used: model_selection.model_used,
        model_selection_status: model_selection.model_selection_status,
    })
}

async fn open_initial_chatgpt_page(
    client: &ChromeCdpClient,
    marked_url: &str,
    browser_context_id: Option<&str>,
    mode: InitialPageOpenMode,
) -> Result<String> {
    debug_phase(&format!(
        "phase=new_page strategy={} url={marked_url}",
        mode.debug_strategy()
    ));
    let page = match mode {
        InitialPageOpenMode::CreateTarget => {
            client
                .new_page(
                    marked_url,
                    /* background */ false,
                    30_000,
                    browser_context_id,
                )
                .await?
        }
        InitialPageOpenMode::RecoverViaExistingAnchor => {
            client
                .open_chatgpt_page_via_existing_anchor(
                    marked_url,
                    /* background */ false,
                    30_000,
                    browser_context_id,
                )
                .await
                .with_context(|| format!("recover ChatGPT page open for `{marked_url}`"))?
        }
    };
    debug_phase("phase=new_page ok");
    Ok(page.page_id)
}

async fn open_page_via_blank_target(
    client: &ChromeCdpClient,
    url: &str,
    background: bool,
    timeout_ms: u64,
    browser_context_id: Option<&str>,
) -> Result<()> {
    // On Chrome builds where CreateTarget itself still works, opening
    // `about:blank` first avoids touching user tabs and gives yoetz a clean
    // anchor before navigating to ChatGPT. When Chrome blocks CreateTarget
    // altogether, the recipe reconnect path recovers via existing anchors
    // instead of looping on more CreateTarget calls.
    client
        .new_page("about:blank", background, timeout_ms, browser_context_id)
        .await
        .context("create blank Chrome page before ChatGPT navigation")?;
    client
        .navigate_page(url, timeout_ms)
        .await
        .with_context(|| format!("navigate blank Chrome page to `{url}`"))?;
    Ok(())
}

async fn open_initial_chatgpt_probe_page(
    client: &ChromeCdpClient,
    url: &str,
    timeout_ms: u64,
    browser_context_id: Option<&str>,
    page_open_mode: InitialPageOpenMode,
) -> Result<()> {
    match page_open_mode {
        InitialPageOpenMode::CreateTarget => {
            match open_page_via_blank_target(
                client,
                url,
                /* background */ true,
                timeout_ms,
                browser_context_id,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(err) if is_external_create_target_block_error(&err) => client
                    .open_chatgpt_page_via_existing_anchor(
                        url,
                        /* background */ true,
                        timeout_ms,
                        browser_context_id,
                    )
                    .await
                    .with_context(|| format!("recover auth probe page open for `{url}`"))
                    .map(|_| ()),
                Err(err) => Err(err),
            }
        }
        InitialPageOpenMode::RecoverViaExistingAnchor => client
            .open_chatgpt_page_via_existing_anchor(
                url,
                /* background */ true,
                timeout_ms,
                browser_context_id,
            )
            .await
            .with_context(|| {
                format!(
                    "open ChatGPT control tab for `{url}` from an existing safe anchor without Target.createTarget"
                )
            })
            .map(|_| ()),
    }
}

pub async fn check_auth(cdp_endpoint: Option<&str>, show_approval_guidance: bool) -> Result<()> {
    check_auth_with_control_run_id(cdp_endpoint, show_approval_guidance, None).await
}

async fn open_chatgpt_auth_probe_page(
    client: &ChromeCdpClient,
    browser_context_id: Option<&str>,
    control_run_id: Option<&str>,
    timeout_ms: u64,
    page_open_mode: InitialPageOpenMode,
) -> Result<()> {
    let url = control_run_id
        .map(chatgpt_web::mark_chatgpt_url)
        .unwrap_or_else(|| chatgpt_web::CHATGPT_URL.to_string());
    open_initial_chatgpt_probe_page(client, &url, timeout_ms, browser_context_id, page_open_mode)
        .await
        .with_context(|| format!("chrome-devtools-mcp open auth probe on `{url}`"))?;
    if let Some(control_run_id) = control_run_id {
        let set_window_name_js = chatgpt_web::build_set_window_name_js(control_run_id);
        client
            .evaluate_script(&set_window_name_js, vec![])
            .await
            .context("mark ChatGPT control tab with window.name")?;
    }
    Ok(())
}

pub async fn check_auth_with_control_run_id(
    cdp_endpoint: Option<&str>,
    show_approval_guidance: bool,
    control_run_id: Option<&str>,
) -> Result<()> {
    let client =
        connect_client_with_attach_attempt_lock(cdp_endpoint, show_approval_guidance).await?;
    ensure_chatgpt_control_tab_ready(&client, None, control_run_id).await
}

pub async fn ensure_chatgpt_control_tab_ready(
    client: &ChromeCdpClient,
    browser_context_id: Option<&str>,
    control_run_id: Option<&str>,
) -> Result<()> {
    ensure_chatgpt_control_tab_ready_with_open_mode(
        client,
        browser_context_id,
        control_run_id,
        InitialPageOpenMode::CreateTarget,
    )
    .await
}

pub async fn ensure_chatgpt_control_tab_ready_with_open_mode(
    client: &ChromeCdpClient,
    browser_context_id: Option<&str>,
    control_run_id: Option<&str>,
    page_open_mode: InitialPageOpenMode,
) -> Result<()> {
    let reused_existing_page = client
        .select_chatgpt_page_for_probe(30_000, browser_context_id, control_run_id)
        .await
        .context("select existing ChatGPT page for auth probe")?
        .is_some();
    if !reused_existing_page {
        open_chatgpt_auth_probe_page(
            client,
            browser_context_id,
            control_run_id,
            30_000,
            page_open_mode,
        )
        .await?;
    }
    let result = wait_for_composer_ready(client, /* focus_composer */ false).await;
    if reused_existing_page && result.is_err() {
        if should_retire_failed_reused_control_tab(control_run_id, &result) {
            let _ = client.close_selected_page(true);
        }
        open_chatgpt_auth_probe_page(
            client,
            browser_context_id,
            control_run_id,
            30_000,
            page_open_mode,
        )
        .await?;
        let retry = wait_for_composer_ready(client, /* focus_composer */ false).await;
        if control_run_id.is_none() {
            let _ = client.close_selected_page(true);
        }
        return retry;
    }
    if !reused_existing_page && control_run_id.is_none() {
        let _ = client.close_selected_page(true);
    }
    result
}

fn should_retire_failed_reused_control_tab(
    control_run_id: Option<&str>,
    result: &Result<()>,
) -> bool {
    control_run_id.is_some() && result.is_err()
}

async fn wait_for_composer_ready(client: &ChromeCdpClient, focus_composer: bool) -> Result<()> {
    let composer_state = evaluate_wait_for_composer_state(client, focus_composer).await?;
    match composer_state
        .get("status")
        .and_then(serde_json::Value::as_str)
    {
        Some("ready") => Ok(()),
        Some("challenge") => Err(anyhow!(
            "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again. {}",
            format_page_probe_summary(&composer_state)
        )),
        Some("login") => Err(anyhow!(
            "chatgpt login required in the attached Chrome session. Log in there and try again. {}",
            format_page_probe_summary(&composer_state)
        )),
        _ => {
            let detail = match classify_live_chatgpt_page_issue(
                composer_state
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
                composer_state
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
                composer_state
                    .get("bodyText")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            ) {
                Some(issue) => issue.to_string(),
                None => {
                    "ChatGPT composer did not mount within 20s — page may not have loaded correctly"
                        .to_string()
                }
            };
            Err(anyhow!(
                "{detail}. {}",
                format_page_probe_summary(&composer_state)
            ))
        }
    }
}

async fn evaluate_wait_for_composer_state(
    client: &ChromeCdpClient,
    focus_composer: bool,
) -> Result<serde_json::Value> {
    let script = build_wait_for_composer_script(focus_composer);
    let mut last_err = None;
    for attempt in 0..3 {
        match client.evaluate_script(&script, vec![]).await {
            Ok(state) => return Ok(state),
            Err(err) => {
                let retryable = should_retry_wait_for_composer_error(&err);
                last_err = Some(err);
                if retryable && attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                break;
            }
        }
    }
    Err(last_err.expect("wait-for-composer attempts should record an error"))
        .context("evaluate_script wait-for-composer")
}

fn should_retry_wait_for_composer_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("execution context")
        || message.contains("context with specified id")
        || message.contains("cannot find context")
        || message.contains("cannot find object with id")
        || message.contains("frame not found")
        || message.contains("target closed")
        || message.contains("session closed")
        || message.contains("navigation")
}

async fn maybe_select_model(
    client: &ChromeCdpClient,
    requested_model: &str,
) -> Result<ModelSelectionOutcome> {
    let keep_current_model = chatgpt_web::should_keep_current_chatgpt_model(requested_model);
    let script = build_model_selection_script(requested_model);
    let selection = client
        .evaluate_script(&script, vec![])
        .await
        .context("evaluate_script select model")?;
    let status = selection
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    debug_phase(&format!(
        "phase=select-model status={status} payload={selection}"
    ));
    let model_used = chatgpt_web::select_reported_chatgpt_model(&selection, requested_model);
    let model_selection_status =
        chatgpt_web::chatgpt_model_selection_status(&selection, requested_model);
    match status {
        "selected" | "already-selected" => Ok(ModelSelectionOutcome {
            model_used,
            model_selection_status,
        }),
        "missing-selector" | "not-found" if keep_current_model => Ok(ModelSelectionOutcome {
            model_used,
            model_selection_status,
        }),
        "missing-selector" => Err(anyhow!(
            "ChatGPT model selector button not found. {}",
            format_page_probe_summary(&selection)
        )),
        "selection-mismatch" => Err(anyhow!(
            "requested ChatGPT model `{}` was not actually selected.{} {}",
            selection
                .get("requested")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(requested_model),
            format_model_selection_diagnostics(&selection),
            format_page_probe_summary(&selection)
        )),
        "not-found" => {
            let requested = selection
                .get("requested")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(requested_model);
            let available = selection
                .get("availableItems")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .collect::<Vec<_>>()
                })
                .filter(|items| !items.is_empty())
                .map(|items| format!(" Available options: {}.", items.join(", ")))
                .unwrap_or_default();
            Err(anyhow!(
                "requested ChatGPT model `{requested}` was not available in the current session.{available} {}",
                format_page_probe_summary(&selection)
            ))
        }
        other => Err(anyhow!(
            "unexpected ChatGPT model selection status `{other}`"
        )),
    }
}

fn format_model_selection_diagnostics(selection: &serde_json::Value) -> String {
    let selected_label = selection
        .get("selectedLabel")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" selected_label={value:?};"))
        .unwrap_or_default();
    let target_test_id = selection
        .get("targetTestId")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" target_testid={value:?};"))
        .unwrap_or_default();
    let target_checked = selection
        .get("targetChecked")
        .and_then(serde_json::Value::as_bool)
        .map(|value| format!(" target_checked={value};"))
        .unwrap_or_default();
    let menu_reopen_attempts = selection
        .get("menuReopenAttempts")
        .and_then(serde_json::Value::as_u64)
        .map(|value| format!(" menu_reopen_attempts={value};"))
        .unwrap_or_default();
    let selector_expanded = selection
        .get("selectorExpanded")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" selector_expanded={value:?};"))
        .unwrap_or_default();
    let item_count = selection
        .get("itemCount")
        .and_then(serde_json::Value::as_u64)
        .map(|value| format!(" item_count={value};"))
        .unwrap_or_default();
    let available_items = selection
        .get("availableItemsAfter")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .map(|items| format!(" available_after=[{}];", items.join(", ")))
        .unwrap_or_default();

    format!(
        "{}{}{}{}{}{}{}",
        selected_label,
        target_test_id,
        target_checked,
        menu_reopen_attempts,
        selector_expanded,
        item_count,
        available_items
    )
}

async fn maybe_disable_extended(client: &ChromeCdpClient) -> Result<()> {
    let script = r##"
() => {
  const button = document.querySelector("button[aria-label*='click to remove'][aria-label*='Extended'], button[aria-label*='remove'][aria-label*='Extended']");
  if (!button) return "already-off";
  button.click();
  return "disabled";
}
"##;
    let _ = client
        .evaluate_script(script, vec![])
        .await
        .context("evaluate_script disable Extended Pro")?;
    Ok(())
}

fn parse_response_baseline(state: &serde_json::Value) -> Result<ResponseBaseline> {
    let assistant_count = state
        .get("assistantCountBeforeSend")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow!("missing assistantCountBeforeSend in send payload"))?;
    let assistant_last_len = state
        .get("assistantLastLenBeforeSend")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow!("missing assistantLastLenBeforeSend in send payload"))?;
    Ok(ResponseBaseline {
        assistant_count,
        assistant_last_len,
    })
}

fn build_model_selection_script(requested_model: &str) -> String {
    chatgpt_web::build_model_selection_function(requested_model)
}

/// Rewrite a CDP attach failure with actionable guidance.
///
/// Chrome 136+ ignores `--remote-debugging-port` on the default profile, so
/// the most common failure mode is "port 9222 not listening" — which surfaces
/// as a refused TCP connection or a non-2xx response on `/json/version`.
/// Instead of leaking the raw reqwest error, point the user at the fix.
fn cdp_attach_hint(err: anyhow::Error) -> anyhow::Error {
    let raw = format!("{err:#}");
    let raw_lower = raw.to_lowercase();
    // Preserve approval-dialog errors verbatim so the outer fallback funnel can
    // classify them as "stop, user needs to click Allow" rather than
    // "transport broken, try the next one."
    if raw_lower.contains("allow remote debugging") {
        return err.context("chrome-devtools-mcp attach to running Chrome");
    }
    if raw_lower.contains("timed out waiting for chrome websocket handshake")
        || (raw_lower.contains("connecting to chrome websocket")
            && raw_lower.contains("websocket handshake")
            && raw_lower.contains("timed out"))
    {
        return anyhow!("{}", approval_wait_message())
            .context("chrome-devtools-mcp attach to running Chrome")
            .context(err);
    }
    if raw_lower.contains("resource temporarily unavailable")
        && raw_lower.contains("connecting to chrome websocket")
    {
        return anyhow!("{}", approval_wait_message())
            .context("chrome-devtools-mcp attach to running Chrome")
            .context(err);
    }
    if raw_lower.contains("connecting to chrome websocket") && raw_lower.contains("403 forbidden") {
        return anyhow!(
            "Chrome rejected the remote debugging client (403 Forbidden). Chrome may still be showing an \"Allow remote debugging?\" dialog — click Allow and retry. If you intentionally blocked the dialog, re-enable remote debugging approval and try again."
        )
        .context("chrome-devtools-mcp attach to running Chrome")
        .context(err);
    }
    err.context(
        "chrome-devtools-mcp could not reach Chrome's CDP endpoint. \
         Chrome 136+ ignores --remote-debugging-port on the default profile — \
         either enable chrome://inspect/#remote-debugging (Chrome 144+) and retry, \
         or pass --cdp=ws://127.0.0.1:PORT after launching Chrome with a non-default \
         --user-data-dir, or use Chrome for Testing",
    )
}

fn emit_stable_idle_warning(message: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("{message}");
    }
}

fn approval_wait_message() -> String {
    let timeout_secs = configured_ws_handshake_timeout_ms() / 1_000;
    format!(
        "live browser attach timed out ({timeout_secs}s). Chrome may be showing an \"Allow remote debugging?\" dialog — click Allow, then retry."
    )
}

async fn with_attach_attempt_lock<T, F, Fut>(show_approval_guidance: bool, action: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    with_attach_attempt_lock_using(
        show_approval_guidance,
        crate::browser::acquire_attach_attempt_lock,
        action,
    )
    .await
}

trait AttachAttemptLockState {
    fn waited(&self) -> bool;
}

impl AttachAttemptLockState for crate::browser::AttachAttemptLock {
    fn waited(&self) -> bool {
        self.waited()
    }
}

async fn with_attach_attempt_lock_using<T, L, Acquire, F, Fut>(
    show_approval_guidance: bool,
    acquire_lock: Acquire,
    action: F,
) -> Result<T>
where
    L: AttachAttemptLockState,
    Acquire: FnOnce() -> Result<L>,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let attach_attempt_lock = acquire_lock()?;
    emit_chrome_attach_attempt_guidance(show_approval_guidance, attach_attempt_lock.waited());
    let _attach_attempt_lock = attach_attempt_lock;
    action().await
}

fn emit_chrome_attach_attempt_guidance(show_approval_guidance: bool, waited: bool) {
    if !show_approval_guidance {
        return;
    }
    if waited {
        eprintln!(
            "info: another yoetz process is already starting a Chrome attach attempt; waiting for it to finish before trying the chrome-devtools-mcp transport"
        );
    }
    eprintln!(
        "info: connecting to Chrome via chrome-devtools-mcp — if prompted, click Allow in Chrome's remote debugging dialog"
    );
}

fn configured_ws_handshake_timeout_ms() -> u64 {
    std::env::var("YOETZ_CDP_WS_HANDSHAKE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(30_000)
}

fn cdp_debug_enabled() -> bool {
    std::env::var(super::client::YOETZ_DEBUG_CDP_ENV)
        .map(|value| {
            let trimmed = value.trim();
            !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case("0")
                && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn debug_phase(message: &str) {
    if cdp_debug_enabled() {
        eprintln!("info: chrome-devtools-mcp {message}");
    }
}

fn is_closed_cdp_transport_error(err: &anyhow::Error) -> bool {
    super::client::is_closed_cdp_transport_error(err)
}

fn should_retry_initial_new_page_after_reconnect(err: &anyhow::Error) -> bool {
    let raw = format!("{err:#}");
    is_closed_cdp_transport_error(err)
        && !is_external_create_target_block_error(err)
        && raw.contains("chrome-devtools-mcp new_page on")
        && raw.contains("creating a new Chrome page for")
}

fn emit_transport_retry_notice(ctx: &DevtoolsMcpRecipeContext) {
    if ctx.show_approval_guidance || std::io::stderr().is_terminal() {
        eprintln!(
            "info: Chrome dropped the initial CDP session while opening the ChatGPT tab; this standalone flow reconnects once and falls back to existing-anchor recovery. Daemon-owned live attach is not reconnecting to preserve the single-approval invariant."
        );
    }
}

fn classify_live_chatgpt_page_issue(
    url: &str,
    title: &str,
    body_text: &str,
) -> Option<&'static str> {
    let haystack = format!("{title} {body_text}").to_lowercase();
    let url = url.to_lowercase();
    if [
        "cloudflare",
        "checking your browser",
        "attention required",
        "security check",
        "just a moment",
        "verify you are human",
        "cf-chl",
    ]
    .iter()
    .any(|marker| haystack.contains(marker))
    {
        return Some(
            "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again",
        );
    }

    if [
        "log in",
        "login",
        "sign in",
        "sign up",
        "create account",
        "continue with google",
        "continue with microsoft",
        "continue with apple",
    ]
    .iter()
    .any(|marker| haystack.contains(marker))
        || url.contains("auth.openai.com")
        || url.contains("/auth/")
        || url.contains("/login")
    {
        return Some(
            "chatgpt login required in the attached Chrome session. Log in there and try again",
        );
    }

    None
}

fn format_page_probe_summary(state: &serde_json::Value) -> String {
    let url = state
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let title = state
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    let title_part = if title.is_empty() {
        "title=<empty>".to_string()
    } else {
        format!("title={title:?}")
    };
    format!("Current page: url={url}, {title_part}")
}

/// Click the attach button, snapshot the mounted upload affordance, then call
/// `upload_file`. `upload_file` accepts either the file input itself or the
/// element that opens the file chooser, so prefer visible menu items/buttons
/// over guessing a hidden input uid.
async fn try_upload_bundle(
    client: &ChromeCdpClient,
    bundle_path: &std::path::Path,
    upload_timeout_ms: u64,
) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_millis(upload_timeout_ms);
    let file_name = bundle_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .context("bundle path must end in a UTF-8 filename")?;

    // Fast path: 0ms input wait falls through to click flow if no input, but
    // once an input is found and upload_file runs we still need real verify time.
    let (input_wait_ms, verify_timeout_ms) = eager_file_input_upload_timeouts();
    if try_upload_via_file_input(
        client,
        bundle_path,
        &file_name,
        input_wait_ms,
        verify_timeout_ms,
    )
    .await
    .context("eager file input upload")?
    {
        return Ok(());
    }

    let initial_diagnostics = collect_upload_diagnostics(client)
        .await
        .context("collect initial upload diagnostics")?;
    let candidates = initial_diagnostics
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    if candidates.is_empty() {
        return Err(anyhow!(
            "no attach/upload controls were detected near the ChatGPT composer.\n{}",
            format_upload_diagnostics(&initial_diagnostics)
        ));
    }

    let mut attempts = Vec::new();

    for candidate in candidates.iter().take(6) {
        let candidate_id = match candidate.get("id").and_then(serde_json::Value::as_str) {
            Some(value) if !value.is_empty() => value,
            _ => continue,
        };
        let label = describe_upload_candidate(candidate);

        let clicked = click_upload_candidate(client, candidate_id)
            .await
            .with_context(|| format!("click upload candidate {label}"))?;
        if !clicked {
            attempts.push(format!("candidate vanished before click: {label}"));
            continue;
        }

        attempts.push(format!("clicked candidate: {label}"));
        tokio::time::sleep(Duration::from_millis(300)).await;

        let remaining_ms = remaining_upload_timeout_ms(deadline);
        if remaining_ms == 0 {
            attempts.push("upload deadline exhausted before file input wait".to_string());
            break;
        }
        if try_upload_via_file_input(client, bundle_path, &file_name, remaining_ms, remaining_ms)
            .await
            .with_context(|| format!("upload after clicking {label}"))?
        {
            return Ok(());
        }

        let menu_clicked = click_upload_menu_item(client)
            .await
            .context("click upload menu item")?;
        if menu_clicked {
            attempts.push(format!("clicked upload menu after: {label}"));
            tokio::time::sleep(Duration::from_millis(300)).await;
            let remaining_ms = remaining_upload_timeout_ms(deadline);
            if remaining_ms == 0 {
                attempts.push("upload deadline exhausted before upload-menu wait".to_string());
                break;
            }
            if try_upload_via_file_input(
                client,
                bundle_path,
                &file_name,
                remaining_ms,
                remaining_ms,
            )
            .await
            .with_context(|| format!("upload after upload menu from {label}"))?
            {
                return Ok(());
            }
        }
    }

    let final_diagnostics = collect_upload_diagnostics(client)
        .await
        .context("collect final upload diagnostics")?;
    Err(anyhow!(
        "could not attach `{file_name}` to ChatGPT.\nAttempts:\n- {}\nInitial diagnostics:\n{}\nFinal diagnostics:\n{}",
        attempts.join("\n- "),
        format_upload_diagnostics(&initial_diagnostics),
        format_upload_diagnostics(&final_diagnostics),
    ))
}

fn remaining_upload_timeout_ms(deadline: std::time::Instant) -> u64 {
    deadline
        .saturating_duration_since(std::time::Instant::now())
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn eager_file_input_upload_timeouts() -> (u64, u64) {
    (0, EAGER_FILE_INPUT_VERIFY_TIMEOUT_MS)
}

async fn try_upload_via_file_input(
    client: &ChromeCdpClient,
    bundle_path: &std::path::Path,
    file_name: &str,
    input_wait_timeout_ms: u64,
    attachment_verify_timeout_ms: u64,
) -> Result<bool> {
    let input_uid = wait_for_file_input_uid(client, input_wait_timeout_ms)
        .await
        .context("wait for file input")?;

    let Some(input_uid) = input_uid else {
        return Ok(false);
    };

    client
        .upload_file(&input_uid, bundle_path)
        .await
        .with_context(|| format!("upload_file via input `{input_uid}`"))?;

    wait_for_attachment_visible(client, file_name, attachment_verify_timeout_ms)
        .await
        .with_context(|| format!("verify attachment chip for `{file_name}`"))?;

    Ok(true)
}

async fn wait_for_file_input_uid(
    client: &ChromeCdpClient,
    timeout_ms: u64,
) -> Result<Option<String>> {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    let scope_script = chatgpt_web::build_scope_composer_file_input_function();

    loop {
        // Tag the composer-scoped file input with our marker before taking a
        // snapshot. Without this scoping, a page-wide first-match walk can
        // return an unrelated hidden file input (review finding #10) and we'd
        // silently inject the bundle into the wrong element.
        let scope_result = client
            .evaluate_script(&scope_script, vec![])
            .await
            .context("scope composer file input before snapshot")?;
        let scope_status = scope_result
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");

        if scope_status == "marked" {
            let snapshot = client
                .take_snapshot(false)
                .await
                .context("take snapshot while searching for marked file input")?;
            if let Some(uid) =
                snapshot.find_marked_file_input_uid(chatgpt_web::COMPOSER_FILE_INPUT_MARKER)
            {
                return Ok(Some(uid));
            }
        }

        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn click_upload_candidate(client: &ChromeCdpClient, candidate_id: &str) -> Result<bool> {
    let candidate_json = serde_json::to_string(candidate_id)?;
    let script = format!(
        r#"() => {{
  const candidate = document.querySelector(`[data-yoetz-upload-candidate=${candidate_json}]`);
  if (!candidate) return false;
  candidate.click();
  return true;
}}"#
    );
    Ok(client.evaluate_script(&script, vec![]).await? == serde_json::Value::Bool(true))
}

async fn click_upload_menu_item(client: &ChromeCdpClient) -> Result<bool> {
    let script = chatgpt_web::build_upload_menu_item_click_function();
    Ok(client
        .evaluate_script(&script, vec![])
        .await?
        .get("status")
        .and_then(serde_json::Value::as_str)
        == Some("clicked"))
}

async fn collect_upload_diagnostics(client: &ChromeCdpClient) -> Result<serde_json::Value> {
    let script = build_collect_upload_diagnostics_script();

    client
        .evaluate_script(&script, vec![])
        .await
        .context("evaluate upload diagnostics")
}

fn build_collect_upload_diagnostics_script() -> String {
    r##"
() => {
  const composer = document.querySelector("#prompt-textarea, div[contenteditable='true'][role='textbox']");
  const composerRect = composer?.getBoundingClientRect() || null;
  const composerForm = composer?.closest("form") || null;

  document
    .querySelectorAll("[data-yoetz-upload-candidate]")
    .forEach((el) => el.removeAttribute("data-yoetz-upload-candidate"));

  const clip = (value, max = 160) =>
    String(value || "").replace(/\s+/g, " ").trim().slice(0, max);

  const isVisible = (el) => {
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return rect.width > 0 &&
      rect.height > 0 &&
      style.visibility !== "hidden" &&
      style.display !== "none" &&
      style.pointerEvents !== "none";
  };

  const distanceToComposer = (el) => {
    if (!composerRect) return 99999;
    const rect = el.getBoundingClientRect();
    const dx = rect.left + rect.width / 2 - (composerRect.left + composerRect.width / 2);
    const dy = rect.top + rect.height / 2 - (composerRect.top + composerRect.height / 2);
    return Math.round(Math.hypot(dx, dy));
  };

  const describe = (el) => {
    const text = clip(el.innerText || el.textContent || "");
    const ariaLabel = clip(el.getAttribute?.("aria-label") || "");
    const title = clip(el.getAttribute?.("title") || "");
    const testId = clip(el.getAttribute?.("data-testid") || "");
    const className = clip(el.getAttribute?.("class") || "");
    const id = clip(el.getAttribute?.("id") || "");
    const normalized = `${ariaLabel} ${title} ${testId} ${id} ${className} ${text}`.toLowerCase();
    const sameForm = !!composerForm && el.closest("form") === composerForm;
    const distance = distanceToComposer(el);
    const hasSvg = !!el.querySelector("svg");
    const sendLike = el.matches("[data-testid='send-button'], [data-testid='fruitjuice-send-button'], button[type='submit']") || /\bsend\b|\bsubmit\b/.test(normalized);
    let score = 0;
    if (sameForm) score += 60;
    if (distance <= 220) score += 40;
    if (/attach|upload|file|paperclip|computer|plus/.test(normalized)) score += 160;
    if (hasSvg) score += 20;
    if (!text) score += 5;
    if (sendLike) score -= 500;
    return {
      tag: el.tagName.toLowerCase(),
      role: clip(el.getAttribute?.("role") || "", 40),
      ariaLabel,
      title,
      testId,
      id,
      className,
      text,
      sameForm,
      distance,
      hasSvg,
      sendLike,
      score,
    };
  };

  const candidates = Array.from(document.querySelectorAll("button, [role='button'], label"))
    .filter((el) => el instanceof HTMLElement && isVisible(el))
    .map((el, index) => {
      const description = describe(el);
      const candidateId = `yoetz-upload-candidate-${index}`;
      el.setAttribute("data-yoetz-upload-candidate", candidateId);
      return { id: candidateId, ...description };
    })
    .filter((candidate) => candidate.sameForm || candidate.distance <= 260)
    .sort((a, b) => b.score - a.score)
    .slice(0, 12);

  const fileInputs = Array.from(document.querySelectorAll("input[type='file']")).map((input, index) => ({
    id: `file-input-${index}`,
    visible: isVisible(input),
    multiple: !!input.multiple,
    accept: clip(input.getAttribute("accept") || "", 80),
    hasFiles: (input.files?.length || 0) > 0,
    fileNames: Array.from(input.files || []).map((file) => file.name),
    sameForm: !!composerForm && input.closest("form") === composerForm,
    distance: distanceToComposer(input),
  }));

  return {
    composerFound: !!composer,
    composerTag: composer?.tagName?.toLowerCase() || null,
    composerText: clip(composer?.innerText || composer?.textContent || "", 80),
    fileInputs,
    candidates,
  };
}
"##
    .to_string()
}

fn describe_upload_candidate(candidate: &serde_json::Value) -> String {
    let id = candidate
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let aria_label = candidate
        .get("ariaLabel")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let title = candidate
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let test_id = candidate
        .get("testId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let text = candidate
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    format!("{id} aria={aria_label:?} title={title:?} testid={test_id:?} text={text:?}")
}

fn format_upload_diagnostics(diagnostics: &serde_json::Value) -> String {
    serde_json::to_string_pretty(diagnostics).unwrap_or_else(|_| diagnostics.to_string())
}

async fn wait_for_attachment_visible(
    client: &ChromeCdpClient,
    file_name: &str,
    upload_timeout_ms: u64,
) -> Result<()> {
    let script = build_wait_for_attachment_visible_script(file_name, upload_timeout_ms)?;
    let evidence = client
        .evaluate_script(&script, vec![])
        .await
        .context("evaluate attachment visibility")?;
    if evidence.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }

    Err(anyhow!(
        "attachment chip for `{file_name}` did not appear in the composer.\n{}",
        format_upload_diagnostics(&evidence)
    ))
}

fn build_wait_for_attachment_visible_script(
    file_name: &str,
    upload_timeout_ms: u64,
) -> Result<String> {
    let probe_function_json =
        serde_json::to_string(&chatgpt_web::build_attachment_probe_function(file_name)?)?;
    Ok(format!(
        r##"
async () => {{
  const probe = eval("(" + {probe_function_json} + ")");
  const stableEnough = (state) => state?.ok && Number(state?.stableReadyCount || 0) >= {upload_stable_polls};
  const deadline = Date.now() + {upload_timeout_ms};
  while (Date.now() < deadline) {{
    const state = probe();
    if (stableEnough(state) || state?.status === "failed") {{
      return state;
    }}
    await new Promise((resolve) => setTimeout(resolve, 250));
  }}
  const finalState = probe();
  if (finalState?.ok && !stableEnough(finalState)) {{
    return {{ ...finalState, ok: false, status: "uploading", stableReady: false }};
  }}
  return finalState;
}}
"##,
        probe_function_json = probe_function_json,
        upload_timeout_ms = upload_timeout_ms,
        upload_stable_polls = chatgpt_web::CHATGPT_UPLOAD_STABLE_POLLS,
    ))
}

/// Stable-idle polling for ChatGPT response completion.
///
/// Returns the final assistant message text once a post-send assistant turn
/// has been idle for the shared ChatGPT stable-idle threshold.
async fn poll_for_stable_response(
    client: &mut ChromeCdpClient,
    ctx: &DevtoolsMcpRecipeContext,
    page_id: &str,
    baseline: ResponseBaseline,
    overall_timeout_ms: u64,
    poll_interval_ms: u64,
    reconnect_policy: ReconnectPolicy,
) -> Result<String> {
    let read_state_js = response_poll_state_script();
    let start = std::time::Instant::now();
    let overall_timeout = Duration::from_millis(overall_timeout_ms);
    let stable_idle_threshold_ms = chatgpt_web::stable_idle_threshold_ms(poll_interval_ms);
    let mut idle_since: Option<std::time::Instant> = None;
    let mut idle_anchor: Option<(i64, i64)> = None;
    let mut reconnect_used = false;

    loop {
        if start.elapsed() > overall_timeout {
            return Err(anyhow!(
                "ChatGPT response did not complete within {overall_timeout_ms}ms"
            ));
        }

        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;

        let state = match client.evaluate_script(&read_state_js, vec![]).await {
            Ok(state) => state,
            Err(err) => {
                match classify_response_poll_eval_error(&err, reconnect_used, reconnect_policy) {
                    ResponsePollEvalFailureAction::ReconnectAndRetry => {
                        emit_stable_idle_warning(
                            "warn: stable-idle poll lost the Chrome websocket; reconnecting once to the existing ChatGPT tab",
                        );
                        *client = reconnect_response_poll_client(ctx, page_id)
                            .await
                            .context("recover Chrome websocket during ChatGPT response wait")?;
                        reconnect_used = true;
                        continue;
                    }
                    ResponsePollEvalFailureAction::Fail => {
                        return Err(err.context(
                            "chrome-devtools-mcp lost the Chrome websocket while waiting for the ChatGPT response",
                        ));
                    }
                    ResponsePollEvalFailureAction::RetrySameClient => {}
                }
                // Treat transient eval errors as non-fatal; keep polling.
                emit_stable_idle_warning(&format!(
                    "warn: stable-idle poll eval failed ({err:#}), retrying"
                ));
                continue;
            }
        };

        let Some(obj) = state.as_object() else {
            emit_stable_idle_warning(&format!(
                "warn: stable-idle poll returned non-object: {state}"
            ));
            continue;
        };

        let poll_state = parse_response_poll_state(obj).context("parse stable-idle poll state")?;
        if !poll_state.error.is_empty() {
            return Err(anyhow!("ChatGPT error: {}", poll_state.error));
        }
        match classify_response_completion(&poll_state, baseline) {
            ResponseCompletionVerdict::Generating => {
                idle_since = None;
                idle_anchor = None;
            }
            ResponseCompletionVerdict::CopyButton => {
                if !poll_state.text.is_empty() {
                    return Ok(poll_state.text);
                }
                let extracted = read_latest_assistant_text(client)
                    .await
                    .context("read latest assistant text after copy-button completion")?;
                if !extracted.is_empty() {
                    return Ok(extracted);
                }
                idle_since = Some(std::time::Instant::now());
                idle_anchor = Some((poll_state.assistant_count, poll_state.assistant_last_len));
            }
            ResponseCompletionVerdict::Idle => {
                let anchor = (poll_state.assistant_count, poll_state.assistant_last_len);
                let stable_for_ms = match (idle_since, idle_anchor) {
                    (Some(since), Some(previous_anchor)) if previous_anchor == anchor => {
                        std::time::Instant::now()
                            .duration_since(since)
                            .as_millis()
                            .min(u128::from(u64::MAX)) as u64
                    }
                    _ => {
                        idle_since = Some(std::time::Instant::now());
                        idle_anchor = Some(anchor);
                        0
                    }
                };
                if stable_for_ms >= stable_idle_threshold_ms {
                    if poll_state.text.is_empty() {
                        return Err(anyhow!(
                            "stable-idle reached but assistant message text is empty"
                        ));
                    }
                    return Ok(poll_state.text);
                }
            }
        }
    }
}

async fn reconnect_response_poll_client(
    ctx: &DevtoolsMcpRecipeContext,
    page_id: &str,
) -> Result<ChromeCdpClient> {
    let client = connect_client(ctx)
        .await
        .context("reconnect Chrome websocket for stable-idle polling")?;
    client
        .select_page_target(page_id, 30_000)
        .with_context(|| format!("reattach to ChatGPT page target `{page_id}` after reconnect"))?;
    Ok(client)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ResponsePollEvalFailureAction {
    RetrySameClient,
    ReconnectAndRetry,
    Fail,
}

fn classify_response_poll_eval_error(
    err: &anyhow::Error,
    reconnect_used: bool,
    reconnect_policy: ReconnectPolicy,
) -> ResponsePollEvalFailureAction {
    if !is_closed_cdp_transport_error(err) {
        return ResponsePollEvalFailureAction::RetrySameClient;
    }
    if reconnect_policy == ReconnectPolicy::Never {
        return ResponsePollEvalFailureAction::Fail;
    }
    if reconnect_used {
        ResponsePollEvalFailureAction::Fail
    } else {
        ResponsePollEvalFailureAction::ReconnectAndRetry
    }
}

fn response_poll_state_script() -> String {
    let send_button_selector_json = chatgpt_web::send_button_selector_json();
    let stop_button_selector_json = chatgpt_web::stop_button_selector_json();
    format!(
        r##"
  () => {{
{visibility_helpers}
{turn_root_helpers}
    const nodes = Array.from(document.querySelectorAll("[data-message-author-role='assistant']"));
  const send = findVisible(document, {send_button_selector_json});
  const last = nodes.length > 0 ? nodes[nodes.length - 1] : null;
  const turnRoot = latestAssistantTurn(last);
  const stopButton = (turnRoot ? findVisible(turnRoot, {stop_button_selector_json}) : null) ||
    (!turnRoot ? findVisible(document, {stop_button_selector_json}) : null);
  const stopGenerating = !!stopButton && !stopButton.disabled;
  const thinkingSelector = ".result-thinking, [data-testid*='thinking'], [class*='thinking']";
  const thinking = (turnRoot ? findVisible(turnRoot, thinkingSelector) : null) ||
    (!turnRoot ? findVisible(document, thinkingSelector) : null);
  if (nodes.length === 0) {{
    return {{
      ready: false,
      count: 0,
      length: 0,
      text: null,
      streaming: stopGenerating,
      stopGenerating,
      thinking: !!thinking,
      copyButtons: 0,
      sendState: !send ? "missing" : send.disabled ? "disabled" : "enabled",
      error: "",
    }};
  }}
  const copyRoot = turnRoot || last.parentElement || document;
  const copyButtons = copyRoot.querySelectorAll("button[aria-label*='Copy'], button[data-testid*='copy']").length;
  const errEl = document.querySelector('[class*="error-toast"], [data-testid*="error"], [role="alert"]');
  const errText = errEl ? (errEl.innerText || "").substring(0, 100).toLowerCase() : "";
  const markers = ["network error","something went wrong","error generating","attachment failed","upload failed","too many requests"];
  const error = markers.find((marker) => errText.includes(marker)) || "";
  const streaming =
    stopGenerating ||
    !!last.querySelector(".result-streaming") ||
    last.classList.contains("result-streaming");
  const text = last.innerText || "";
  return {{
    count: nodes.length,
    length: text.length,
    text,
    streaming,
    sendState: !send ? "missing" : send.disabled ? "disabled" : "enabled",
    hasStopButton: stopGenerating,
    thinking: !!thinking,
    copyButtons,
    error,
  }};
}}
  "##,
        send_button_selector_json = send_button_selector_json,
        stop_button_selector_json = stop_button_selector_json,
        visibility_helpers = chatgpt_web::JS_VISIBILITY_HELPERS,
        turn_root_helpers = chatgpt_web::JS_TURN_ROOT_HELPERS,
    )
}

fn parse_response_poll_state(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<ResponsePollState> {
    Ok(ResponsePollState {
        assistant_count: obj
            .get("count")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        assistant_last_len: obj
            .get("length")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        text: obj
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        streaming: obj
            .get("streaming")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        send_state: match obj
            .get("sendState")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("missing")
        {
            "enabled" => ResponseSendState::Enabled,
            "disabled" => ResponseSendState::Disabled,
            "missing" => ResponseSendState::Missing,
            other => return Err(anyhow!("invalid send state `{other}`")),
        },
        has_stop_button: obj
            .get("hasStopButton")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        has_thinking_indicator: obj
            .get("thinking")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        copy_button_count: obj
            .get("copyButtons")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize,
        error: obj
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn classify_response_completion(
    state: &ResponsePollState,
    baseline: ResponseBaseline,
) -> ResponseCompletionVerdict {
    let composer_idle = matches!(
        state.send_state,
        ResponseSendState::Enabled | ResponseSendState::Missing
    );
    if state.streaming || !composer_idle || state.has_stop_button || state.has_thinking_indicator {
        return ResponseCompletionVerdict::Generating;
    }
    let new_message = state.assistant_count > baseline.assistant_count;
    if new_message && state.copy_button_count > 0 {
        return ResponseCompletionVerdict::CopyButton;
    }
    // We intentionally treat only monotonic assistant growth as forward
    // progress. If ChatGPT ever performs an in-place rewrite with the same
    // message count and length, we keep waiting until the hard response timeout
    // rather than risk classifying a stale pre-send turn as complete.
    let same_message_grew = state.assistant_count == baseline.assistant_count
        && state.assistant_last_len > baseline.assistant_last_len;
    if (new_message && state.assistant_last_len > 0) || same_message_grew {
        ResponseCompletionVerdict::Idle
    } else {
        ResponseCompletionVerdict::Generating
    }
}

async fn read_latest_assistant_text(client: &ChromeCdpClient) -> Result<String> {
    let script = r##"
() => {
  const nodes = Array.from(document.querySelectorAll("[data-message-author-role='assistant']"));
  if (nodes.length === 0) return "";
  return nodes[nodes.length - 1].innerText || "";
}
"##;
    let value = client
        .evaluate_script(script, vec![])
        .await
        .context("evaluate latest assistant text")?;
    Ok(value
        .as_str()
        .map(str::trim)
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    #[tokio::test]
    async fn run_errors_without_bundle() {
        let ctx = DevtoolsMcpRecipeContext {
            bundle_path: None,
            bundle_text: None,
            ..DevtoolsMcpRecipeContext::default()
        };

        let err = run(&ctx).await.expect_err("should require bundle input");
        let msg = format!("{err:#}");
        assert!(msg.contains("bundle"), "error should mention bundle: {msg}");
    }

    // The constants-are-sane test used to live here but clippy's
    // `assertions_on_constants` lint correctly pointed out that asserting on
    // a const at runtime is pointless — if the value is wrong, the compile
    // is already broken. We instead rely on the `const { }` block below,
    // which fails the build if any of these invariants regress.
    const _CONST_SANITY: () = {
        assert!(chatgpt_web::STABLE_IDLE_FLOOR_MS >= 1_000);
        assert!(chatgpt_web::STABLE_IDLE_INTERVAL_MULTIPLIER >= 2);
        assert!(!chatgpt_web::CHATGPT_URL.is_empty());
    };

    #[test]
    fn eager_file_input_upload_keeps_verify_slack_after_zero_wait_probe() {
        let (input_wait_ms, verify_timeout_ms) = eager_file_input_upload_timeouts();
        assert_eq!(input_wait_ms, 0);
        assert!(
            verify_timeout_ms >= 5_000,
            "finding an eager file input must not upload and then verify with a 0ms deadline"
        );
    }

    fn lock_env() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        #[allow(unsafe_code)]
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            match self.previous.as_deref() {
                Some(value) => unsafe {
                    std::env::set_var(self.key, value);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }

    #[test]
    fn cdp_attach_hint_preserves_approval_dialog_errors() {
        // Approval-wait errors must pass through so the outer transport funnel
        // can classify them as "user needs to click Allow" (stop-fallback)
        // rather than wrap them in generic Chrome-136+ guidance.
        let err = anyhow!(
            "live browser attach timed out (30s). Chrome may be showing an \"Allow remote debugging?\" dialog — click Allow, then retry."
        );
        let rewritten = cdp_attach_hint(err);
        let msg = format!("{rewritten:#}");
        assert!(msg.contains("Allow remote debugging"));
        assert!(!msg.contains("chrome://inspect"));
    }

    #[test]
    #[serial]
    fn approval_wait_message_uses_configured_handshake_timeout() {
        let _guard = lock_env();
        let _timeout = EnvVarGuard::set("YOETZ_CDP_WS_HANDSHAKE_TIMEOUT_MS", "120000");
        assert!(approval_wait_message().contains("120s"));
    }

    #[tokio::test]
    async fn with_attach_attempt_lock_acquires_before_running_action() {
        #[derive(Clone, Copy)]
        struct FakeLock;

        impl AttachAttemptLockState for FakeLock {
            fn waited(&self) -> bool {
                false
            }
        }

        let acquired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acquired_for_action = acquired.clone();

        with_attach_attempt_lock_using(
            false,
            || {
                acquired.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<FakeLock, anyhow::Error>(FakeLock)
            },
            || async move {
                assert!(
                    acquired_for_action.load(std::sync::atomic::Ordering::SeqCst),
                    "attach attempt lock should be acquired before the action runs"
                );
                Ok::<(), anyhow::Error>(())
            },
        )
        .await
        .unwrap();
    }

    #[test]
    fn retire_failed_reused_control_tab_only_for_persistent_control_tabs() {
        let err = Err(anyhow!("composer did not mount"));
        assert!(should_retire_failed_reused_control_tab(
            Some("control-run"),
            &err
        ));
        assert!(!should_retire_failed_reused_control_tab(None, &err));
        assert!(!should_retire_failed_reused_control_tab(
            Some("control-run"),
            &Ok(())
        ));
    }

    #[test]
    fn cdp_attach_hint_wraps_other_errors_with_actionable_guidance() {
        let err =
            anyhow!("requesting `http://127.0.0.1:9222/json/version` failed: connection refused");
        let rewritten = cdp_attach_hint(err);
        let msg = format!("{rewritten:#}");
        assert!(msg.contains("chrome://inspect"));
        assert!(msg.contains("Chrome 136+"));
        // Original error chain is preserved.
        assert!(msg.contains("connection refused"));
    }

    #[test]
    fn cdp_attach_hint_rewrites_websocket_handshake_timeouts_as_approval_waits() {
        let err = anyhow!(
            "connecting to Chrome websocket `ws://127.0.0.1:9222/devtools/browser/test` failed: timed out waiting for Chrome websocket handshake"
        );
        let rewritten = cdp_attach_hint(err);
        let msg = format!("{rewritten:#}");
        assert!(msg.contains("Allow remote debugging"));
        assert!(!msg.contains("chrome://inspect"));
    }

    #[test]
    fn cdp_attach_hint_rewrites_forbidden_websocket_handshakes_with_block_guidance() {
        let err = anyhow!(
            "connecting to Chrome websocket `ws://127.0.0.1:9222/devtools/browser/test` failed: HTTP error: 403 Forbidden"
        );
        let rewritten = cdp_attach_hint(err);
        let msg = format!("{rewritten:#}");
        assert!(msg.contains("403 Forbidden"));
        assert!(msg.contains("Chrome rejected the remote debugging client"));
        assert!(msg.contains("click Allow"));
        assert!(!msg.contains("chrome://inspect"));
    }

    #[test]
    fn classify_live_chatgpt_page_issue_detects_login_and_challenge() {
        assert_eq!(
            classify_live_chatgpt_page_issue(
                "https://chatgpt.com/auth/login",
                "Log in - OpenAI",
                "Continue with Google"
            ),
            Some(
                "chatgpt login required in the attached Chrome session. Log in there and try again"
            )
        );
        assert_eq!(
            classify_live_chatgpt_page_issue(
                "https://chatgpt.com/",
                "Just a moment...",
                "Verify you are human"
            ),
            Some(
                "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again"
            )
        );
        assert_eq!(
            classify_live_chatgpt_page_issue("https://chatgpt.com/", "ChatGPT", "Send a message"),
            None
        );
    }

    #[test]
    fn page_probe_summary_redacts_body_text() {
        let summary = format_page_probe_summary(&serde_json::json!({
            "url": "https://chatgpt.com/",
            "title": "ChatGPT",
            "bodyText": "secret review draft"
        }));
        assert!(summary.contains("url=https://chatgpt.com/"));
        assert!(summary.contains("title=\"ChatGPT\""));
        assert!(!summary.contains("secret review draft"));
        assert!(!summary.contains("body="));
    }

    #[test]
    fn wait_for_composer_retry_classifier_matches_transient_eval_failures() {
        assert!(should_retry_wait_for_composer_error(&anyhow!(
            "Runtime.evaluate failed: Execution context was destroyed."
        )));
        assert!(should_retry_wait_for_composer_error(&anyhow!(
            "Protocol error: Cannot find context with specified id"
        )));
        assert!(should_retry_wait_for_composer_error(&anyhow!(
            "Target closed"
        )));
        assert!(!should_retry_wait_for_composer_error(&anyhow!(
            "chatgpt login required in the attached Chrome session"
        )));
    }

    #[test]
    fn wait_for_composer_script_embeds_focus_flag_without_snapshot_args() {
        let focused = build_wait_for_composer_script(true);
        assert!(focused.contains("const focusComposer = true;"));
        assert!(!focused.contains("async (focusComposer = true)"));

        let unfocused = build_wait_for_composer_script(false);
        assert!(unfocused.contains("const focusComposer = false;"));
    }

    #[test]
    fn closed_cdp_transport_errors_are_classified() {
        let err = anyhow!("Unable to make method calls because underlying connection is closed");
        assert!(is_closed_cdp_transport_error(&err));
        let other = anyhow!("timed out waiting for ChatGPT response");
        assert!(!is_closed_cdp_transport_error(&other));
    }

    #[test]
    fn initial_new_page_closed_transport_errors_trigger_retry() {
        let err = anyhow!(
            "chrome-devtools-mcp new_page on `https://chatgpt.com/?_yoetz=test`: creating a new Chrome page for `https://chatgpt.com/?_yoetz=test` failed: Unable to make method calls because underlying connection is closed"
        );
        assert!(should_retry_initial_new_page_after_reconnect(&err));
    }

    #[test]
    fn retry_classifier_ignores_non_new_page_closed_transport_errors() {
        let err = anyhow!(
            "mark yoetz-owned ChatGPT tab with window.name: Unable to make method calls because underlying connection is closed"
        );
        assert!(!should_retry_initial_new_page_after_reconnect(&err));
    }

    #[test]
    fn retry_classifier_skips_external_create_target_block_errors() {
        let err = anyhow!(
            "chrome-devtools-mcp new_page on `https://chatgpt.com/?_yoetz=test`: creating a new Chrome page for `https://chatgpt.com/?_yoetz=test` failed: Chrome's default-profile CDP endpoint likely rejected external `Target.createTarget` while opening `https://chatgpt.com/?_yoetz=test`. Chrome 146+/147 can allow attach/read operations but close the session on new-tab creation for untrusted clients. First, open chrome://inspect/#remote-debugging, refresh Discover network targets (or Open dedicated DevTools for Node), and retry. If Chrome still closes the session, launch Chrome with `--remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug` and pass `--cdp`, or use Chrome for Testing. Unable to make method calls because underlying connection is closed"
        );
        assert!(!should_retry_initial_new_page_after_reconnect(&err));
    }

    #[test]
    fn external_create_target_block_still_triggers_recovery_after_reconnect() {
        let err = anyhow!(
            "chrome-devtools-mcp new_page on `https://chatgpt.com/?_yoetz=test`: creating a new Chrome page for `https://chatgpt.com/?_yoetz=test` failed: Chrome's default-profile CDP endpoint likely rejected external `Target.createTarget` while opening `https://chatgpt.com/?_yoetz=test`. Chrome 146+/147 can allow attach/read operations but close the session on new-tab creation for untrusted clients. First, open chrome://inspect/#remote-debugging, refresh Discover network targets (or Open dedicated DevTools for Node), and retry. If Chrome still closes the session, launch Chrome with `--remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug` and pass `--cdp`, or use Chrome for Testing. Unable to make method calls because underlying connection is closed"
        );
        assert!(should_recover_initial_page_open_after_reconnect(&err));
    }

    #[test]
    fn initial_page_retry_uses_existing_anchor_recovery() {
        assert_eq!(
            retry_initial_page_open_mode(),
            InitialPageOpenMode::RecoverViaExistingAnchor
        );
        assert_eq!(
            retry_initial_page_open_mode().debug_strategy(),
            "recover_via_existing_anchor"
        );
    }

    #[test]
    fn response_poll_closed_transport_reconnects_only_once() {
        let closed = anyhow!("Unable to make method calls because underlying connection is closed");
        let other = anyhow!("Execution context was destroyed");

        assert_eq!(
            classify_response_poll_eval_error(&closed, false, ReconnectPolicy::OneStandaloneRetry),
            ResponsePollEvalFailureAction::ReconnectAndRetry
        );
        assert_eq!(
            classify_response_poll_eval_error(&closed, true, ReconnectPolicy::OneStandaloneRetry),
            ResponsePollEvalFailureAction::Fail
        );
        assert_eq!(
            classify_response_poll_eval_error(&other, false, ReconnectPolicy::OneStandaloneRetry),
            ResponsePollEvalFailureAction::RetrySameClient
        );
    }

    #[test]
    fn daemon_response_poll_closed_transport_fails_without_reconnect() {
        let closed = anyhow!("Unable to make method calls because underlying connection is closed");

        assert_eq!(
            classify_response_poll_eval_error(&closed, false, ReconnectPolicy::Never),
            ResponsePollEvalFailureAction::Fail
        );
    }

    #[test]
    fn model_selection_script_supports_auto_and_explicit_modes() {
        let auto_script = build_model_selection_script("auto");
        assert!(auto_script.contains(r#"const requested = "auto";"#));
        assert!(!auto_script.contains(r#""kept-current-no-selector""#));
        assert!(auto_script.contains("const deriveRequestedTier = (value) =>"));
        assert!(auto_script.contains("const classifyTier = (item) =>"));
        assert!(auto_script.contains("const buildTierRankings = (entries) =>"));

        let explicit_script = build_model_selection_script("gpt-5-4-pro");
        assert!(explicit_script.contains(r#"const requested = "gpt-5-4-pro";"#));
        assert!(explicit_script.contains("\"gpt-5-pro\":\"gpt-5-4-pro\""));
        assert!(!explicit_script.contains("\"gpt-5-3-pro\""));
        assert!(explicit_script.contains("const selectBestTierItem = (entries, slug, rankings) =>"));
        assert!(explicit_script.contains("availableItems"));
    }

    #[test]
    fn classify_response_completion_rejects_stale_prior_response() {
        let baseline = ResponseBaseline {
            assistant_count: 2,
            assistant_last_len: 120,
        };
        let stale = ResponsePollState {
            assistant_count: 2,
            assistant_last_len: 120,
            text: "old answer".to_string(),
            streaming: false,
            send_state: ResponseSendState::Enabled,
            has_stop_button: false,
            has_thinking_indicator: false,
            copy_button_count: 0,
            error: String::new(),
        };
        assert_eq!(
            classify_response_completion(&stale, baseline),
            ResponseCompletionVerdict::Generating
        );
    }

    #[test]
    fn classify_response_completion_accepts_new_or_growing_post_send_response() {
        let baseline = ResponseBaseline {
            assistant_count: 2,
            assistant_last_len: 120,
        };
        let new_message = ResponsePollState {
            assistant_count: 3,
            assistant_last_len: 18,
            text: "new answer".to_string(),
            streaming: false,
            send_state: ResponseSendState::Enabled,
            has_stop_button: false,
            has_thinking_indicator: false,
            copy_button_count: 0,
            error: String::new(),
        };
        assert_eq!(
            classify_response_completion(&new_message, baseline),
            ResponseCompletionVerdict::Idle
        );

        let same_message_grew = ResponsePollState {
            assistant_count: 2,
            assistant_last_len: 140,
            text: "expanded answer".to_string(),
            streaming: false,
            send_state: ResponseSendState::Enabled,
            has_stop_button: false,
            has_thinking_indicator: false,
            copy_button_count: 0,
            error: String::new(),
        };
        assert_eq!(
            classify_response_completion(&same_message_grew, baseline),
            ResponseCompletionVerdict::Idle
        );
    }

    #[test]
    fn classify_response_completion_accepts_copy_button_on_new_message() {
        let baseline = ResponseBaseline {
            assistant_count: 2,
            assistant_last_len: 120,
        };
        let completed = ResponsePollState {
            assistant_count: 3,
            assistant_last_len: 0,
            text: "done".to_string(),
            streaming: false,
            send_state: ResponseSendState::Missing,
            has_stop_button: false,
            has_thinking_indicator: false,
            copy_button_count: 1,
            error: String::new(),
        };
        assert_eq!(
            classify_response_completion(&completed, baseline),
            ResponseCompletionVerdict::CopyButton
        );
    }

    #[test]
    fn attachment_visibility_script_matches_file_name_variable() {
        let script = build_wait_for_attachment_visible_script("bundle.txt", 180_000).unwrap();
        assert!(script.contains("name === fileName"));
        assert!(script.contains("exactNameMatched"));
        assert!(script.contains("stableReadyCount"));
        assert!(script.contains("Date.now() + 180000"));
    }

    #[test]
    fn upload_diagnostics_script_cleans_previous_candidate_ids() {
        let script = build_collect_upload_diagnostics_script();
        assert!(script.contains("querySelectorAll(\"[data-yoetz-upload-candidate]\")"));
        assert!(script.contains("removeAttribute(\"data-yoetz-upload-candidate\")"));
    }

    #[test]
    fn response_poll_script_looks_for_copy_buttons_on_turn_root() {
        let script = response_poll_state_script();
        assert!(script.contains("latestAssistantTurn"));
        assert!(script.contains("const copyRoot = turnRoot || last.parentElement || document;"));
        assert!(script.contains("copyRoot.querySelectorAll"));
        assert!(script.contains("findVisible(turnRoot,"));
    }
}

//! Claude Fable 5 / Max / Thinking recipe over the live Chrome CDP client.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

use super::client::{is_external_create_target_block_error, ChromeCdpClient};
use super::DevtoolsMcpRecipeContext;
use crate::claude_recipe::{self, AnyhowResultExt};
use crate::claude_web;
use crate::web_recipe::{WebModelSelectionStatus, WebRecipeTransportPhase};

const NO_PROGRESS_POLL_LIMIT: u8 = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeRunResult {
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: WebModelSelectionStatus,
    pub conversation_id: Option<String>,
    pub conversation_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResponseBaseline {
    count: i64,
    last_length: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResponseState {
    count: i64,
    last_length: i64,
    text: String,
    streaming: bool,
    stop: bool,
    thinking: bool,
    copy_buttons: usize,
    error: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletionVerdict {
    Generating,
    CopyButton,
    Idle,
}

pub async fn run(ctx: &DevtoolsMcpRecipeContext) -> Result<ClaudeRunResult> {
    if claude_recipe::canonical_fable_max_model(&ctx.model).is_none() {
        bail!(
            "Claude CDP recipe requires model `{}` (alias `{}`); got `{}`",
            claude_recipe::CLAUDE_FABLE_MAX_MODEL,
            claude_recipe::CLAUDE_FABLE_MAX_ALIAS,
            ctx.model
        );
    }
    let bundle_path = ctx.bundle_path.as_deref().ok_or_else(|| {
        anyhow!("Claude recipe requires `--bundle`; CDP uploads a file attachment")
    })?;
    let mut client = crate::chrome_devtools_mcp::chatgpt::connect_client_with_attach_attempt_lock(
        ctx.cdp_endpoint.as_deref(),
        ctx.show_approval_guidance,
    )
    .await?;
    let browser_context_id = client
        .resolve_browser_context_id(
            ctx.browser_context_id.as_deref(),
            ctx.profile_email.as_deref(),
        )
        .context("resolve Chrome browser context for Claude")?;
    run_with_client(&mut client, ctx, bundle_path, browser_context_id.as_deref()).await
}

pub async fn check_auth(cdp_endpoint: Option<&str>, show_approval_guidance: bool) -> Result<()> {
    let client = crate::chrome_devtools_mcp::chatgpt::connect_client_with_attach_attempt_lock(
        cdp_endpoint,
        show_approval_guidance,
    )
    .await?;
    let run_id = claude_web::generate_run_id();
    let marked_url = claude_web::mark_claude_url(&run_id)?;
    open_claude_page(&client, &marked_url, None)
        .await
        .context("open Claude auth-check page")?;
    let result = wait_for_composer(&client).await;
    let _ = client.close_selected_page(false);
    result
}

async fn run_with_client(
    client: &mut ChromeCdpClient,
    ctx: &DevtoolsMcpRecipeContext,
    bundle_path: &Path,
    browser_context_id: Option<&str>,
) -> Result<ClaudeRunResult> {
    let marked_url = claude_web::mark_claude_url(&ctx.run_id)?;
    open_claude_page(client, &marked_url, browser_context_id)
        .await
        .with_context(|| format!("open yoetz-owned Claude page `{marked_url}`"))?;
    client
        .evaluate_script(&claude_web::build_set_window_name_js(&ctx.run_id)?, vec![])
        .await
        .context("mark yoetz-owned Claude tab with window.name")?;

    wait_for_composer(client).await?;
    let model_selection = select_fable_max_thinking(client).await?;
    if model_selection != WebModelSelectionStatus::Selected {
        bail!("Claude Fable 5 / Max / Thinking selection was not verified")
    }

    upload_bundle(client, bundle_path, ctx.upload_timeout_ms)
        .await
        .with_claude_phase(WebRecipeTransportPhase::Upload)
        .context("upload bundle to Claude")?;

    client
        .evaluate_script(&claude_web::build_focus_composer_function(), vec![])
        .await
        .with_claude_phase(WebRecipeTransportPhase::Send)
        .context("focus Claude composer after upload")?;
    client
        .type_text(&ctx.prompt, None)
        .await
        .with_claude_phase(WebRecipeTransportPhase::Send)
        .context("type prompt into Claude composer")?;
    let send = client
        .evaluate_script(&claude_web::build_send_function(), vec![])
        .await
        .with_claude_phase(WebRecipeTransportPhase::Send)
        .context("click enabled Claude send button")?;
    if send.get("status").and_then(Value::as_str) != Some("sent") {
        bail!("could not find an enabled Claude send button; diagnostics={send}")
    }
    let baseline = ResponseBaseline {
        count: send
            .get("assistantCount")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        last_length: send
            .get("assistantLastLength")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    };

    let response = poll_for_response(
        client,
        baseline,
        ctx.response_timeout_ms,
        ctx.response_poll_interval_ms,
    )
    .await
    .with_claude_phase(WebRecipeTransportPhase::WaitResponse)
    .context("stable-idle polling for Claude response")?;
    let conversation = read_conversation(client).await?;

    Ok(ClaudeRunResult {
        response,
        model_used: Some(claude_recipe::CLAUDE_REPORTED_MODEL.to_string()),
        model_selection_status: model_selection,
        conversation_id: conversation.as_ref().map(|value| value.id.clone()),
        conversation_url: conversation.map(|value| value.url),
    })
}

async fn open_claude_page(
    client: &ChromeCdpClient,
    marked_url: &str,
    browser_context_id: Option<&str>,
) -> Result<()> {
    match client
        .new_page("about:blank", false, 30_000, browser_context_id)
        .await
    {
        Ok(_) => client
            .navigate_page(marked_url, 30_000)
            .await
            .map(|_| ())
            .with_context(|| format!("navigate blank Chrome target to `{marked_url}`")),
        Err(err) if is_external_create_target_block_error(&err) => client
            .open_recipe_page_via_existing_site_anchor(
                marked_url,
                "claude.ai",
                false,
                30_000,
                browser_context_id,
            )
            .await
            .map(|_| ())
            .context("recover Claude page open through an existing safe Chrome anchor"),
        Err(err) => Err(err),
    }
}

async fn wait_for_composer(client: &ChromeCdpClient) -> Result<()> {
    let state = client
        .evaluate_script(&claude_web::build_wait_for_composer_function(), vec![])
        .await
        .context("wait for Claude composer")?;
    match state.get("status").and_then(Value::as_str) {
        Some("ready") => Ok(()),
        Some("login") => bail!("Claude login is required in the attached Chrome profile"),
        Some("challenge") => bail!(
            "Cloudflare challenge detected on claude.ai; solve it in the attached Chrome window and retry"
        ),
        other => bail!("Claude composer did not become ready; status={other:?}, diagnostics={state}"),
    }
}

async fn select_fable_max_thinking(client: &ChromeCdpClient) -> Result<WebModelSelectionStatus> {
    open_model_menu(client).await?;
    let fable = client
        .evaluate_script(&claude_web::build_select_fable_function(), vec![])
        .await
        .context("select Fable 5 from Claude model menu")?;
    require_status(&fable, "selected", "Fable 5", "options")?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    open_effort_submenu(client).await?;
    let max = client
        .evaluate_script(&claude_web::build_select_max_function(), vec![])
        .await
        .context("select Max from Claude effort submenu")?;
    require_status(&max, "selected", "Max effort", "effortOptions")?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    open_effort_submenu(client).await?;
    let thinking = client
        .evaluate_script(&claude_web::build_ensure_thinking_on_function(), vec![])
        .await
        .context("enable Claude Thinking")?;
    match thinking.get("status").and_then(Value::as_str) {
        Some("already_on" | "clicked") => {}
        _ => bail!("Claude Thinking switch was unavailable; diagnostics={thinking}"),
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // A successful click is not proof. Close, re-open, hover again, and read
    // the selected model, selected effort, and Thinking switch postconditions.
    client
        .evaluate_script(&claude_web::build_close_model_menu_function(), vec![])
        .await
        .context("close Claude model menu before verification")?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    open_effort_submenu(client).await?;
    let verification = client
        .evaluate_script(
            &claude_web::build_verify_fable_max_thinking_function(),
            vec![],
        )
        .await
        .context("verify Claude Fable 5 / Max / Thinking postconditions")?;
    let status = claude_web::model_selection_status(&verification);
    if status != WebModelSelectionStatus::Selected {
        bail!(
            "Claude exact model contract is unavailable or mismatched; required Fable 5 + Max + Thinking on; diagnostics={verification}"
        );
    }
    client
        .evaluate_script(&claude_web::build_close_model_menu_function(), vec![])
        .await
        .context("close Claude model menu after verification")?;
    Ok(status)
}

async fn open_model_menu(client: &ChromeCdpClient) -> Result<()> {
    let result = client
        .evaluate_script(&claude_web::build_open_model_menu_function(), vec![])
        .await
        .context("open Claude model menu")?;
    if matches!(
        result.get("status").and_then(Value::as_str),
        Some("opened" | "opening")
    ) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(())
    } else {
        bail!("Claude model selector was unavailable; diagnostics={result}")
    }
}

async fn open_effort_submenu(client: &ChromeCdpClient) -> Result<()> {
    open_model_menu(client).await?;
    let marked = client
        .evaluate_script(&claude_web::build_mark_effort_parent_function(), vec![])
        .await
        .context("mark Claude Effort menu trigger")?;
    require_status(&marked, "marked", "Effort menu", "options")?;
    let snapshot = client
        .take_snapshot(false)
        .await
        .context("snapshot marked Claude Effort menu trigger")?;
    let effort_text = marked
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("Effort");
    let uid = snapshot
        .find_uid_by_role_and_text("menuitem", effort_text)
        .or_else(|| snapshot.find_uid_by_role_and_text("menuitem", "Effort"))
        .or_else(|| snapshot.find_uid_by_text(claude_web::EFFORT_HOVER_MARKER))
        .ok_or_else(|| anyhow!("marked Claude Effort menu trigger was missing from snapshot"))?;
    client
        .hover(&uid)
        .await
        .context("dispatch real CDP mouse movement over Claude Effort menu trigger")?;
    tokio::time::sleep(Duration::from_millis(400)).await;
    Ok(())
}

fn require_status(
    value: &Value,
    expected: &str,
    operation: &str,
    diagnostics_key: &str,
) -> Result<()> {
    if value.get("status").and_then(Value::as_str) == Some(expected) {
        return Ok(());
    }
    bail!(
        "Claude {operation} is unavailable; {}={}",
        diagnostics_key,
        value.get(diagnostics_key).cloned().unwrap_or(Value::Null)
    )
}

async fn upload_bundle(
    client: &ChromeCdpClient,
    bundle_path: &Path,
    timeout_ms: u64,
) -> Result<()> {
    let file_name = bundle_path
        .file_name()
        .and_then(|value| value.to_str())
        .context("Claude bundle path must end in a UTF-8 filename")?;
    let scope = client
        .evaluate_script(&claude_web::build_scope_file_input_function(), vec![])
        .await
        .context("scope Claude composer file input")?;
    require_status(&scope, "marked", "file input", "selector")?;
    let snapshot = client
        .take_snapshot(false)
        .await
        .context("snapshot Claude file input")?;
    let uid = snapshot
        .find_marked_file_input_uid(claude_web::FILE_INPUT_MARKER)
        .ok_or_else(|| anyhow!("marked Claude file input was missing from snapshot"))?;
    client
        .upload_file(&uid, bundle_path)
        .await
        .context("upload file through Claude composer input")?;

    let probe = claude_web::build_attachment_probe_function(file_name)?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut stable_ready_ticks = 0_u8;
    loop {
        let state = client
            .evaluate_script(&probe, vec![])
            .await
            .context("probe Claude attachment readiness")?;
        if update_attachment_stability(&mut stable_ready_ticks, &state) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("Claude attachment `{file_name}` did not become ready within {timeout_ms}ms; diagnostics={state}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn update_attachment_stability(stable_ready_ticks: &mut u8, state: &Value) -> bool {
    if state.get("status").and_then(Value::as_str) == Some("candidate") {
        *stable_ready_ticks = stable_ready_ticks.saturating_add(1);
    } else {
        *stable_ready_ticks = 0;
    }
    *stable_ready_ticks >= 2
}

async fn poll_for_response(
    client: &ChromeCdpClient,
    baseline: ResponseBaseline,
    timeout_ms: u64,
    poll_interval_ms: u64,
) -> Result<String> {
    let script = claude_web::build_response_poll_function();
    let started = Instant::now();
    let stable_threshold_ms = claude_web::stable_idle_threshold_ms(poll_interval_ms);
    let mut stable_since: Option<Instant> = None;
    let mut stable_anchor: Option<(i64, i64)> = None;
    let mut no_progress_polls = 0_u8;

    loop {
        if started.elapsed() > Duration::from_millis(timeout_ms) {
            bail!("Claude response did not complete within {timeout_ms}ms");
        }
        tokio::time::sleep(Duration::from_millis(poll_interval_ms)).await;
        let value = client
            .evaluate_script(&script, vec![])
            .await
            .context("poll Claude response state")?;
        let state = parse_response_state(&value)?;
        if !state.error.is_empty() {
            bail!("Claude page error: {}", state.error);
        }
        if update_no_progress_stall(&mut no_progress_polls, &state, baseline) {
            bail!(
                "Claude response made no post-send progress for {NO_PROGRESS_POLL_LIMIT} polls after generation became idle; baseline_count={}, baseline_length={}, observed_count={}, observed_length={}, poll_interval_ms={poll_interval_ms}",
                baseline.count,
                baseline.last_length,
                state.count,
                state.last_length,
            );
        }
        match classify_completion(&state, baseline) {
            CompletionVerdict::Generating => {
                stable_since = None;
                stable_anchor = None;
            }
            CompletionVerdict::CopyButton | CompletionVerdict::Idle => {
                let anchor = (state.count, state.last_length);
                let stable_for = match (stable_since, stable_anchor) {
                    (Some(since), Some(previous)) if previous == anchor => since.elapsed(),
                    _ => {
                        stable_since = Some(Instant::now());
                        stable_anchor = Some(anchor);
                        Duration::ZERO
                    }
                };
                if stable_for >= Duration::from_millis(stable_threshold_ms) {
                    if state.text.is_empty() {
                        bail!("Claude stable-idle reached but response text was empty");
                    }
                    return Ok(state.text);
                }
            }
        }
    }
}

fn update_no_progress_stall(
    stalled_polls: &mut u8,
    state: &ResponseState,
    baseline: ResponseBaseline,
) -> bool {
    let inactive_without_progress = !state.streaming
        && !state.stop
        && !state.thinking
        && classify_completion(state, baseline) == CompletionVerdict::Generating;
    if inactive_without_progress {
        *stalled_polls = stalled_polls.saturating_add(1);
    } else {
        *stalled_polls = 0;
    }
    *stalled_polls >= NO_PROGRESS_POLL_LIMIT
}

fn parse_response_state(value: &Value) -> Result<ResponseState> {
    let send_state = value
        .get("sendState")
        .and_then(Value::as_str)
        .unwrap_or("missing");
    if !matches!(send_state, "enabled" | "disabled" | "missing") {
        bail!("invalid Claude send state `{send_state}`")
    }
    Ok(ResponseState {
        count: value.get("count").and_then(Value::as_i64).unwrap_or(0),
        last_length: value.get("length").and_then(Value::as_i64).unwrap_or(0),
        text: value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        streaming: value
            .get("streaming")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        stop: value
            .get("hasStopButton")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        thinking: value
            .get("thinking")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        copy_buttons: value
            .get("copyButtons")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        error: value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn classify_completion(state: &ResponseState, baseline: ResponseBaseline) -> CompletionVerdict {
    if state.streaming || state.stop || state.thinking {
        return CompletionVerdict::Generating;
    }
    let new_turn = state.count > baseline.count;
    let same_turn_grew = state.count == baseline.count && state.last_length > baseline.last_length;
    if !(new_turn || same_turn_grew) || state.last_length == 0 {
        return CompletionVerdict::Generating;
    }
    if state.copy_buttons > 0 {
        CompletionVerdict::CopyButton
    } else {
        CompletionVerdict::Idle
    }
}

async fn read_conversation(
    client: &ChromeCdpClient,
) -> Result<Option<crate::web_recipe::WebConversation>> {
    let value = client
        .evaluate_script("() => window.location.href || ''", vec![])
        .await
        .context("read Claude conversation URL")?;
    let Some(url) = value.as_str() else {
        return Ok(None);
    };
    match claude_web::normalize_conversation(url) {
        Ok(conversation) => Ok(Some(conversation)),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn baseline() -> ResponseBaseline {
        ResponseBaseline {
            count: 1,
            last_length: 100,
        }
    }

    #[test]
    fn completion_fails_closed_while_streaming_stopping_or_thinking() {
        let mut state = ResponseState {
            count: 2,
            last_length: 200,
            text: "answer".to_string(),
            streaming: false,
            stop: false,
            thinking: false,
            copy_buttons: 0,
            error: String::new(),
        };
        for field in ["streaming", "stop", "thinking"] {
            match field {
                "streaming" => state.streaming = true,
                "stop" => state.stop = true,
                "thinking" => state.thinking = true,
                _ => unreachable!(),
            }
            assert_eq!(
                classify_completion(&state, baseline()),
                CompletionVerdict::Generating
            );
            state.streaming = false;
            state.stop = false;
            state.thinking = false;
        }
    }

    #[test]
    fn disabled_send_with_post_send_growth_is_idle() {
        let state = parse_response_state(&json!({
            "count":2,"length":200,"text":"answer","streaming":false,
            "sendState":"disabled","hasStopButton":false,"thinking":false,"copyButtons":0,"error":""
        }))
        .unwrap();

        assert_eq!(
            classify_completion(&state, baseline()),
            CompletionVerdict::Idle
        );
    }

    #[test]
    fn completion_requires_monotonic_post_send_growth() {
        let stale = parse_response_state(&json!({
            "count":1,"length":100,"text":"old","streaming":false,
            "sendState":"missing","hasStopButton":false,"thinking":false,"copyButtons":0,"error":""
        }))
        .unwrap();
        assert_eq!(
            classify_completion(&stale, baseline()),
            CompletionVerdict::Generating
        );
        let grown = ResponseState {
            last_length: 101,
            text: "grown".into(),
            ..stale
        };
        assert_eq!(
            classify_completion(&grown, baseline()),
            CompletionVerdict::Idle
        );
    }

    #[test]
    fn idle_no_progress_fast_fails_after_bounded_polls_and_resets_on_activity() {
        let stale = parse_response_state(&json!({
            "count":1,"length":100,"text":"old","streaming":false,
            "sendState":"missing","hasStopButton":false,"thinking":false,"copyButtons":0,"error":""
        }))
        .unwrap();
        let mut stalled_polls = 0;
        for _ in 1..NO_PROGRESS_POLL_LIMIT {
            assert!(!update_no_progress_stall(
                &mut stalled_polls,
                &stale,
                baseline()
            ));
        }
        assert!(update_no_progress_stall(
            &mut stalled_polls,
            &stale,
            baseline()
        ));

        let active = ResponseState {
            streaming: true,
            ..stale
        };
        assert!(!update_no_progress_stall(
            &mut stalled_polls,
            &active,
            baseline()
        ));
        assert_eq!(stalled_polls, 0);
    }

    #[test]
    fn empty_new_turn_is_no_progress_but_completed_growth_is_not() {
        let empty = parse_response_state(&json!({
            "count":2,"length":0,"text":"","streaming":false,
            "sendState":"missing","hasStopButton":false,"thinking":false,"copyButtons":0,"error":""
        }))
        .unwrap();
        let mut stalled_polls = NO_PROGRESS_POLL_LIMIT - 1;
        assert!(update_no_progress_stall(
            &mut stalled_polls,
            &empty,
            baseline()
        ));

        let complete = ResponseState {
            last_length: 101,
            text: "grown".into(),
            ..empty
        };
        assert!(!update_no_progress_stall(
            &mut stalled_polls,
            &complete,
            baseline()
        ));
        assert_eq!(stalled_polls, 0);
    }

    #[test]
    fn copy_button_is_only_a_candidate_after_idle_and_growth() {
        let state = ResponseState {
            count: 2,
            last_length: 200,
            text: "answer".into(),
            streaming: false,
            stop: false,
            thinking: false,
            copy_buttons: 1,
            error: String::new(),
        };
        assert_eq!(
            classify_completion(&state, baseline()),
            CompletionVerdict::CopyButton
        );
    }

    #[test]
    fn model_diagnostics_are_actionable() {
        let value = json!({"status":"unavailable","options":["Opus 4.8","Sonnet 5"]});
        let err = require_status(&value, "selected", "Fable 5", "options").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("Fable 5"));
        assert!(message.contains("Opus 4.8"));
    }

    #[test]
    fn attachment_requires_two_consecutive_named_enabled_candidates() {
        let candidate = json!({"status":"candidate"});
        let pending = json!({"status":"pending"});
        let mut ticks = 0;
        assert!(!update_attachment_stability(&mut ticks, &candidate));
        assert_eq!(ticks, 1);
        assert!(!update_attachment_stability(&mut ticks, &pending));
        assert_eq!(ticks, 0);
        assert!(!update_attachment_stability(&mut ticks, &candidate));
        assert!(update_attachment_stability(&mut ticks, &candidate));
    }

    #[tokio::test]
    async fn run_rejects_non_fable_model_before_browser_attach() {
        let ctx = DevtoolsMcpRecipeContext {
            model: "current".to_string(),
            bundle_path: Some("bundle.md".into()),
            ..DevtoolsMcpRecipeContext::default()
        };
        let err = run(&ctx).await.unwrap_err();
        assert!(err.to_string().contains("fable-5-max"));
    }
}

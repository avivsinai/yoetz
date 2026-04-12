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
//! - No SPA reset dance — `new_page` creates a fresh page so the thread
//!   starts empty
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
use std::time::Duration;

use super::client::CdpMcpClient;
use super::DevtoolsMcpRecipeContext;

const CHATGPT_URL: &str = "https://chatgpt.com/";

/// Stable-idle polling parameters.
const POLL_INTERVAL_MS: u64 = 5_000;
/// Number of consecutive polls where (messageCount, textLength) must be
/// unchanged before we declare the response complete. At 5s intervals, 4
/// stable polls = 20s of genuinely idle generation, which is enough to
/// distinguish "response finished" from "brief mid-stream pause."
const STABLE_IDLE_CONSECUTIVE_POLLS: u32 = 4;
const UPLOAD_INPUT_WAIT_MS: u64 = 5_000;
const ATTACHMENT_VERIFY_WAIT_MS: u64 = 15_000;

/// The full ChatGPT Pro recipe. Returns the assistant's final response text.
pub async fn run(ctx: &DevtoolsMcpRecipeContext) -> Result<String> {
    if ctx.bundle_path.is_none() {
        return Err(anyhow!(
            "ChatGPT recipe requires `--bundle`; this transport uploads a file attachment and does not support paste mode"
        ));
    }

    // Step 0: attach directly to the user's running Chrome session.
    // The Chrome "Allow remote debugging" dialog fires here, once per Chrome
    // session. Every subsequent yoetz invocation in the same Chrome session
    // should be silent.
    if ctx.show_approval_guidance {
        eprintln!(
            "info: connecting to Chrome via chrome-devtools-mcp — if prompted, click Allow in Chrome's remote debugging dialog (one-time per Chrome session)"
        );
    }
    let client = CdpMcpClient::connect_to_running_chrome(ctx.cdp_endpoint.as_deref())
        .await
        .context("chrome-devtools-mcp attach to running Chrome")?;

    // Step 1: open a fresh chatgpt.com page. Fresh page = zero conversation
    // history, no SPA reset dance needed.
    client
        .new_page(CHATGPT_URL, /* background */ false, 30_000)
        .await
        .context("chrome-devtools-mcp new_page on chatgpt.com")?;

    // Step 2: wait for the composer to mount, then focus it. We use
    // `evaluate_script` rather than the snapshot-uid model because ChatGPT's
    // composer role + accessible name are locale-dependent and unreliable.
    // `#prompt-textarea` has been stable since 2023 (per Agent Y research).
    let wait_composer_js = r##"
async () => {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    const composer = document.querySelector("#prompt-textarea, div[contenteditable='true'][role='textbox']");
    if (composer) {
      composer.focus();
      return true;
    }
    await new Promise(r => setTimeout(r, 200));
  }
  return false;
}
"##;
    let composer_ready = client
        .evaluate_script(wait_composer_js, vec![])
        .await
        .context("evaluate_script wait-for-composer")?;
    if composer_ready != serde_json::Value::Bool(true) {
        return Err(anyhow!(
            "ChatGPT composer did not mount within 20s — page may not have loaded correctly"
        ));
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
    try_upload_bundle(&client, bundle_path)
        .await
        .context("upload bundle to ChatGPT")?;

    // Step 4: type the delivery text into the focused composer.
    //
    // `type_text` types into the currently focused element, which we already
    // focused in Step 2.
    let delivery_text = ctx.prompt.clone();

    // Make sure the composer is still focused (upload step may have stolen
    // focus to the file picker).
    let refocus_js = r##"
() => {
  const composer = document.querySelector("#prompt-textarea, div[contenteditable='true'][role='textbox']");
  if (composer) composer.focus();
  return !!composer;
}
"##;
    let _ = client
        .evaluate_script(refocus_js, vec![])
        .await
        .context("refocus composer after upload")?;

    client
        .type_text(&delivery_text, /* submit_key */ None)
        .await
        .context("type_text into ChatGPT composer")?;

    // Step 5: click the send button via JS (CSS selector).
    //
    // We match by data-testid with a fallback chain because the testid has
    // bounced between `send-button` / `fruitjuice-send-button` / back. Also
    // fall back to the last submit button in the composer form.
    let click_send_js = r##"
() => {
  const candidates = [
    "[data-testid='send-button']",
    "[data-testid='fruitjuice-send-button']",
    "form button[type='submit']:last-of-type",
  ];
  for (const sel of candidates) {
    const btn = document.querySelector(sel);
    if (btn && !btn.disabled) {
      btn.click();
      return sel;
    }
  }
  return null;
}
"##;
    let clicked = client
        .evaluate_script(click_send_js, vec![])
        .await
        .context("evaluate_script click send button")?;
    if clicked == serde_json::Value::Null {
        return Err(anyhow!(
            "could not find an enabled ChatGPT send button (tried data-testid='send-button', fruitjuice-send-button, form button[type='submit']:last-of-type)"
        ));
    }

    // Step 6: stable-idle polling for response completion.
    //
    // Heuristic (ported from yoetz v0.2.33 Pro Extended auto-poll):
    // - Absence of `.result-streaming` class on the last assistant message
    // - (messageCount, textLength) unchanged across N consecutive polls
    //
    // This replaces the unreliable "Regenerate" button wait_for. Agent Y
    // research showed "Regenerate" is missing or inconsistently placed in
    // many ChatGPT flows (Custom GPT, Canvas, certain Pro modes).
    let response_text = poll_for_stable_response(&client, ctx.response_timeout_ms)
        .await
        .context("stable-idle polling for ChatGPT response")?;

    Ok(response_text)
}

/// Click the attach button, snapshot the mounted upload affordance, then call
/// `upload_file`. `upload_file` accepts either the file input itself or the
/// element that opens the file chooser, so prefer visible menu items/buttons
/// over guessing a hidden input uid.
async fn try_upload_bundle(client: &CdpMcpClient, bundle_path: &std::path::Path) -> Result<()> {
    let file_name = bundle_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .context("bundle path must end in a UTF-8 filename")?;

    if try_upload_via_file_input(client, bundle_path, &file_name)
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

        if try_upload_via_file_input(client, bundle_path, &file_name)
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
            if try_upload_via_file_input(client, bundle_path, &file_name)
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

async fn try_upload_via_file_input(
    client: &CdpMcpClient,
    bundle_path: &std::path::Path,
    file_name: &str,
) -> Result<bool> {
    let input_uid = wait_for_file_input_uid(client, UPLOAD_INPUT_WAIT_MS)
        .await
        .context("wait for file input")?;

    let Some(input_uid) = input_uid else {
        return Ok(false);
    };

    client
        .upload_file(&input_uid, bundle_path)
        .await
        .with_context(|| format!("upload_file via input `{input_uid}`"))?;

    wait_for_attachment_visible(client, file_name)
        .await
        .with_context(|| format!("verify attachment chip for `{file_name}`"))?;

    Ok(true)
}

async fn wait_for_file_input_uid(client: &CdpMcpClient, timeout_ms: u64) -> Result<Option<String>> {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);

    loop {
        let snapshot = client
            .take_snapshot(false)
            .await
            .context("take snapshot while searching for file input")?;
        if let Some(uid) = snapshot.find_file_input_uid() {
            return Ok(Some(uid));
        }

        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn click_upload_candidate(client: &CdpMcpClient, candidate_id: &str) -> Result<bool> {
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

async fn click_upload_menu_item(client: &CdpMcpClient) -> Result<bool> {
    let script = r##"
() => {
  const selectors = ["[role='menuitem']", "button", "[role='button']", "label", "li"];
  const nodes = Array.from(document.querySelectorAll(selectors.join(",")));
  const target = nodes.find((el) => {
    const text = `${el.innerText || ""} ${el.getAttribute?.("aria-label") || ""} ${el.getAttribute?.("title") || ""}`
      .replace(/\s+/g, " ")
      .trim()
      .toLowerCase();
    return /upload from computer|from computer|upload files|choose files|browse/i.test(text);
  });
  if (!target) return false;
  target.click();
  return true;
}
"##;

    Ok(client.evaluate_script(script, vec![]).await? == serde_json::Value::Bool(true))
}

async fn collect_upload_diagnostics(client: &CdpMcpClient) -> Result<serde_json::Value> {
    let script = r##"
() => {
  const composer = document.querySelector("#prompt-textarea, div[contenteditable='true'][role='textbox']");
  const composerRect = composer?.getBoundingClientRect() || null;
  const composerForm = composer?.closest("form") || null;

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
"##;

    client
        .evaluate_script(script, vec![])
        .await
        .context("evaluate upload diagnostics")
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

async fn wait_for_attachment_visible(client: &CdpMcpClient, file_name: &str) -> Result<()> {
    let file_name_json = serde_json::to_string(file_name)?;
    let file_stem = std::path::Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(file_name);
    let file_stem_json = serde_json::to_string(file_stem)?;
    let script = format!(
        r#"
async () => {{
  const fileName = {file_name_json};
  const fileStem = {file_stem_json};
  const deadline = Date.now() + {ATTACHMENT_VERIFY_WAIT_MS};
  const clip = (value, max = 120) => String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
  while (Date.now() < deadline) {{
    const visibleEvidence = Array.from(document.querySelectorAll("button, span, div, li"))
      .map((el) => {{
        const text = clip(el.innerText || el.textContent || "");
        const ariaLabel = clip(el.getAttribute?.("aria-label") || "");
        const title = clip(el.getAttribute?.("title") || "");
        return {{ text, ariaLabel, title }};
      }})
      .filter((entry) => {{
        const combined = `${{entry.text}} ${{entry.ariaLabel}} ${{entry.title}}`;
        return combined.includes(fileName) || (fileStem && combined.includes(fileStem));
      }})
      .slice(0, 6);

    if (visibleEvidence.length > 0) {{
      return {{ ok: true, visibleEvidence }};
    }}

    await new Promise((resolve) => setTimeout(resolve, 250));
  }}

  const inputs = Array.from(document.querySelectorAll("input[type='file']")).map((input) => ({{
    fileNames: Array.from(input.files || []).map((file) => file.name),
    multiple: !!input.multiple,
  }}));
  return {{ ok: false, inputs }};
}}
"#
    );

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

/// Stable-idle polling for ChatGPT response completion.
///
/// Ported from the v0.2.33 Pro Extended auto-poll heuristic. Returns the
/// final assistant message text once the response has been idle for
/// `STABLE_IDLE_CONSECUTIVE_POLLS * POLL_INTERVAL_MS` milliseconds.
async fn poll_for_stable_response(
    client: &CdpMcpClient,
    overall_timeout_ms: u64,
) -> Result<String> {
    let read_state_js = r##"
() => {
  const nodes = Array.from(document.querySelectorAll("[data-message-author-role='assistant']"));
  if (nodes.length === 0) {
    return { ready: false, count: 0, length: 0, text: null, streaming: false };
  }
  const last = nodes[nodes.length - 1];
  const streaming = !!last.querySelector(".result-streaming") || last.classList.contains("result-streaming");
  const text = last.innerText || "";
  return {
    ready: !streaming && text.length > 0,
    count: nodes.length,
    length: text.length,
    text,
    streaming,
  };
}
"##;

    let start = std::time::Instant::now();
    let overall_timeout = Duration::from_millis(overall_timeout_ms);
    let mut last_count: i64 = -1;
    let mut last_length: i64 = -1;
    let mut stable_polls: u32 = 0;

    loop {
        if start.elapsed() > overall_timeout {
            return Err(anyhow!(
                "ChatGPT response did not complete within {overall_timeout_ms}ms"
            ));
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;

        let state = match client.evaluate_script(read_state_js, vec![]).await {
            Ok(state) => state,
            Err(err) => {
                // Treat transient eval errors as non-fatal; keep polling.
                eprintln!("warn: stable-idle poll eval failed ({err:#}), retrying");
                continue;
            }
        };

        let Some(obj) = state.as_object() else {
            eprintln!("warn: stable-idle poll returned non-object: {state}");
            continue;
        };

        let ready = obj.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);
        let count = obj.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        let length = obj.get("length").and_then(|v| v.as_i64()).unwrap_or(0);
        let streaming = obj
            .get("streaming")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if !ready || streaming {
            // Reset stability counter; still streaming.
            last_count = count;
            last_length = length;
            stable_polls = 0;
            continue;
        }

        // `ready == true && !streaming`: candidate complete. Check stability.
        if count == last_count && length == last_length {
            stable_polls += 1;
            if stable_polls >= STABLE_IDLE_CONSECUTIVE_POLLS {
                let text = obj
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() {
                    return Err(anyhow!(
                        "stable-idle reached but assistant message text is empty"
                    ));
                }
                return Ok(text);
            }
        } else {
            // State changed between polls — still technically streaming.
            last_count = count;
            last_length = length;
            stable_polls = 1; // First stable observation at the new state
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(POLL_INTERVAL_MS >= 1_000);
        assert!(STABLE_IDLE_CONSECUTIVE_POLLS >= 2);
        assert!(!CHATGPT_URL.is_empty());
    };
}

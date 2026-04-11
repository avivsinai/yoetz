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
//! - No Playwright `connectOverCDP` hang — chrome-devtools-mcp uses Puppeteer
//!   which handles Chrome 147's target bootstrap correctly
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

/// The full ChatGPT Pro recipe. Returns the assistant's final response text.
pub async fn run(ctx: &DevtoolsMcpRecipeContext) -> Result<String> {
    if ctx.bundle_path.is_none() && ctx.bundle_text.is_none() {
        return Err(anyhow!(
            "ChatGPT recipe requires either a bundle file (--bundle) or inline bundle text"
        ));
    }

    // Step 0: bring up the MCP client. chrome-devtools-mcp attaches to user's
    // running Chrome via its internal Puppeteer `connect({browserURL})` call.
    // The Chrome 147 "Allow remote debugging" dialog fires here, once per
    // Chrome session. Every subsequent yoetz invocation in the same Chrome
    // session is silent.
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
    // If the attach button is missing or the upload flow fails, fall back
    // to inlining the bundle text into the prompt (paste mode).
    let uploaded = if let Some(bundle_path) = &ctx.bundle_path {
        try_upload_bundle(&client, bundle_path)
            .await
            .unwrap_or_else(|err| {
                eprintln!(
                    "warn: chrome-devtools-mcp upload failed ({err:#}), falling back to paste mode"
                );
                false
            })
    } else {
        false
    };

    // Step 4: type the delivery text into the focused composer.
    //
    // `type_text` types into the currently focused element, which we already
    // focused in Step 2. If upload succeeded, the delivery text is just the
    // user's prompt. Otherwise inline the bundle.
    let delivery_text = if uploaded {
        ctx.prompt.clone()
    } else if let Some(text) = &ctx.bundle_text {
        format!("{}\n\n{}", ctx.prompt, text)
    } else {
        // No bundle available and no paste-mode text — just send the prompt.
        ctx.prompt.clone()
    };

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

/// Click the attach button, snapshot to find the lazy-mounted file input,
/// call `upload_file`. Returns true on success, errors or false on fallback.
async fn try_upload_bundle(client: &CdpMcpClient, bundle_path: &std::path::Path) -> Result<bool> {
    // Trigger the lazy mount of `<input type="file">` by clicking the attach
    // button. No stable testid — match by aria-label containing "Attach".
    let click_attach_js = r##"
() => {
  const selectors = [
    "button[aria-label*='Attach' i]",
    "button[aria-label*='Upload' i]",
    "button[data-testid*='attach' i]",
  ];
  for (const sel of selectors) {
    const btn = document.querySelector(sel);
    if (btn) { btn.click(); return sel; }
  }
  return null;
}
"##;
    let clicked = client
        .evaluate_script(click_attach_js, vec![])
        .await
        .context("evaluate_script click attach button")?;
    if clicked == serde_json::Value::Null {
        return Err(anyhow!("attach button not found via aria-label"));
    }

    // Give the menu / file picker a moment to mount the hidden input.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Some ChatGPT variants open a menu with "Upload from computer" rather
    // than mounting the file input directly. Try that path too.
    let menu_click_js = r##"
() => {
  const items = Array.from(document.querySelectorAll("[role='menuitem'], li"));
  const target = items.find((el) => /upload|from computer/i.test(el.textContent || ''));
  if (target) { target.click(); return true; }
  return false;
}
"##;
    let _ = client.evaluate_script(menu_click_js, vec![]).await;

    // Now snapshot and find the upload input's uid.
    //
    // The snapshot should include the newly-mounted `<input type="file">`
    // as a node. We look for it by searching for "file" in input type.
    let snapshot = client
        .take_snapshot(/* verbose */ false)
        .await
        .context("take_snapshot after attach click")?;

    let upload_uid = snapshot.find_uid_by_text("file").or_else(|| {
        // Fallback: some snapshot renderers label the input as
        // "file upload" or similar.
        snapshot
            .find_uid_by_role("textbox", "File upload")
            .or_else(|| snapshot.find_uid_by_role("button", "Upload"))
    });

    let uid = match upload_uid {
        Some(uid) => uid,
        None => {
            return Err(anyhow!(
                "upload input not found in snapshot after attach click"
            ))
        }
    };

    client
        .upload_file(&uid, bundle_path)
        .await
        .context("chrome-devtools-mcp upload_file")?;

    // Give ChatGPT a moment to process the upload + show the attachment chip.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    Ok(true)
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

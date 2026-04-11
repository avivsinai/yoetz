//! ChatGPT Pro recipe over chrome-devtools-mcp.
//!
//! Seven-step flow, each step a typed `CdpMcpClient` tool call. Drops the
//! dev-browser prepare/send/poll dance with SPA reset + readState +
//! assistant-count baseline — those were Playwright-quirk workarounds. The
//! chrome-devtools-mcp uid-from-snapshot model makes each primitive a clean
//! function call.
//!
//! Steps:
//! 1. `new_page https://chatgpt.com/` — fresh page attached to running Chrome
//! 2. `take_snapshot` — a11y tree with uids for composer + upload + send
//! 3. `upload_file <composer-attach-uid> <bundle.md path>` — file upload
//! 4. `take_snapshot` — re-snapshot to confirm attachment landed
//! 5. `type_text <prompt>` — typed via the composer's focused input
//! 6. `click <send-button-uid>` — submit
//! 7. `wait_for` response completion marker then `evaluate_script` to extract
//!    the final assistant message text

use anyhow::{anyhow, Context, Result};

use super::client::CdpMcpClient;
use super::DevtoolsMcpRecipeContext;

const CHATGPT_URL: &str = "https://chatgpt.com/";

/// Hints the recipe uses to find uids in the a11y snapshot. Each is a role +
/// name pair that `Snapshot::find_uid_by_role` matches.
///
/// These are the ChatGPT 2026-04 DOM contract. If OpenAI renames the role or
/// label, update here. A parallel research sub-agent is verifying the current
/// values against live chatgpt.com HTML — results will land in
/// `~/.claude/projects/-Users-avivsinai-MyProjects-yoetz/memory/project_chatgpt_dom_selectors.md`.
mod hints {
    /// Composer textarea (ProseMirror contenteditable).
    pub const COMPOSER_ROLE: &str = "textbox";
    pub const COMPOSER_NAME: &str = "Message ChatGPT";

    /// Send button.
    pub const SEND_ROLE: &str = "button";
    pub const SEND_NAME: &str = "Send prompt";

    /// File upload / attach button (paperclip in the composer).
    /// The chrome-devtools-mcp `upload_file` tool requires an input[type=file]
    /// uid; usually this is the hidden `input` the paperclip triggers.
    pub const UPLOAD_ROLE: &str = "button";
    pub const UPLOAD_NAME: &str = "Upload files and more";

    /// Response-ready markers for `wait_for`.
    pub const STOP_GENERATING_TEXT: &str = "Stop generating";
    pub const REGENERATE_TEXT: &str = "Regenerate";
}

/// The full ChatGPT Pro recipe. Returns the assistant's final response text.
pub async fn run(ctx: &DevtoolsMcpRecipeContext) -> Result<String> {
    if ctx.bundle_path.is_none() && ctx.bundle_text.is_none() {
        return Err(anyhow!(
            "ChatGPT recipe requires either a bundle file (--bundle) or inline bundle text"
        ));
    }

    // Step 0: bring up the MCP client. Attach to user's running Chrome via
    // chrome-devtools-mcp's `--browser-url` arg under the hood. This is the
    // step where Chrome 147's "Allow remote debugging" dialog fires (once per
    // Chrome session). Everything after this is silent.
    if ctx.show_approval_guidance {
        eprintln!(
            "info: connecting to Chrome via chrome-devtools-mcp — if prompted, click Allow in Chrome's remote debugging dialog (one-time per Chrome session)"
        );
    }
    let client = CdpMcpClient::connect_to_running_chrome(ctx.cdp_endpoint.as_deref())
        .await
        .context("chrome-devtools-mcp attach to running Chrome")?;

    // Step 1: open chatgpt.com on a fresh page. Fresh page = zero conversation
    // history, no SPA reset dance needed.
    client
        .new_page(CHATGPT_URL, /* background */ false, 30_000)
        .await
        .context("chrome-devtools-mcp new_page on chatgpt.com")?;

    // Step 2: first snapshot, find composer + upload + send uids.
    let snapshot = client
        .take_snapshot(/* verbose */ false)
        .await
        .context("take_snapshot after chatgpt.com load")?;

    let composer_uid = snapshot
        .find_uid_by_role(hints::COMPOSER_ROLE, hints::COMPOSER_NAME)
        .or_else(|| snapshot.find_uid_by_role("textbox", "Message"))
        .ok_or_else(|| {
            anyhow!(
                "could not find ChatGPT composer input in the a11y snapshot. \
                 The composer role/name may have changed in the current ChatGPT DOM."
            )
        })?;

    let upload_uid = snapshot
        .find_uid_by_role(hints::UPLOAD_ROLE, hints::UPLOAD_NAME)
        .or_else(|| snapshot.find_uid_by_role("button", "Attach files"));

    // Step 3: upload the bundle if we have a path + an upload uid.
    let uploaded = match (&ctx.bundle_path, &upload_uid) {
        (Some(path), Some(uid)) => {
            client
                .upload_file(uid, path)
                .await
                .context("chrome-devtools-mcp upload_file for bundle.md")?;
            true
        }
        (Some(_), None) => {
            eprintln!(
                "warn: ChatGPT upload button not visible in snapshot — falling back to paste mode"
            );
            false
        }
        (None, _) => false,
    };

    // Step 4: re-snapshot after upload to confirm attachment landed. If we
    // uploaded, the snapshot should now show an attachment chip; the send
    // button becomes enabled; composer still has the same uid.
    let snapshot_after_upload = client
        .take_snapshot(false)
        .await
        .context("take_snapshot after upload attempt")?;

    let send_uid = snapshot_after_upload
        .find_uid_by_role(hints::SEND_ROLE, hints::SEND_NAME)
        .or_else(|| snapshot_after_upload.find_uid_by_role("button", "Send message"))
        .ok_or_else(|| {
            anyhow!(
                "could not find ChatGPT send button in the a11y snapshot after upload attempt. \
                 The send button role/name may have changed in the current ChatGPT DOM."
            )
        })?;

    // Step 5: type the delivery text into the composer.
    //
    // The chrome-devtools-mcp `type_text` tool types into the currently
    // focused element. We click the composer first to make sure it's focused.
    let delivery_text = if uploaded {
        ctx.prompt.clone()
    } else if let Some(text) = &ctx.bundle_text {
        format!("{}\n\n{}", ctx.prompt, text)
    } else {
        // Upload failed AND no paste-mode text: fall back to prompt-only.
        // Better to send a short prompt than nothing.
        ctx.prompt.clone()
    };

    client
        .click(&composer_uid, /* double_click */ false)
        .await
        .context("click ChatGPT composer to focus")?;

    client
        .type_text(&delivery_text, /* submit_key */ None)
        .await
        .context("type_text into ChatGPT composer")?;

    // Step 6: click send.
    client
        .click(&send_uid, false)
        .await
        .context("click ChatGPT send button")?;

    // Step 7: wait for the response-ready markers, then extract.
    //
    // Completion heuristic: after send, ChatGPT shows "Stop generating" while
    // the response streams. When the stream finishes, "Stop generating" is
    // replaced with "Regenerate". So we wait for "Regenerate" as the strong
    // completion signal.
    let _ = client
        .wait_for(
            &[hints::REGENERATE_TEXT],
            ctx.response_timeout_ms.max(60_000),
        )
        .await
        .context("wait_for ChatGPT response completion (Regenerate marker)")?;

    // Extract the final assistant message text via evaluate_script. The JS
    // runs in the page, reads the last [data-message-author-role='assistant']
    // element's innerText, returns it as a string.
    let extract_js = r#"
() => {
  const nodes = document.querySelectorAll("[data-message-author-role='assistant']");
  if (nodes.length === 0) return null;
  const last = nodes[nodes.length - 1];
  return last ? last.innerText : null;
}
"#;

    let response_json = client
        .evaluate_script(extract_js, vec![])
        .await
        .context("evaluate_script to extract ChatGPT assistant response")?;

    match response_json {
        serde_json::Value::String(text) if !text.is_empty() => Ok(text),
        serde_json::Value::Null => Err(anyhow!(
            "ChatGPT recipe completed but no assistant message was found on the page"
        )),
        other => Err(anyhow!(
            "ChatGPT recipe extract returned unexpected JSON shape: {other}"
        )),
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

    #[test]
    fn hints_are_non_empty() {
        // Guard against a bad refactor silently removing every hint.
        assert!(!hints::COMPOSER_ROLE.is_empty());
        assert!(!hints::COMPOSER_NAME.is_empty());
        assert!(!hints::SEND_ROLE.is_empty());
        assert!(!hints::SEND_NAME.is_empty());
        assert!(!hints::REGENERATE_TEXT.is_empty());
    }
}

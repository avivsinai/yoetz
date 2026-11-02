//! Shared claude.ai contracts and DOM-script builders for browser transports.

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use crate::web_recipe::{WebConversation, WebModelSelectionStatus};

pub const CLAUDE_URL: &str = "https://claude.ai/new";
pub const COMPOSER_SELECTOR: &str = "[data-testid='chat-input']";
pub const MODEL_SELECTOR: &str = "[data-testid='model-selector-dropdown']";
pub const FILE_INPUT_SELECTOR: &str = "input[data-testid='file-upload']";
pub const ATTACHMENT_SELECTOR: &str = "[data-testid='file-thumbnail']";
/// Attachment card captured 2026-11-02: the filename is a `[title]` on the
/// card link and the remove button reads "Remove <file>". Older cards: `h3`,
/// "Remove". Same reads as `extensions/chatgpt-native/src/claude-dom.js`.
const ATTACHMENT_REMOVE_SELECTOR: &str = "button[aria-label='Remove'], button[aria-label^='Remove '], button[data-cds-attachment-remove]";
pub const COPY_ACTION_SELECTOR: &str = "[data-testid='action-bar-copy']";
/// claude.ai picker (Base UI menu captured 2026-11-02, no testids): models are
/// `menuitemradio` rows with `data-model-id`; Effort is a submenu `menuitem`
/// whose text starts with "Effort"; its levels are `menuitemradio` rows without
/// `data-model-id`. Base UI keeps a closed menu mounted with `data-closed`, so
/// `visible` also rejects items inside one. Same reads as
/// `extensions/chatgpt-native/src/claude-dom.js`.
const MENU_VISIBLE_JS: &str = r#"(el) => !!el && el.getClientRects().length > 0 && !el.closest("[role='menu'][data-closed]")"#;
const FIND_EFFORT_TRIGGER_JS: &str = r#"Array.from(document.querySelectorAll("[role='menuitem'][aria-haspopup='menu'], [data-testid='effort-menu-trigger']")).filter(visible).find((el) => normalize(el.innerText || el.textContent).toLowerCase().startsWith("effort"))"#;
const EFFORT_OPTIONS_JS: &str = r#"Array.from(document.querySelectorAll("[role='menuitemradio']:not([data-model-id])")).filter(visible)"#;
pub const EFFORT_HOVER_MARKER: &str = "yoetz-claude-effort-target";
pub const FILE_INPUT_MARKER: &str = "yoetz-claude-upload-target";
pub const STABLE_IDLE_FLOOR_MS: u64 = 90_000;
pub const STABLE_IDLE_INTERVAL_MULTIPLIER: u64 = 3;

pub fn stable_idle_threshold_ms(interval_ms: u64) -> u64 {
    interval_ms
        .saturating_mul(STABLE_IDLE_INTERVAL_MULTIPLIER)
        .max(STABLE_IDLE_FLOOR_MS)
}

pub fn generate_run_id() -> String {
    crate::chatgpt_web::generate_run_id()
}

pub fn validate_thread_mode(raw: Option<&str>) -> Result<()> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("fresh") => Ok(()),
        Some(other) => bail!(
            "Claude chrome-devtools-mcp supports only `thread=fresh`; got `{other}`. Conversation resume is native-extension-only"
        ),
    }
}

pub fn normalize_conversation(raw: &str) -> Result<WebConversation> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("Claude conversation must not be empty");
    }
    let id = if raw.contains("://") {
        conversation_id_from_url(raw)?
    } else {
        raw.to_string()
    };
    validate_conversation_id(&id)?;
    Ok(WebConversation {
        url: conversation_url(&id),
        id,
    })
}

pub fn conversation_url(conversation_id: &str) -> String {
    format!("https://claude.ai/chat/{conversation_id}")
}

fn conversation_id_from_url(raw: &str) -> Result<String> {
    let prefix = "https://claude.ai/chat/";
    if !raw.starts_with(prefix) {
        bail!("Claude conversation URL must use https://claude.ai/chat/<uuid>");
    }
    let id = &raw[prefix.len()..];
    if id.contains(['?', '#', '/']) {
        bail!("Claude conversation URL must not contain a query or fragment");
    }
    Ok(id.to_string())
}

fn validate_conversation_id(id: &str) -> Result<()> {
    let bytes = id.as_bytes();
    let valid = bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit());
    if !valid {
        bail!("Claude conversation id must be a UUID");
    }
    Ok(())
}

pub fn mark_claude_url(run_id: &str) -> Result<String> {
    crate::chatgpt_web::validate_run_id(run_id)
        .map_err(|_| anyhow!("invalid Claude run id `{run_id}`"))?;
    Ok(format!("{CLAUDE_URL}?_yoetz={run_id}"))
}

pub fn build_set_window_name_js(run_id: &str) -> Result<String> {
    crate::chatgpt_web::validate_run_id(run_id)
        .map_err(|_| anyhow!("invalid Claude run id `{run_id}`"))?;
    let marker = serde_json::to_string(&format!("yoetz:{run_id}"))?;
    Ok(format!(
        "() => {{ window.name = {marker}; return window.name; }}"
    ))
}

pub fn build_wait_for_composer_function() -> String {
    format!(
        r##"async () => {{
  const deadline = Date.now() + 20000;
  const read = () => {{
    const composer = document.querySelector({composer});
    const url = window.location.href || "";
    const title = document.title || "";
    const bodyText = String(document.body?.innerText || "").replace(/\s+/g, " ").trim().slice(0, 300);
    const haystack = `${{title}} ${{bodyText}}`.toLowerCase();
    if (composer) return {{ status: "ready", url, title, bodyText }};
    if (/cloudflare|checking your browser|attention required|security check|just a moment|verify you are human|cf-chl/.test(haystack))
      return {{ status: "challenge", url, title, bodyText }};
    if (/log in|login|sign in|sign up|continue with google|\/login|\/oauth/.test(`${{haystack}} ${{url.toLowerCase()}}`))
      return {{ status: "login", url, title, bodyText }};
    return {{ status: "pending", url, title, bodyText }};
  }};
  let state = read();
  while (state.status === "pending" && Date.now() < deadline) {{
    await new Promise((resolve) => setTimeout(resolve, 200));
    state = read();
  }}
  if (state.status === "pending") state.status = "timeout";
  return state;
}}"##,
        composer = serde_json::to_string(COMPOSER_SELECTOR).expect("selector JSON")
    )
}

pub fn build_open_model_menu_function() -> String {
    format!(
        r##"() => {{
  const button = document.querySelector({selector});
  if (!button) return {{ status: "missing", diagnostics: {{ selector: {selector} }} }};
  if (button.getAttribute("aria-expanded") !== "true") button.click();
  return {{ status: button.getAttribute("aria-expanded") === "true" ? "opened" : "opening", label: (button.innerText || button.textContent || "").trim() }};
}}"##,
        selector = serde_json::to_string(MODEL_SELECTOR).expect("selector JSON")
    )
}

pub fn build_close_model_menu_function() -> String {
    format!(
        r##"() => {{
  const button = document.querySelector({selector});
  if (!button) return {{ status: "missing" }};
  if (button.getAttribute("aria-expanded") === "true") button.click();
  return {{ status: "closed" }};
}}"##,
        selector = serde_json::to_string(MODEL_SELECTOR).expect("selector JSON")
    )
}

pub fn build_select_fable_function() -> String {
    format!(
        r##"() => {{
  const visible = {menu_visible};
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const options = Array.from(document.querySelectorAll("[role='menuitemradio']"))
    .filter(visible)
    .map((el) => ({{ el, text: normalize(el.innerText || el.textContent) }}))
    .filter(({{ text }}) => text);
  const fable = options.find(({{ text }}) => text.toLowerCase().startsWith("fable 5"));
  if (!fable) return {{ status: "unavailable", options: options.map(({{ text }}) => text).slice(0, 40) }};
  fable.el.click();
  return {{ status: "selected", selected: fable.text, options: options.map(({{ text }}) => text).slice(0, 40) }};
}}"##,
        menu_visible = MENU_VISIBLE_JS,
    )
}

/// Mark the visible Effort parent row so the CDP driver can try a physical
/// mouse move before falling back to the DOM hover sequence below.
pub fn build_mark_effort_parent_function() -> String {
    format!(
        r##"() => {{
  const visible = {menu_visible};
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  document.querySelectorAll(`[title='{marker}']`).forEach((el) => el.removeAttribute("title"));
  const options = Array.from(document.querySelectorAll("[role='menuitem'], [role='menuitemradio']"))
    .filter(visible)
    .map((el) => ({{ el, text: normalize(el.innerText || el.textContent) }}))
    .filter(({{ text }}) => text);
  const effort = {find_effort};
  const effortText = normalize(effort?.innerText || effort?.textContent);
  if (!effort) return {{ status: "unavailable", options: options.map(({{ text }}) => text).slice(0, 40) }};
  effort.setAttribute("title", "{marker}");
  return {{ status: "marked", marker: "{marker}", text: effortText, options: options.map(({{ text }}) => text).slice(0, 40) }};
}}"##,
        marker = EFFORT_HOVER_MARKER,
        find_effort = FIND_EFFORT_TRIGGER_JS,
        menu_visible = MENU_VISIBLE_JS,
    )
}

/// Open Claude's Effort submenu with the attributed pointer/mouse sequence
/// used by the native extension. Claude's Base UI hover intent can ignore a
/// single physical mouse teleport while the model menu is still animating.
pub fn build_open_effort_submenu_function() -> String {
    format!(
        r##"() => {{
  const visible = {menu_visible};
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const existing = {effort_options}
    .map((el) => normalize(el.innerText || el.textContent))
    .filter(Boolean);
  if (existing.length > 0) return {{ status: "already_open", effortOptions: existing }};
  const effort = {find_effort};
  if (!visible(effort)) return {{ status: "unavailable", effortOptions: existing }};
  const rect = effort.getBoundingClientRect?.() || {{ left: 0, top: 0, width: 1, height: 1 }};
  const clientX = Number(rect.left || 0) + Math.max(1, Number(rect.width || 1) / 2);
  const clientY = Number(rect.top || 0) + Math.max(1, Number(rect.height || 1) / 2);
  for (const type of ["pointerover", "pointerenter", "pointermove", "mouseover", "mouseenter", "mousemove"]) {{
    const pointer = type.startsWith("pointer") && typeof globalThis.PointerEvent !== "undefined";
    const EventType = pointer ? globalThis.PointerEvent : globalThis.MouseEvent;
    if (typeof EventType !== "function") continue;
    effort.dispatchEvent(new EventType(type, {{
      bubbles: true,
      cancelable: true,
      view: globalThis.window,
      clientX,
      clientY,
      ...(pointer ? {{ pointerType: "mouse", pointerId: 1, isPrimary: true }} : {{}})
    }}));
  }}
  return {{ status: "dispatched", effort: normalize(effort.innerText || effort.textContent), effortOptions: existing }};
}}"##,
        effort_options = EFFORT_OPTIONS_JS,
        find_effort = FIND_EFFORT_TRIGGER_JS,
        menu_visible = MENU_VISIBLE_JS,
    )
}

pub fn build_select_max_function() -> String {
    format!(
        r##"() => {{
  const visible = {menu_visible};
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const options = {effort_options}
    .map((el) => ({{ el, text: normalize(el.innerText || el.textContent) }}))
    .filter(({{ text }}) => text);
  const effortOptions = options.map(({{ text }}) => text);
  if (options.length === 0) return {{ status: "unavailable", reason: "effort submenu did not open", effortOptions }};
  const max = options.find(({{ el, text }}) => text.toLowerCase().startsWith("max")
    && el.getAttribute("aria-disabled") !== "true" && !el.hasAttribute("data-disabled"));
  if (!max) return {{ status: "unavailable", reason: `no enabled Max effort; live effort options: ${{effortOptions.join(", ")}}`, effortOptions }};
  max.el.click();
  return {{ status: "selected", selected: max.text, effortOptions }};
}}"##,
        effort_options = EFFORT_OPTIONS_JS,
        menu_visible = MENU_VISIBLE_JS,
    )
}

/// Re-read both exact Claude model postconditions after selection.
///
/// Fable 5 is a checked model `menuitemradio` and Max a checked effort
/// `menuitemradio`; there is no independent Thinking control. A successful
/// click is never proof for either remaining leg.
pub fn build_verify_fable_max_function() -> String {
    format!(
        r##"() => {{
  const visible = {menu_visible};
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const modelChip = normalize(document.querySelector({model_selector})?.innerText || document.querySelector({model_selector})?.textContent);
  const visibleText = Array.from(document.querySelectorAll("[role='menuitem'], [role='menuitemradio'], button, [role='switch']"))
    .filter(visible).map((el) => normalize(el.innerText || el.textContent)).filter(Boolean);
  const effortChip = Array.from(document.querySelectorAll("button, [role='button']"))
    .filter(visible).map((el) => normalize(el.innerText || el.textContent))
    .find((text) => /^(Low|Medium|High|Extra|Max)(?:\b|$)/i.test(text)) || "";
  const modelSelected = Array.from(document.querySelectorAll("[role='menuitemradio'][aria-checked='true']"))
    .filter(visible).some((el) => normalize(el.innerText || el.textContent).toLowerCase().startsWith("fable 5"));
  const maxChecked = Array.from(document.querySelectorAll("[role='menuitemradio'][aria-checked='true']:not([data-model-id])"))
    .filter(visible).some((el) => normalize(el.innerText || el.textContent).toLowerCase().startsWith("max"));
  const modelVerified = modelSelected && /\bFable 5\b/i.test(modelChip);
  const maxVerified = maxChecked && /\bMax\b/i.test(modelChip);
  return {{
    status: modelVerified && maxVerified ? "selected" : "mismatch",
    modelVerified, maxVerified, modelChip, effortChip,
    options: visibleText.slice(0, 50),
  }};
}}"##,
        model_selector = serde_json::to_string(MODEL_SELECTOR).expect("selector JSON"),
        menu_visible = MENU_VISIBLE_JS,
    )
}

pub fn model_selection_status(value: &Value) -> WebModelSelectionStatus {
    match value.get("status").and_then(Value::as_str) {
        Some("selected")
            if value.get("modelVerified").and_then(Value::as_bool) == Some(true)
                && value.get("maxVerified").and_then(Value::as_bool) == Some(true) =>
        {
            WebModelSelectionStatus::Selected
        }
        Some("unavailable") => WebModelSelectionStatus::Unavailable,
        _ => WebModelSelectionStatus::Mismatch,
    }
}

pub fn build_scope_file_input_function() -> String {
    format!(
        r##"() => {{
  const inputs = Array.from(document.querySelectorAll({selector}));
  if (inputs.length !== 1) return {{ status: "unavailable", selector: {selector}, count: inputs.length }};
  const input = inputs[0];
  input.setAttribute("title", "{marker}");
  return {{ status: "marked", marker: "{marker}", count: inputs.length }};
}}"##,
        selector = serde_json::to_string(FILE_INPUT_SELECTOR).expect("selector JSON"),
        marker = FILE_INPUT_MARKER
    )
}

pub fn build_attachment_probe_function(file_name: &str) -> Result<String> {
    build_attachment_probe_function_for(file_name, true)
}

/// yz-w4s: a file after the first must match its own tile by exact name; the
/// "Pasted content" fallback would accept an earlier file's tile.
pub fn build_attachment_probe_function_for(
    file_name: &str,
    accept_pasted_content: bool,
) -> Result<String> {
    let file_name = serde_json::to_string(file_name)?;
    Ok(format!(
        r##"() => {{
  const expected = {file_name};
  const nodes = Array.from(document.querySelectorAll("{attachment_selector}"));
  const label = (node) => String(node.querySelector("[title]")?.getAttribute("title") || node.querySelector("h3")?.textContent || "").replace(/\s+/g, " ").trim();
  const removable = (node) => !!node.querySelector("{remove_selector}");
  const labels = nodes.map(label);
  const matched = nodes.find((node) => label(node) === expected && removable(node));
  const pastedContent = {accept_pasted_content} && nodes.find((node) =>
    `${{label(node)}} ${{node.textContent || ""}}`.replace(/\s+/g, " ").trim().toLowerCase().includes("pasted content") &&
    removable(node)
  );
  const send = document.querySelector("button[aria-label='Send message']");
  const sendEnabled = !!send && !send.disabled && send.getAttribute("aria-disabled") !== "true";
  return {{ status: (matched || pastedContent) && sendEnabled ? "candidate" : "pending", count: nodes.length, labels, sendEnabled, pastedContent: !!pastedContent }};
}}"##,
        attachment_selector = ATTACHMENT_SELECTOR,
        remove_selector = ATTACHMENT_REMOVE_SELECTOR,
    ))
}

pub fn build_focus_composer_function() -> String {
    format!(
        "() => {{ const el = document.querySelector({}); if (el) el.focus(); return !!el; }}",
        serde_json::to_string(COMPOSER_SELECTOR).expect("selector JSON")
    )
}

pub fn build_send_function() -> String {
    format!(
        r##"() => {{
  const assistantRoots = Array.from(document.querySelectorAll("[data-is-streaming]"));
  const copyButtons = document.querySelectorAll("{copy_selector}").length;
  const candidates = Array.from(document.querySelectorAll("button"));
  const send = document.querySelector("button[aria-label='Send message']");
  const diagnostics = {{ buttonLabels: candidates.filter((el) => el.getClientRects().length > 0).map((el) => `${{el.getAttribute("aria-label") || ""}} ${{el.getAttribute("data-testid") || ""}} ${{el.innerText || ""}}`.trim()).filter(Boolean).slice(0, 40) }};
  if (!send) return {{ status: "missing", assistantCount: assistantRoots.length, assistantLastLength: 0, copyButtons, diagnostics }};
  if (send.disabled || send.getAttribute("aria-disabled") === "true") return {{ status: "disabled", assistantCount: assistantRoots.length, assistantLastLength: 0, copyButtons, diagnostics }};
  const last = assistantRoots.at(-1);
  const lastBody = last?.querySelector?.(".font-claude-response");
  const assistantLastLength = String(lastBody?.innerText || lastBody?.textContent || "").trim().length;
  send.click();
  return {{ status: "sent", assistantCount: assistantRoots.length, assistantLastLength, copyButtons, diagnostics }};
}}"##,
        copy_selector = COPY_ACTION_SELECTOR
    )
}

pub fn build_response_poll_function() -> String {
    format!(
        r##"() => {{
  const visible = (el) => !!el && el.getClientRects().length > 0;
  const copyButtons = Array.from(document.querySelectorAll("{copy_selector}")).filter(visible);
  const nodes = Array.from(document.querySelectorAll("[data-is-streaming]"));
  const last = nodes.at(-1) || null;
  const turn = last?.closest?.("[data-testid*='turn'], article") || last;
  const stop = document.querySelector("button[aria-label='Stop response']");
  const thinking = last?.getAttribute?.("data-is-streaming") === "true" &&
    !!last?.querySelector?.("button[class*='group/status']");
  const streaming = !!stop || last?.getAttribute?.("data-is-streaming") === "true";
  const send = document.querySelector("button[aria-label='Send message']");
  const text = String(last?.querySelector?.(".font-claude-response")?.innerText || last?.querySelector?.(".font-claude-response")?.textContent || "").trim();
  const err = Array.from(document.querySelectorAll("[role='alert'], [data-testid*='error']")).find(visible);
  return {{
    count: nodes.length, length: text.length, text, streaming,
    sendState: !send ? "missing" : send.disabled || send.getAttribute("aria-disabled") === "true" ? "disabled" : "enabled",
    hasStopButton: !!stop, thinking,
    copyButtons: turn ? Array.from(turn.querySelectorAll("{copy_selector}")).filter(visible).length : copyButtons.length,
    error: String(err?.innerText || "").trim().slice(0, 300),
  }};
}}"##,
        copy_selector = COPY_ACTION_SELECTOR
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stable_idle_has_ninety_second_floor() {
        assert_eq!(stable_idle_threshold_ms(1_000), 90_000);
        assert_eq!(stable_idle_threshold_ms(40_000), 120_000);
    }

    #[test]
    fn normalizes_uuid_and_exact_https_claude_url() {
        let id = "123e4567-e89b-12d3-a456-426614174000";
        for raw in [id, &format!("https://claude.ai/chat/{id}")] {
            assert_eq!(normalize_conversation(raw).unwrap().id, id);
        }
        for raw in [
            "http://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000",
            "https://evil.example/chat/123e4567-e89b-12d3-a456-426614174000",
            "https://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000/extra",
            "not-a-uuid",
        ] {
            assert!(normalize_conversation(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn marker_url_and_window_name_are_run_scoped() {
        let run = "20260718T102228Z_ab12cd";
        assert_eq!(
            mark_claude_url(run).unwrap(),
            format!("https://claude.ai/new?_yoetz={run}")
        );
        assert!(build_set_window_name_js(run)
            .unwrap()
            .contains(&format!("window.name = \"yoetz:{run}\"")));
    }

    #[test]
    fn model_scripts_bind_fable_max_hover_fallback_and_exact_postconditions() {
        assert!(build_open_model_menu_function().contains("model-selector-dropdown"));
        assert!(build_select_fable_function().contains("fable 5"));
        assert!(build_mark_effort_parent_function().contains(EFFORT_HOVER_MARKER));
        let hover = build_open_effort_submenu_function();
        for event in [
            "pointerover",
            "pointerenter",
            "pointermove",
            "mouseover",
            "mouseenter",
            "mousemove",
        ] {
            assert!(hover.contains(event));
        }
        assert!(hover.contains("already_open"));
        assert!(hover.contains(":not([data-model-id])"));
        assert!(build_select_max_function().contains("startsWith(\"max\")"));
        let verify = build_verify_fable_max_function();
        assert!(verify.contains("modelVerified && maxVerified"));
        assert!(!verify.contains("thinkingChecked"));
    }

    #[test]
    fn selected_status_requires_fable_and_max_verified_postconditions() {
        let selected = json!({"status":"selected","modelVerified":true,"maxVerified":true});
        assert_eq!(
            model_selection_status(&selected),
            WebModelSelectionStatus::Selected
        );
        for key in ["modelVerified", "maxVerified"] {
            let mut mismatch = selected.clone();
            mismatch[key] = Value::Bool(false);
            assert_eq!(
                model_selection_status(&mismatch),
                WebModelSelectionStatus::Mismatch
            );
        }
        assert_eq!(
            model_selection_status(&json!({"status":"unavailable"})),
            WebModelSelectionStatus::Unavailable
        );
    }

    #[test]
    fn upload_send_and_response_scripts_use_claude_testids() {
        let scope = build_scope_file_input_function();
        assert!(scope.contains("file-upload"));
        assert!(scope.contains("querySelectorAll"));
        assert!(scope.contains("inputs.length !== 1"));
        assert!(scope.contains("count: inputs.length"));
        let attachment = build_attachment_probe_function("bundle.md").unwrap();
        assert!(attachment.contains("file-thumbnail"));
        assert!(attachment.contains("pasted content"));
        assert!(attachment.contains("querySelector(\"[title]\")"));
        assert!(attachment.contains("button[data-cds-attachment-remove]"));
        assert!(attachment.contains("sendEnabled ? \"candidate\""));
        let send = build_send_function();
        assert!(send.contains("assistantCount"));
        assert!(send.contains(".font-claude-response"));
        assert!(send.contains("status: \"sent\""));
        let poll = build_response_poll_function();
        assert!(poll.contains("action-bar-copy"));
        assert!(poll.contains("data-is-streaming"));
        assert!(poll.contains("group/status"));
        assert!(poll.contains("thinking"));
    }
}

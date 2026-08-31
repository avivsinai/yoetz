//! Shared ChatGPT web contracts and DOM-script builders used by browser transports.

use anyhow::{anyhow, Result};
use rand::random;
use std::path::Path;
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

use crate::chatgpt_recipe::{ChatgptModelSelectionStatus, ChatgptModelStrategy};

pub const CHATGPT_URL: &str = "https://chatgpt.com/";
const CHATGPT_LEGACY_HOST: &str = "chat.openai.com";
const CHATGPT_HOST: &str = "chatgpt.com";
pub const COMPOSER_SELECTOR: &str =
    "#prompt-textarea, div[contenteditable='true'][role='textbox'], [role='textbox']";
pub const MODEL_SELECTOR_BUTTON_SELECTOR: &str = "button[aria-haspopup='menu']";
pub const ATTACHMENT_TILE_SELECTOR: &str = "[class*='file-tile'], [data-testid*='attachment']";
pub const ATTACHMENT_TRIGGER_SELECTOR: &str = "button[data-testid='composer-plus-btn'], button[aria-label*='Attach'], button[aria-label*='attach'], button[data-testid*='attach']";
pub const SEND_BUTTON_SELECTOR: &str =
    "[data-testid='send-button'], [data-testid='fruitjuice-send-button'], form button[type='submit']:last-of-type";
pub const STOP_BUTTON_SELECTOR: &str = "[data-testid='stop-button'], button[aria-label*='Stop']";
pub const UPLOAD_MENU_TEXT_PATTERN: &str =
    "upload from computer|from computer|upload files|choose files|browse";
pub const STABLE_IDLE_FLOOR_MS: u64 = 90_000;
pub const STABLE_IDLE_INTERVAL_MULTIPLIER: u64 = 3;
pub(crate) const CHATGPT_UPLOAD_STABLE_POLLS: u64 = 2;
pub(crate) const JS_VISIBILITY_HELPERS: &str = r#"
  const isVisibleWithoutLayout = (el) => {
    if (!el) return false;
    let current = el;
    while (current) {
      if (current.hidden || current.getAttribute("hidden") !== null
        || current.getAttribute("aria-hidden") === "true"
        || current.getAttribute("inert") !== null) return false;
      const currentStyle = window.getComputedStyle(current);
      if (currentStyle.visibility === "hidden"
        || currentStyle.display === "none"
        || currentStyle.opacity === "0"
        || currentStyle.contentVisibility === "hidden") return false;
      current = current.parentElement;
    }
    const style = window.getComputedStyle(el);
    return style.visibility !== "hidden"
      && style.display !== "none"
      && style.pointerEvents !== "none";
  };
  const isVisible = (el) => {
    if (!isVisibleWithoutLayout(el)) return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };
  const findVisible = (root, selector) =>
    Array.from((root || document).querySelectorAll(selector)).find((el) => isVisible(el)) || null;
"#;
pub(crate) const JS_COMPOSER_SCOPE_HELPERS: &str = r#"
  const COMPOSER_ROOT_SELECTOR = "[data-testid*='composer'], [class*='composer'], main, [role='main']";
  const getComposerScope = (composerEl = document.querySelector(COMPOSER_SELECTOR)) => {
    const composerForm = composerEl?.closest("form") || null;
    const composerRoot = composerForm ||
      composerEl?.closest(COMPOSER_ROOT_SELECTOR) ||
      composerEl?.parentElement ||
      null;
    const seenRoots = new Set();
    const roots = [];
    [composerForm, composerRoot, document].forEach((root) => {
      if (root && !seenRoots.has(root)) {
        seenRoots.add(root);
        roots.push(root);
      }
    });
    return { composerEl, composerForm, composerRoot, roots };
  };
"#;
pub(crate) const JS_TURN_ROOT_HELPERS: &str = r#"
  const latestAssistantTurn = (msg) =>
    msg?.closest(".agent-turn, [class*='agent-turn'], [class*='turn-messages']") ||
    msg?.parentElement?.parentElement ||
    msg?.parentElement ||
    null;
"#;
static RUN_ID_TS_FORMAT: &[FormatItem<'static>] =
    format_description!("[year][month][day]T[hour][minute][second]Z");
const AUTH_MARKERS: &[&str] = &[
    "send a message",
    "message chatgpt",
    "new chat",
    "send-button",
    "prompt-textarea",
    "composer",
    "create-new-chat-button",
    "composer-plus-btn",
];
const CHALLENGE_MARKERS: &[&str] = &[
    "cloudflare",
    "checking your browser",
    "attention required",
    "security check",
    "just a moment",
    "verify you are human",
    "cf-chl",
];
const LOGIN_MARKERS: &[&str] = &[
    "log in",
    "login",
    "sign in",
    "sign up",
    "create account",
    "continue with google",
    "continue with microsoft",
    "continue with apple",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatgptConversation {
    pub id: String,
    pub url: String,
}

pub fn stable_idle_threshold_ms(interval_ms: u64) -> u64 {
    interval_ms
        .saturating_mul(STABLE_IDLE_INTERVAL_MULTIPLIER)
        .max(STABLE_IDLE_FLOOR_MS)
}

pub fn generate_run_id() -> String {
    let ts = OffsetDateTime::now_utc()
        .format(RUN_ID_TS_FORMAT)
        .unwrap_or_else(|_| "unknown".to_string());
    let suffix = format!("{:06x}", random::<u32>() & 0x00ff_ffff);
    format!("{ts}_{suffix}")
}

pub fn validate_run_id(run_id: &str) -> Result<()> {
    let valid = !run_id.is_empty()
        && run_id.len() <= 128
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'.' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid `run_id`: expected 1-128 ASCII letters, digits, `_`, `:`, `.`, or `-`"
        ))
    }
}

pub fn validate_thread_mode(raw: Option<&str>) -> Result<()> {
    match raw.unwrap_or("fresh").trim().to_ascii_lowercase().as_str() {
        "" | "fresh" => Ok(()),
        "reuse" => Err(anyhow!(
            "thread=reuse is no longer supported. yoetz always opens a fresh ChatGPT tab for each request and never reuses user tabs — this lets you chat in ChatGPT normally without yoetz interfering. Omit `--var thread=reuse`."
        )),
        other => Err(anyhow!(
            "unsupported `thread` value `{other}`; expected `fresh`"
        )),
    }
}

pub fn normalize_conversation(raw: &str) -> Result<ChatgptConversation> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "invalid `conversation`: expected a ChatGPT conversation id or /c/<id> URL"
        ));
    }
    let id = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        conversation_id_from_url(trimmed)?
    } else {
        trimmed.to_string()
    };
    validate_conversation_id(&id)?;
    Ok(ChatgptConversation {
        url: chatgpt_conversation_url(&id),
        id,
    })
}

pub fn chatgpt_conversation_url(conversation_id: &str) -> String {
    format!(
        "{CHATGPT_URL}c/{}",
        percent_encode_path_segment(conversation_id)
    )
}

fn conversation_id_from_url(raw: &str) -> Result<String> {
    let without_scheme = raw.strip_prefix("https://").ok_or_else(|| {
        anyhow!("invalid `conversation` URL: expected https://chatgpt.com/c/<id>")
    })?;
    let (host, path_and_more) = without_scheme.split_once('/').ok_or_else(|| {
        anyhow!("invalid `conversation` URL: expected https://chatgpt.com/c/<id>")
    })?;
    let host = host.to_ascii_lowercase();
    if host != CHATGPT_HOST && host != CHATGPT_LEGACY_HOST {
        return Err(anyhow!(
            "invalid `conversation` URL host `{host}`; expected chatgpt.com or chat.openai.com"
        ));
    }
    let path = path_and_more.split(['?', '#']).next().unwrap_or_default();
    let id = path
        .strip_prefix("c/")
        .ok_or_else(|| anyhow!("invalid `conversation` URL path: expected /c/<id>"))?;
    if id.contains('/') {
        return Err(anyhow!(
            "invalid `conversation` URL path: expected a single /c/<id> path segment"
        ));
    }
    Ok(id.to_string())
}

fn validate_conversation_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id != "."
        && id != ".."
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid `conversation`: expected 1-256 ASCII letters, digits, `_`, `-`, or `.`"
        ))
    }
}

pub fn mark_chatgpt_url(run_id: &str) -> String {
    format!("{CHATGPT_URL}?{}", chatgpt_run_url_marker(run_id))
}

pub(crate) fn chatgpt_run_url_marker(run_id: &str) -> String {
    format!("_yoetz={}", percent_encode_query_component(run_id))
}

fn percent_encode_query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub fn build_set_window_name_js(run_id: &str) -> String {
    let window_name =
        serde_json::to_string(&format!("yoetz:{run_id}")).expect("serialize window.name value");
    format!(
        r#"() => {{
  window.name = {window_name};
  return window.name;
}}"#
    )
}

pub fn select_reported_chatgpt_model(
    selection: &serde_json::Value,
    requested_model: &str,
) -> Option<String> {
    if is_current_model_selection(selection, requested_model)
        || is_verified_sol_chat_pro_selection(selection, requested_model)
    {
        return selection
            .get("modelUsed")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
    }
    None
}

pub(crate) fn chatgpt_model_selection_status(
    selection: &serde_json::Value,
    requested_model: &str,
) -> ChatgptModelSelectionStatus {
    let status = selection
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    match status {
        "selected" if is_verified_sol_chat_pro_selection(selection, requested_model) => {
            ChatgptModelSelectionStatus::Selected
        }
        "selected" | "selection-mismatch" => ChatgptModelSelectionStatus::Mismatch,
        "current" if is_current_model_selection(selection, requested_model) => {
            ChatgptModelSelectionStatus::Current
        }
        "current" => ChatgptModelSelectionStatus::Mismatch,
        "missing-selector" | "not-found" => ChatgptModelSelectionStatus::Unavailable,
        _ => ChatgptModelSelectionStatus::Unavailable,
    }
}

fn is_current_model_selection(selection: &serde_json::Value, requested_model: &str) -> bool {
    selection.get("status").and_then(serde_json::Value::as_str) == Some("current")
        && requested_model.trim() == "current"
        && selection
            .get("requested")
            .and_then(serde_json::Value::as_str)
            == Some("current")
        && selection
            .get("familyStatus")
            .and_then(serde_json::Value::as_str)
            == Some("skipped")
        && selection
            .get("effortStatus")
            .and_then(serde_json::Value::as_str)
            == Some("skipped")
}

fn is_verified_sol_chat_pro_selection(
    selection: &serde_json::Value,
    requested_model: &str,
) -> bool {
    if selection.get("status").and_then(serde_json::Value::as_str) != Some("selected")
        || requested_model.trim() != crate::chatgpt_recipe::CHATGPT_SOL_CHAT_PRO_MODEL
        || selection
            .get("requested")
            .and_then(serde_json::Value::as_str)
            != Some(crate::chatgpt_recipe::CHATGPT_SOL_CHAT_PRO_MODEL)
    {
        return false;
    }
    if selection
        .get("familyStatus")
        .and_then(serde_json::Value::as_str)
        != Some("verified")
        || selection
            .get("effortStatus")
            .and_then(serde_json::Value::as_str)
            != Some("verified")
    {
        return false;
    }
    let model_used = selection
        .get("modelUsed")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    matches!(
        model_used
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
            .as_str(),
        "gpt-5.6 sol pro"
    )
}

pub fn model_selector_button_selector_json() -> String {
    serde_json::to_string(MODEL_SELECTOR_BUTTON_SELECTOR)
        .expect("serialize model selector selector")
}

pub fn composer_selector_json() -> String {
    serde_json::to_string(COMPOSER_SELECTOR).expect("serialize composer selector")
}

pub fn attachment_tile_selector_json() -> String {
    serde_json::to_string(ATTACHMENT_TILE_SELECTOR).expect("serialize attachment tile selector")
}

pub fn attachment_trigger_selector_json() -> String {
    serde_json::to_string(ATTACHMENT_TRIGGER_SELECTOR)
        .expect("serialize attachment trigger selector")
}

pub fn send_button_selector_json() -> String {
    serde_json::to_string(SEND_BUTTON_SELECTOR).expect("serialize send button selector")
}

pub fn stop_button_selector_json() -> String {
    serde_json::to_string(STOP_BUTTON_SELECTOR).expect("serialize stop button selector")
}

pub fn upload_menu_text_pattern_json() -> String {
    serde_json::to_string(UPLOAD_MENU_TEXT_PATTERN).expect("serialize upload menu text pattern")
}

pub fn build_model_selection_function(
    requested_model: &str,
    model_strategy: ChatgptModelStrategy,
) -> String {
    build_model_selection_function_with_surface_evidence(requested_model, model_strategy, false)
}

pub fn build_model_selection_function_with_surface_evidence(
    requested_model: &str,
    model_strategy: ChatgptModelStrategy,
    prior_surface_evidence_seen: bool,
) -> String {
    let requested_model =
        serde_json::to_string(requested_model).expect("serialize requested model");
    let model_strategy = serde_json::to_string(&model_strategy).expect("serialize model strategy");
    let model_button_selector = model_selector_button_selector_json();
    let composer_selector = composer_selector_json();
    format!(
        r##"
async () => {{
  const requested = {requested_model};
  const strategy = {model_strategy};
  const supported = "gpt-5-6-sol-chat-pro";
  const MODEL_BUTTON_SELECTOR = {model_button_selector};
  const COMPOSER_SELECTOR = {composer_selector};
  const SURFACE_SETTLE_TIMEOUT_MS = 2000;
  const SURFACE_SETTLE_INTERVAL_MS = 50;
  const MIN_IMPLICIT_SURFACE_ABSENCE_MS = 1500;
  const SURFACE_REQUIRED_STABLE_OBSERVATIONS = 2;
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const fold = (value) => normalize(value).toLowerCase();
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const textOf = (node) => normalize(node?.innerText || node?.textContent || "");
{visibility_helpers}

  function surfaceControls() {{
    const candidates = [];
    for (const group of Array.from(document.querySelectorAll('[role="radiogroup"][aria-label="Select chat surface"]'))) {{
      if (!isVisibleWithoutLayout(group)) continue;
      const chats = Array.from(group.querySelectorAll('[role="radio"][data-tpp-toggle-value="chatgpt"]'))
        .filter((node) => isVisible(node));
      const works = Array.from(group.querySelectorAll('[role="radio"][data-tpp-toggle-value="work"]'))
        .filter((node) => isVisible(node));
      if (chats.length !== 1 || works.length !== 1) return null;
      candidates.push({{ group, chat: chats[0], work: works[0] }});
    }}
    return candidates.length === 1 ? candidates[0] : null;
  }}

  function surfaceState(node) {{
    return {{
      ariaChecked: node?.getAttribute("aria-checked") || null,
      dataState: node?.getAttribute("data-state") || null
    }};
  }}

  function surfaceSelectionIsChat(controls) {{
    const chat = surfaceState(controls?.chat);
    const work = surfaceState(controls?.work);
    return chat.ariaChecked === "true" && work.ariaChecked === "false";
  }}

  function surfaceObservedToggleNodes(group = null) {{
    const scope = group || document;
    return Array.from(scope.querySelectorAll?.('[role="radio"][data-tpp-toggle-value]') || [])
      .filter((node) => isVisible(node));
  }}

  function surfaceObservedValues(group = null) {{
    return surfaceObservedToggleNodes(group)
      .map((node) => node.getAttribute("data-tpp-toggle-value"))
      .filter(Boolean)
      .filter((value, index, values) => values.indexOf(value) === index)
      .slice(0, 10);
  }}

  function implicitChatSurfaceProof(observedValues) {{
    if (observedValues.length > 0
      || surfaceEvidencePresent()) return false;
    const composer = findVisible(document, COMPOSER_SELECTOR);
    return composer?.getAttribute("aria-label") === "Chat with ChatGPT";
  }}

  function surfaceEvidencePresent() {{
    return document.querySelector('[role="radiogroup"][aria-label="Select chat surface"]') !== null
      || document.querySelector('[role="radio"][data-tpp-toggle-value]') !== null;
  }}

  async function ensureChatSurface() {{
    const startedAt = Date.now();
    let attempts = 0;
    let state = surfaceState(null);
    let observedValues = [];
    let clicked = false;
    let stableProof = null;
    let stableObservations = 0;
    let lastComposerAria = null;
    while (Date.now() - startedAt < SURFACE_SETTLE_TIMEOUT_MS) {{
      attempts += 1;
      const controls = surfaceControls();
      state = surfaceState(controls?.chat);
      observedValues = surfaceObservedValues();
      const visibleSurfaceToggleCount = surfaceObservedToggleNodes().length;
      surfaceEvidenceSeen = surfaceEvidenceSeen || surfaceEvidencePresent();
      const composer = findVisible(document, COMPOSER_SELECTOR);
      lastComposerAria = composer?.getAttribute("aria-label") || null;
      const implicitProofReady = Date.now() - startedAt >= MIN_IMPLICIT_SURFACE_ABSENCE_MS;
      const proof = controls && surfaceSelectionIsChat(controls)
        && visibleSurfaceToggleCount === 2
        ? "controls"
        : !controls && !surfaceEvidenceSeen && implicitProofReady && implicitChatSurfaceProof(observedValues)
          ? "composer_aria"
          : null;
      if (proof === stableProof) {{
        stableObservations += 1;
      }} else {{
        stableProof = proof;
        stableObservations = proof ? 1 : 0;
      }}
      if (proof && stableObservations >= SURFACE_REQUIRED_STABLE_OBSERVATIONS) {{
        return {{
          ok: true,
          elapsedMs: Date.now() - startedAt,
          attempts,
          verificationAttempts: Math.max(0, attempts - 1),
          state,
          observedValues,
          surfaceProofKind: proof === "controls"
            ? "explicit_chat_work_radios"
            : "implicit_chat_composer_aria",
          surfaceChatState: surfaceState(controls?.chat),
          surfaceWorkState: surfaceState(controls?.work),
          surfaceVisibleToggleCount,
          surfaceComposerAria: proof === "composer_aria"
            ? composer?.getAttribute("aria-label") || null
            : null,
          surfaceEvidenceSeen
        }};
      }}
      if (controls && !surfaceSelectionIsChat(controls) && !clicked) {{
        realClick(controls.chat);
        clicked = true;
        stableProof = null;
        stableObservations = 0;
      }}
      const remainingMs = SURFACE_SETTLE_TIMEOUT_MS - (Date.now() - startedAt);
      if (remainingMs <= 0) break;
      await wait(Math.min(SURFACE_SETTLE_INTERVAL_MS, remainingMs));
    }}
    return {{
      ok: false,
      warning: clicked
        ? "ChatGPT Chat surface could not be verified after selection"
        : "ChatGPT Chat surface toggle not found or could not be read",
      failureReason: clicked ? "chat_surface_selection_mismatch" : "chat_surface_control_not_found",
      elapsedMs: Date.now() - startedAt,
      attempts,
      verificationAttempts: Math.max(0, attempts - 1),
      state,
      observedValues,
      surfaceProofKind: null,
      surfaceChatState: surfaceState(null),
      surfaceWorkState: surfaceState(null),
      surfaceVisibleToggleCount: surfaceObservedToggleNodes().length,
      surfaceComposerAria: lastComposerAria,
      surfaceEvidenceSeen
    }};
  }}

  function classTokens(node) {{
    return normalize(node?.getAttribute?.("class") || "").split(" ").filter(Boolean);
  }}

  function legacyPickerMarkers() {{
    return Array.from(document.querySelectorAll(
      "[data-testid='model-switcher-dropdown-button'], [data-testid='model-switcher-selected-model'], [data-testid^='model-switcher-']"
    )).filter(isVisible).map((node) => node.getAttribute("data-testid") || "").filter(Boolean);
  }}

  function composerScopes() {{
    const composer = document.querySelector(COMPOSER_SELECTOR);
    const scopes = [];
    const add = (scope) => {{ if (scope && !scopes.includes(scope)) scopes.push(scope); }};
    add(composer?.closest("form"));
    add(composer?.closest("[data-testid*='composer'], [class*='composer']"));
    add(composer?.parentElement);
    const local = scopes.slice();
    for (const scope of local) {{
      const parent = scope?.parentElement;
      if (!parent) continue;
      for (const sibling of Array.from(parent.children || [])) {{
        const marker = fold([
          sibling.getAttribute?.("data-testid"),
          sibling.getAttribute?.("class"),
          sibling.getAttribute?.("aria-label")
        ].filter(Boolean).join(" "));
        if (sibling !== scope && /\b(composer|model|toolbar|controls|pill)\b/.test(marker)) add(sibling);
      }}
    }}
    return scopes;
  }}

  function pillLabel(node) {{
    return normalize([textOf(node), node?.getAttribute?.("aria-label"), node?.getAttribute?.("title")].filter(Boolean).join(" "));
  }}

  function summaryMatches(value) {{
    const valueFolded = fold(value);
    const effort = "instant|medium|high|extra high|pro|max|light";
    return new RegExp(`^(?:${{effort}})$`).test(valueFolded)
      || new RegExp(`^\\d+(?:\\.\\d+)+(?: sol)? (?:${{effort}})$`).test(valueFolded)
      || /\bgpt[\s.-]*\d/.test(valueFolded);
  }}

  function findPill() {{
    for (const scope of composerScopes()) {{
      const buttons = Array.from(scope.querySelectorAll(MODEL_BUTTON_SELECTOR)).filter(isVisible);
      const exact = buttons.find((button) => button.classList.contains("__composer-pill") && summaryMatches(pillLabel(button)));
      if (exact) return exact;
      const fallback = buttons.find((button) => summaryMatches(pillLabel(button)));
      if (fallback) return fallback;
      const familyToken = buttons.find((button) => button.classList.contains("__composer-pill") && pillHasModelFamilyToken(pillLabel(button)));
      if (familyToken) return familyToken;
      const anyPill = buttons.find((button) => button.classList.contains("__composer-pill"));
      if (anyPill) return anyPill;
    }}
    return null;
  }}

  async function waitForPill() {{
    let pill = findPill();
    for (let attempt = 0; attempt < 20 && !pill; attempt += 1) {{
      await wait(250);
      pill = findPill();
    }}
    return pill;
  }}

  function visibleMenus() {{
    return Array.from(document.querySelectorAll("[role='menu']")).filter((menu) => isVisible(menu) && pickerSurfaceIsOpen(menu));
  }}

  function pickerSurfaceIsOpen(node) {{
    let current = node;
    while (current) {{
      const state = current.getAttribute?.("data-state");
      if (state === "closed") return false;
      current = current.parentElement;
    }}
    return true;
  }}

  function radios(menu) {{
    return Array.from(menu?.querySelectorAll?.("[role='menuitemradio']") || []).filter(isVisible);
  }}

  function isChecked(item) {{
    return item?.getAttribute?.("aria-checked") === "true" || item?.getAttribute?.("data-state") === "checked";
  }}

  function mainMenu() {{
    return visibleMenus().find((menu) => {{
      const labels = radios(menu).map((item) => fold(textOf(item)));
      return labels.includes("medium") && labels.includes("high")
        && (labels.includes("pro"));
    }}) || null;
  }}

  function sliderSnapshot(slider, surface) {{
    const valueText = normalize(slider?.getAttribute("aria-valuetext") || "");
    let nearby = valueText;
    if (!nearby) {{
      let scope = slider?.parentElement;
      for (let depth = 0; scope && depth < 8; depth += 1, scope = scope.parentElement) {{
        const label = Array.from(scope.querySelectorAll("span, div") || [])
          .map((node) => normalize(textOf(node)))
          .find((text) => /^[A-Z][A-Za-z ]{{1,24}},\s*\d+\s+of\s+\d+\s*\.?$/.test(text));
        if (label) {{ nearby = label; break; }}
        if (scope === surface) break;
      }}
    }}
    const match = nearby.match(/^([A-Z][A-Za-z ]{{1,24}}),\s*(\d+)\s+of\s+(\d+)\s*\.?\s*$/);
    const now = Number(slider?.getAttribute("aria-valuenow"));
    const min = Number(slider?.getAttribute("aria-valuemin"));
    const max = Number(slider?.getAttribute("aria-valuemax"));
    const ordinal = now - min + 1;
    const total = max - min + 1;
    if (!match || !Number.isFinite(now) || !Number.isFinite(min) || !Number.isFinite(max)
      || max <= min || Number(match[2]) !== ordinal || Number(match[3]) !== total) return null;
    return {{ label: fold(match[1]), display: match[1], now, min, max }};
  }}

  function looksLikePersonal(menu) {{
    const text = textOf(menu);
    return /\bFaster\b/i.test(text) && /\bSmarter\b/i.test(text) && /\bModel\b/i.test(text)
      && /\bEffort\b/i.test(text) && /\bSpeed\b/i.test(text) && !/\bAdvanced\b/i.test(text);
  }}

  function hybridMenu() {{
    return visibleMenus().find((menu) => {{
      if (looksLikePersonal(menu)) return false;
      return Array.from(menu.querySelectorAll("[role='slider']") || []).some((slider) => sliderSnapshot(slider, menu));
    }}) || null;
  }}

  function selectModelViewToggle(menu) {{
    return Array.from(menu?.querySelectorAll?.("[role='menuitem']") || [])
      .find((item) => normalize(item.getAttribute("aria-label") || "").toLowerCase() === "select model") || null;
  }}

  async function activateHybridFamilyView(menu) {{
    if (!menu) return null;
    if (radios(menu).length > 0) return menu;
    const toggle = selectModelViewToggle(menu);
    if (!toggle) return null;
    realClick(toggle);
    for (let attempt = 0; attempt < 30; attempt += 1) {{
      const active = hybridMenu();
      if (active && radios(active).length > 0) return active;
      await wait(100);
    }}
    return null;
  }}

  function composerMenuTriggers() {{
    const triggers = [];
    for (const scope of composerScopes()) {{
      for (const node of Array.from(scope.querySelectorAll(MODEL_BUTTON_SELECTOR))) triggers.push(node);
    }}
    return triggers;
  }}

  function leftoverTriggers() {{
    return composerMenuTriggers().filter((trigger) =>
      trigger.getAttribute("aria-expanded") === "true" || trigger.getAttribute("data-state") === "open");
  }}

  function familyMenu(main) {{
    return visibleMenus().find((menu) => menu !== main
      && radios(menu).some((item) => fold(textOf(item)) === "gpt-5.6 sol")) || null;
  }}

  function readState(menu) {{
    const effortItems = radios(menu);
    const familyTrigger = Array.from(menu?.querySelectorAll?.("[role='menuitem']") || [])
      .find((item) => item.getAttribute("aria-haspopup") === "menu"
        && (/^(?:gpt|o\d)\b/i.test(textOf(item)) || /\bModel\b/i.test(textOf(item)))) || null;
    return {{ shape: "menu", menu, effortItems, familyTrigger, familyLabel: textOf(familyTrigger), familyProof: false }};
  }}

  function dispatch(element, type, kind, init = {{}}) {{
    const Constructor = window[kind] || Event;
    element.dispatchEvent(new Constructor(type, {{ bubbles: true, cancelable: true, composed: true, ...init }}));
  }}

  async function pointerOpen(element, mode, main) {{
    element?.focus?.();
    const phases = [
      ["pointerdown", "PointerEvent", {{ button: 0, buttons: 1, pointerId: 1, pointerType: "mouse", isPrimary: true }}],
      ["mousedown", "MouseEvent", {{ button: 0, buttons: 1 }}],
      ["pointerup", "PointerEvent", {{ button: 0, buttons: 0, pointerId: 1, pointerType: "mouse", isPrimary: true }}],
      ["mouseup", "MouseEvent", {{ button: 0, buttons: 0 }}],
      ["click", "MouseEvent", {{ button: 0, buttons: 0, detail: 1 }}]
    ];
    for (const phase of phases) {{
      dispatch(element, phase[0], phase[1], phase[2]);
      await wait(125);
      if (mode === "main" ? mainMenu() : mode === "hybrid" ? hybridMenu() : mode === "personal" ? personalMenu() : familyMenu(main)) return true;
    }}
    return false;
  }}

  function realClick(element) {{
    element?.focus?.();
    dispatch(element, "pointerdown", "PointerEvent", {{ button: 0, buttons: 1, pointerId: 1, pointerType: "mouse", isPrimary: true }});
    dispatch(element, "mousedown", "MouseEvent", {{ button: 0, buttons: 1 }});
    dispatch(element, "pointerup", "PointerEvent", {{ button: 0, buttons: 0, pointerId: 1, pointerType: "mouse", isPrimary: true }});
    dispatch(element, "mouseup", "MouseEvent", {{ button: 0, buttons: 0 }});
    dispatch(element, "click", "MouseEvent", {{ button: 0, buttons: 0, detail: 1 }});
  }}

  function keyPress(element, key, code) {{
    dispatch(element, "keydown", "KeyboardEvent", {{ key, code }});
    dispatch(element, "keyup", "KeyboardEvent", {{ key, code }});
  }}

  async function waitForMain() {{
    for (let attempt = 0; attempt < 30; attempt += 1) {{
      const menu = mainMenu();
      if (menu) return menu;
      await wait(100);
    }}
    return null;
  }}

  async function openMain(pill) {{
    const existing = mainMenu();
    if (existing) return existing;
    await pointerOpen(pill, "main", null);
    let menu = await waitForMain();
    if (menu) return menu;
    keyPress(pill, "Enter", "Enter");
    return waitForMain();
  }}

  async function openHybrid(pill) {{
    const existing = hybridMenu();
    if (existing) return activateHybridFamilyView(existing);
    await pointerOpen(pill, "hybrid", null);
    for (let attempt = 0; attempt < 30; attempt += 1) {{
      const opened = hybridMenu();
      if (opened) return activateHybridFamilyView(opened);
      await wait(100);
    }}
    keyPress(pill, "Enter", "Enter");
    for (let attempt = 0; attempt < 30; attempt += 1) {{
      const opened = hybridMenu();
      if (opened) return activateHybridFamilyView(opened);
      await wait(100);
    }}
    return null;
  }}

  function personalMenu() {{
    return Array.from(document.querySelectorAll("[role='dialog'], [role='menu'], [data-testid='composer-intelligence-picker-content']"))
      .filter((node) => isVisible(node) && pickerSurfaceIsOpen(node) && looksLikePersonal(node))
      .sort((left, right) => (left.querySelectorAll("*").length || 0) - (right.querySelectorAll("*").length || 0))[0] || null;
  }}

  async function openPersonal(pill) {{
    const existing = personalMenu();
    if (existing) return existing;
    await pointerOpen(pill, "personal", null);
    for (let attempt = 0; attempt < 30; attempt += 1) {{
      const opened = personalMenu();
      if (opened) return opened;
      await wait(100);
    }}
    keyPress(pill, "Enter", "Enter");
    for (let attempt = 0; attempt < 30; attempt += 1) {{
      const opened = personalMenu();
      if (opened) return opened;
      await wait(100);
    }}
    return null;
  }}

  function labeledRowValue(row, label) {{
    const text = normalize(textOf(row)).replace(/\s+/g, " ");
    const pattern = label === "Model" ? /^Model\s+(.+)$/i : /^Effort\s+(.+)$/i;
    return normalize(text.match(pattern)?.[1] || "");
  }}

  function structuralFamilyEvidence(control) {{
    const empty = {{ label: "", labels: [], source: null, ambiguous: false }};
    if (!control || control.getAttribute("aria-haspopup") !== "menu" || !/\bModel\b/i.test(textOf(control))) return empty;
    const matches = Array.from(control.querySelectorAll("*") || [])
      .map((node) => ({{ node, label: normalize(textOf(node)) }}))
      .filter((match) => /^(?:gpt|o\d)\b/i.test(match.label));
    const labels = [];
    for (const match of matches) {{
      if (!labels.some((label) => fold(label) === fold(match.label))) labels.push(match.label);
    }}
    if (labels.length > 1) return {{ label: "", labels, source: null, ambiguous: true }};
    if (matches.length === 0) return empty;
    const explicit = matches.find((match) => {{
      const current = normalize(match.node.getAttribute("aria-current") || "").toLowerCase();
      return (current && current !== "false") || match.node.getAttribute("data-state") === "checked";
    }});
    const selected = explicit || matches.reduce((deepest, match) => match.node.parentElement?.contains(deepest.node) ? deepest : match);
    return {{
      label: selected.label,
      labels,
      source: explicit ? "explicit" : "deepest_unique",
      ambiguous: false
    }};
  }}

  function readPersonalState(menu) {{
    const controls = Array.from(menu?.querySelectorAll?.("[role='menuitem'], button") || []);
    let familyEvidence = null;
    const familyTrigger = controls.find((item) => {{
      const evidence = structuralFamilyEvidence(item);
      if (evidence.label || evidence.ambiguous) familyEvidence = evidence;
      return Boolean(evidence.label || evidence.ambiguous);
    }}) || controls.find((item) => item.getAttribute("aria-haspopup") === "menu"
      && (/\bModel\b/i.test(textOf(item)) || /^(?:gpt|o\d)\b/i.test(textOf(item)))) || null;
    const effortRow = controls.find((item) => /\bEffort\b/i.test(textOf(item))) || null;
    const familyLabel = familyEvidence?.label || labeledRowValue(familyTrigger, "Model");
    const effortLabel = labeledRowValue(effortRow, "Effort");
    if (!familyTrigger || !effortRow || !effortLabel || (!familyLabel && !familyEvidence?.ambiguous)) return null;
    return {{
      shape: "personal",
      menu,
      effortItems: [],
      effortRow,
      effortLabel,
      verifiedEffortDisplay: effortLabel,
      familyTrigger,
      familyLabel,
      familyLabelCandidates: familyEvidence?.labels || (familyLabel ? [familyLabel] : []),
      familyLabelSource: familyEvidence?.source || (familyLabel ? "labeled_row" : null),
      familyLabelAmbiguous: familyEvidence?.ambiguous || false,
      familyProof: !familyEvidence?.ambiguous && foldFamilyLabel(familyLabel) === "5.6 sol"
    }};
  }}

  async function readPersonalFamilyProof(state) {{
    if (!state) return {{ ok: false, checkedItems: [], families: [], sol: null }};
    if (!state.familyTrigger || state.familyLabelAmbiguous) {{
      state.familyProof = !state.familyLabelAmbiguous && foldFamilyLabel(state.familyLabel) === "5.6 sol";
      return {{
        ok: state.familyProof,
        checkedItems: [],
        families: state.familyLabel ? [state.familyLabel] : [],
        sol: null
      }};
    }}
    return readFamilyProof(state);
  }}

  function personalEffortOption(state) {{
    const personal = state?.menu || personalMenu();
    const candidates = Array.from(document.querySelectorAll("[role='menu'], [role='dialog']"))
      .filter((surface) => surface !== personal
        && pickerSurfaceIsOpen(surface)
        && isVisible(surface)
        && !looksLikePersonal(surface));
    const matches = candidates.map((surface) => ({{
      surface,
      options: Array.from(surface.querySelectorAll("[role='menuitemradio'], [role='menuitem']"))
        .filter((item) => isVisible(item) && fold(textOf(item)) === "pro")
    }})).filter((entry) => entry.options.length === 1);
    return matches.length === 1 ? matches[0].options[0] : null;
  }}

  async function waitForPersonalEffortOption(state) {{
    for (let attempt = 0; attempt < 30; attempt += 1) {{
      const option = personalEffortOption(state);
      if (option) return option;
      await wait(100);
    }}
    return null;
  }}

  async function selectPersonal(pill, state) {{
    let familyProof = await readPersonalFamilyProof(state);
    if (!familyProof.ok) {{
      if (familyProof.checkedItems.length !== 1 || !familyProof.sol) {{
        await closeMenus(pill);
        return result("not-found", pill, state, familyProof.families, "GPT-5.6 Sol was not visible in the personal picker");
      }}
      realClick(familyProof.sol);
      await wait(250);
      pill = await waitForPill();
      state = await openPersonal(pill);
      if (!state) return result("selection-mismatch", pill, null, familyProof.families, "personal picker did not reopen after selecting GPT-5.6 Sol");
      familyProof = await readPersonalFamilyProof(state);
      if (!familyProof.ok) {{
        await closeMenus(pill);
        return result("selection-mismatch", pill, state, familyProof.families, "GPT-5.6 Sol personal family selection could not be verified");
      }}
    }}
    if (state.familyTrigger) {{
      if (!await closeMenus(pill)) return result("selection-mismatch", pill, state, familyProof.families, "ChatGPT personal family picker could not close before effort selection");
      pill = await waitForPill();
      state = readPersonalState(await openPersonal(pill));
      if (!state) return result("selection-mismatch", pill, null, familyProof.families, "ChatGPT personal picker did not reopen before effort selection");
    }}
    if (!effortVerified(state)) {{
      realClick(state.effortRow);
      await wait(250);
      const proOption = await waitForPersonalEffortOption(state);
      if (!proOption) {{
        await closeMenus(pill);
        return result("not-found", pill, state, familyProof.families, "Pro was not visible as a GPT-5.6 Sol effort tier");
      }}
      realClick(proOption);
      await wait(250);
      pill = await waitForPill();
      state = readPersonalState(await openPersonal(pill));
      if (!state) return result("selection-mismatch", pill, null, familyProof.families, "personal picker did not reopen after selecting Pro effort");
    }}
    familyProof = await readPersonalFamilyProof(state);
    if (!familyProof.ok || !effortVerified(state)) {{
      await closeMenus(pill);
      return result("selection-mismatch", pill, state, familyProof.families, "GPT-5.6 Sol at verified Pro effort could not be confirmed in the personal picker");
    }}
    state.familyLabel = familyProof.checkedItems.length === 1 ? textOf(familyProof.checkedItems[0]) : state.familyLabel;
    state.familyProof = true;
    state.verifiedEffortDisplay = "Pro";
    if (!await closeMenus(pill, state, {{ requireProPill: true }})) {{
      return result("selection-mismatch", pill, state, familyProof.families, "ChatGPT personal picker or closed composer model pill failed verification");
    }}
    pill = await waitForPill();
    const closedPill = closedPillDiagnostics(pill, state);
    if (closedPill.closedPillFamilyStatus === "unverified") {{
      return result("selection-mismatch", pill, state, familyProof.families, "ChatGPT composer model pill reported another model family after closing the personal picker");
    }}
    if (closedPill.closedPillEffortStatus !== "verified") {{
      return result("selection-mismatch", pill, state, familyProof.families, "ChatGPT composer model pill did not confirm verified Pro effort");
    }}
    if (closedPill.closedPillFamilyStatus === "skipped") {{
      const postClose = await reverifyAfterClose(pill, state);
      if (!postClose.ok) {{
        return result("selection-mismatch", pill, state, familyProof.families, "ChatGPT model family was not independently re-read after the closed composer pill omitted it");
      }}
    }}
    const selected = result("selected", pill, state, familyProof.families);
    if (selected.status === "selected") selected.modelUsed = "GPT-5.6 Sol Pro";
    selected.pickerShape = "personal";
    return selected;
  }}

  async function selectHybrid(pill, menu) {{
    menu = await activateHybridFamilyView(menu);
    if (!menu) return result("not-found", pill, null, [], "ChatGPT model picker did not expose an active model selection view");
    const items = radios(menu);
    const families = items.map(textOf).filter(Boolean);
    const sol = items.find((item) => fold(textOf(item)) === "gpt-5.6 sol");
    const state = {{ menu, effortItems: [], familyTrigger: null, familyLabel: sol ? textOf(sol) : "", familyProof: Boolean(sol && isChecked(sol)), shape: "slider" }};
    if (!sol) {{
      await closeMenus(pill);
      return result("not-found", pill, state, families, "GPT-5.6 Sol was not visible in the family menu");
    }}
    if (!isChecked(sol)) {{
      realClick(sol);
      await wait(250);
      pill = await waitForPill();
      menu = await openHybrid(pill);
      if (!menu) return result("selection-mismatch", pill, state, families, "GPT-5.6 Sol family menu selection could not be verified");
    }}
    const reread = radios(menu || hybridMenu());
    const checked = reread.filter(isChecked);
    const solLabel = checked.length === 1 ? textOf(checked[0]) : "";
    if (checked.length !== 1 || fold(solLabel) !== "gpt-5.6 sol") {{
      await closeMenus(pill);
      return result("selection-mismatch", pill, state, families, "GPT-5.6 Sol family menu selection could not be verified");
    }}
    let liveSlider = Array.from((menu || hybridMenu())?.querySelectorAll("[role='slider']") || []).find((node) => sliderSnapshot(node, menu));
    let liveSnap = sliderSnapshot(liveSlider, menu);
    if (!liveSnap || liveSnap.label !== "pro") {{
      if (liveSlider) keyPress(liveSlider, "End", "End");
      await wait(250);
      pill = await waitForPill();
      menu = await openHybrid(pill);
      liveSlider = Array.from((menu || {{}}).querySelectorAll?.("[role='slider']") || []).find((node) => sliderSnapshot(node, menu));
      liveSnap = sliderSnapshot(liveSlider, menu);
      if (!liveSnap || liveSnap.label !== "pro") {{
        await closeMenus(pill);
        return result("selection-mismatch", pill, state, families, "GPT-5.6 Sol effort slider did not move to verified Pro");
      }}
    }}
    state.familyLabel = solLabel;
    state.familyProof = true;
    state.verifiedEffortDisplay = liveSnap.display || "Pro";
    if (!await closeMenus(pill, state)) {{
      return result("selection-mismatch", pill, state, families, "ChatGPT model picker remained open after verification");
    }}
    pill = await waitForPill();
    const closedPill = closedPillDiagnostics(pill, state);
    if (closedPill.closedPillFamilyStatus === "unverified") {{
      return result("selection-mismatch", pill, state, families, "ChatGPT composer model pill reported another model family after closing the picker");
    }}
    if (closedPill.closedPillEffortStatus !== "verified") {{
      return result("selection-mismatch", pill, state, families, "ChatGPT composer model pill did not confirm verified Pro effort");
    }}
    if (closedPill.closedPillFamilyStatus === "skipped") {{
      const postClose = await reverifyAfterClose(pill, state);
      if (!postClose.ok) {{
        return result("selection-mismatch", pill, state, families, "ChatGPT model family was not independently re-read after the closed composer pill omitted it");
      }}
    }}
    const selected = result("selected", pill, state, reread.map(textOf).filter(Boolean) || families);
    if (selected.status === "selected") selected.modelUsed = solLabel + " " + (liveSnap.display || "Pro");
    selected.pickerShape = "slider";
    return selected;
  }}

  async function openFamilyMenu(state) {{
    if (!state?.familyTrigger) return null;
    const hover = [
      ["pointerenter", "PointerEvent", {{ pointerId: 1, pointerType: "mouse", isPrimary: true }}],
      ["mouseenter", "MouseEvent", {{}}],
      ["pointermove", "PointerEvent", {{ pointerId: 1, pointerType: "mouse", isPrimary: true }}],
      ["mousemove", "MouseEvent", {{}}]
    ];
    for (const phase of hover) {{
      dispatch(state.familyTrigger, phase[0], phase[1], phase[2]);
      await wait(125);
      const opened = familyMenu(state.menu);
      if (opened) return opened;
    }}
    await pointerOpen(state.familyTrigger, "family", state.menu);
    for (let attempt = 0; attempt < 20; attempt += 1) {{
      const opened = familyMenu(state.menu);
      if (opened) return opened;
      await wait(100);
    }}
    return null;
  }}

  function mounted(node) {{
    return Boolean(node && (node.isConnected || document.documentElement?.contains?.(node)));
  }}

  function pickerDialog() {{
    return Array.from(document.querySelectorAll("[role='dialog'], [data-testid='composer-intelligence-picker-content']"))
      .filter((node) => isVisible(node) && pickerSurfaceIsOpen(node))
      .find((node) => /\bModel\b/i.test(textOf(node)) && /\bEffort\b/i.test(textOf(node))) || null;
  }}

  function readPickerCloseVerification(pill, state, options = {{}}) {{
    const familyTrigger = state?.familyTrigger;
    const familyOpen = mounted(familyTrigger)
      && (familyTrigger.getAttribute("aria-expanded") === "true"
        || familyTrigger.getAttribute("data-state") === "open");
    const familySurface = familyMenu(state?.menu);
    const leftoverOpen = leftoverTriggers().length > 0;
    const pickerOpen = Boolean(mainMenu())
      || Boolean(familySurface)
      || Boolean(pickerDialog())
      || familyOpen
      || leftoverOpen
      || (mounted(pill) && (pill.getAttribute("aria-expanded") === "true"
        || pill.getAttribute("data-state") === "open"));
    const pillText = pill ? textOf(pill) : "";
    const closedPillPro = options.requireProPill !== true || pillConfirmsEffortLabel(pillText, "Pro");
    return {{
      familyTriggerClosed: !familyOpen,
      pickerSurfaceClosed: !pickerOpen,
      modelTriggerClosed: !pickerOpen,
      closedPillPro,
      closedPillText: pillText || null,
      ok: !familyOpen && !pickerOpen
    }};
  }}

  function neutralComposerArea(pill) {{
    const composer = document.querySelector(COMPOSER_SELECTOR);
    if (composer && composer !== pill) return composer;
    return composerScopes()[0] || null;
  }}

  function dispatchHoverLeaveEvents(element, relatedTarget) {{
    const rect = relatedTarget?.getBoundingClientRect?.() || {{ left: 0, top: 0, width: 1, height: 1 }};
    const clientX = Number(rect.left || 0) + Math.max(1, Number(rect.width || 1) / 2);
    const clientY = Number(rect.top || 0) + Math.max(1, Number(rect.height || 1) / 2);
    for (const [type, kind, init] of [
      ["pointerleave", "PointerEvent", {{ pointerId: 1, pointerType: "mouse", isPrimary: true, relatedTarget, clientX, clientY }}],
      ["mouseleave", "MouseEvent", {{ relatedTarget, clientX, clientY }}],
      ["pointermove", "PointerEvent", {{ pointerId: 1, pointerType: "mouse", isPrimary: true, relatedTarget, clientX, clientY }}],
      ["mousemove", "MouseEvent", {{ relatedTarget, clientX, clientY }}]
    ]) {{
      dispatch(element, type, kind, init);
    }}
  }}

  async function waitForPickerClose(pill, state, options) {{
    let verification = readPickerCloseVerification(pill, state, options);
    for (let attempt = 0; attempt < 3 && !verification.ok; attempt += 1) {{
      await wait(50);
      verification = readPickerCloseVerification(pill, state, options);
    }}
    return verification;
  }}

  let pickerCloseMethod = null;
  let pickerCloseVerification = null;
  async function closeMenus(pill, state = null, options = {{}}) {{
    const methods = [];
    let verification = readPickerCloseVerification(pill, state, options);
    const tryMethod = async (method, action) => {{
      methods.push(method);
      try {{
        await action();
      }} catch {{
        // Continue to the next bounded close path and fail closed if needed.
      }}
      verification = await waitForPickerClose(pill, state, options);
    }};

    if (!verification.ok) {{
      await tryMethod("escape", () => keyPress(pill, "Escape", "Escape"));
    }}
    if (!verification.ok && !verification.pickerSurfaceClosed && state?.familyTrigger) {{
      const neutral = neutralComposerArea(pill);
      await tryMethod("hover_leave", () => dispatchHoverLeaveEvents(state.familyTrigger, neutral));
      if (!verification.ok && !verification.pickerSurfaceClosed) {{
        await tryMethod("trigger_escape", () => keyPress(state.familyTrigger, "Escape", "Escape"));
      }}
    }}
    if (!verification.ok && leftoverTriggers().length > 0) {{
      for (const trigger of leftoverTriggers()) {{
        await tryMethod("leftover_escape", () => keyPress(trigger, "Escape", "Escape"));
        if (verification.ok) break;
      }}
    }}
    if (!verification.ok && !verification.pickerSurfaceClosed) {{
      const neutral = neutralComposerArea(pill);
      if (neutral) await tryMethod("neutral_click", () => realClick(neutral));
    }}

    pickerCloseMethod = methods.length > 0 ? methods.join("+") : null;
    pickerCloseVerification = verification;
    return verification.ok;
  }}

  async function readFamilyProof(state) {{
    const submenu = await openFamilyMenu(state);
    const familyItems = radios(submenu);
    const checkedItems = familyItems.filter((item) => item.getAttribute("aria-checked") === "true");
    const families = familyItems.map(textOf).filter(Boolean);
    const checkedFamily = checkedItems.length === 1 ? textOf(checkedItems[0]) : "";
    state.familyLabel = checkedFamily;
    state.familyProof = checkedItems.length === 1 && fold(checkedFamily) === "gpt-5.6 sol";
    state.familyItems = familyItems;
    state.familyCheckedItems = checkedItems;
    return {{
      ok: state.familyProof,
      submenu,
      familyItems,
      checkedItems,
      families,
      sol: familyItems.find((item) => fold(textOf(item)) === "gpt-5.6 sol") || null
    }};
  }}

  function recordPostCloseVerification(verification) {{
    const {{ ok, ...receipt }} = verification;
    postCloseVerification = receipt;
    return verification;
  }}

  async function reverifyAfterClose(pill, selectedState) {{
    let reopened = null;
    let reopenedState = null;
    if (selectedState?.shape === "personal") {{
      reopened = await openPersonal(pill);
      reopenedState = reopened ? readPersonalState(reopened) : null;
    }} else if (selectedState?.shape === "slider") {{
      reopened = await openHybrid(pill);
      if (reopened) {{
        const familyItems = radios(reopened);
        const checkedItems = familyItems.filter(isChecked);
        const checkedFamily = checkedItems.length === 1 ? textOf(checkedItems[0]) : "";
        const slider = Array.from(reopened.querySelectorAll("[role='slider']") || [])
          .find((node) => sliderSnapshot(node, reopened));
        const snapshot = sliderSnapshot(slider, reopened);
        reopenedState = {{
          shape: "slider",
          menu: reopened,
          effortItems: [],
          effortSlider: slider,
          familyLabel: checkedFamily,
          familyProof: checkedItems.length === 1 && fold(checkedFamily) === "gpt-5.6 sol",
          verifiedEffortDisplay: snapshot?.display || null,
          familyTrigger: null
        }};
      }}
    }} else {{
      reopened = await openMain(pill);
      reopenedState = reopened ? readState(reopened) : null;
    }}
    if (!reopenedState || !supportedPickerShape(reopenedState)) {{
      return recordPostCloseVerification({{
        ok: false,
        postCloseFamilyStatus: "unverified",
        postCloseEffortStatus: "unverified",
        postClosePickerShape: reopenedState?.shape || null,
        postClosePickerCloseVerification: null,
        postCloseClosedPillFamilyStatus: null,
        postCloseClosedPillEffortStatus: null,
        postCloseClosedPillText: null
      }});
    }}
    const familyProof = reopenedState.shape === "personal"
      ? await readPersonalFamilyProof(reopenedState)
      : reopenedState.shape === "slider"
        ? {{
          ok: reopenedState.familyProof === true,
          families: [reopenedState.familyLabel].filter(Boolean),
          checkedItems: reopenedState.familyProof ? [{{}}] : [],
          sol: null
        }}
        : await readFamilyProof(reopenedState);
    const familyStatus = familyProof.ok ? "verified" : "unverified";
    const verifiedState = familyProof.state || reopenedState;
    const effortStatus = effortVerified(verifiedState) ? "verified" : "unverified";
    const closed = await closeMenus(pill, verifiedState, {{ requireProPill: true }});
    pill = await waitForPill();
    const closedPill = closedPillDiagnostics(pill, verifiedState);
    return recordPostCloseVerification({{
      ok: familyStatus === "verified"
        && effortStatus === "verified"
        && closed
        && closedPill.closedPillEffortStatus === "verified"
        && closedPill.closedPillFamilyStatus !== "unverified",
      postCloseFamilyStatus: familyStatus,
      postCloseEffortStatus: effortStatus,
      postClosePickerShape: verifiedState.shape || null,
      postClosePickerCloseVerification: pickerCloseVerification,
      postCloseClosedPillFamilyStatus: closedPill.closedPillFamilyStatus,
      postCloseClosedPillEffortStatus: closedPill.closedPillEffortStatus,
      postCloseClosedPillText: closedPill.closedPillText || null
    }});
  }}

  function familyVerified(state) {{
    return state?.familyProof === true && fold(state?.familyLabel) === "gpt-5.6 sol";
  }}

  function effortVerified(state) {{
    if (state?.shape === "slider") return fold(state?.verifiedEffortDisplay) === "pro";
    if (state?.shape === "personal") return fold(state?.effortLabel) === "pro";
    const items = state?.effortItems || [];
    const checked = items.find((item) => isChecked(item));
    const checkedLabel = checked ? fold(textOf(checked)) : null;
    return checkedLabel === "pro";
  }}

  function supportedPickerShape(state) {{
    return state?.shape === "menu" || state?.shape === "slider" || state?.shape === "personal";
  }}

  function pillConfirmsEffortLabel(pillText, effortLabel) {{
    const foldedPill = fold(pillText).replace(/\s+/g, " ");
    const foldedEffort = fold(effortLabel).replace(/\s+/g, " ");
    return foldedEffort === "pro" && (foldedPill === "pro" || foldedPill.endsWith(" pro"));
  }}

  function foldFamilyLabel(value) {{
    return fold(value).replace(/^gpt[\s-]*/, "").replace(/\s+/g, " ");
  }}

  function pillConfirmsFamilyLabel(pillText, familyLabel) {{
    const foldedFamily = foldFamilyLabel(familyLabel);
    if (!foldedFamily) return false;
    return fold(pillText).split(/\n+/).some((line) => {{
      const foldedLine = foldFamilyLabel(line);
      return foldedLine === foldedFamily || foldedLine.startsWith(`${{foldedFamily}} `);
    }});
  }}

  function pillHasModelFamilyToken(pillText) {{
    const foldedPill = fold(pillText).replace(/\s+/g, " ");
    return /\bgpt[\s.-]*\d/.test(foldedPill)
      || /\bo\d(?:[\s.-]*\d)?\b/.test(foldedPill)
      || /\b\d+(?:\.\d+)+\b/.test(foldedPill);
  }}

  function closedPillDiagnostics(pill, state) {{
    const pillText = pill ? textOf(pill) : "";
    const familyLabel = state?.familyLabel || "";
    const effortLabel = state?.verifiedEffortDisplay
      || textOf(state?.effortItems?.find((item) => fold(textOf(item)) === "pro"));
    const closedPillFamilyStatus = pillText && familyLabel
      ? pillConfirmsFamilyLabel(pillText, familyLabel)
        ? "verified"
        : pillHasModelFamilyToken(pillText) ? "unverified" : "skipped"
      : "skipped";
    const closedPillEffortStatus = pillText && effortLabel
      ? pillConfirmsEffortLabel(pillText, effortLabel) ? "verified" : "unverified"
      : "skipped";
    return {{
      closedPillText: pillText,
      closedPillFamilyStatus,
      closedPillEffortStatus
    }};
  }}

  let postCloseVerification = {{
    postCloseFamilyStatus: "skipped",
    postCloseEffortStatus: "skipped",
    postClosePickerShape: null,
    postClosePickerCloseVerification: null,
    postCloseClosedPillFamilyStatus: null,
    postCloseClosedPillEffortStatus: null,
    postCloseClosedPillText: null
  }};

  function result(status, pill, state, families, warning = null) {{
    const closedPill = closedPillDiagnostics(pill, state);
    const pickerShapeIsSupported = supportedPickerShape(state);
    const pickerFamilyIsVerified = pickerShapeIsSupported && familyVerified(state);
    const pickerEffortIsVerified = pickerShapeIsSupported && effortVerified(state);
    const familyCorroborated = closedPill.closedPillFamilyStatus === "verified"
      || postCloseVerification.postCloseFamilyStatus === "verified";
    const resultStatus = status === "selected"
      && (!pickerShapeIsSupported
        || !pickerFamilyIsVerified
        || !pickerEffortIsVerified
        || !familyCorroborated
        || closedPill.closedPillEffortStatus !== "verified"
        || postCloseVerification.postCloseFamilyStatus === "unverified"
        || postCloseVerification.postCloseEffortStatus === "unverified")
      ? "selection-mismatch"
      : status;
    const resultWarning = resultStatus === "selection-mismatch" && !warning
      ? "ChatGPT model picker exposed an unsupported shape; refusing unverified model selection"
      : warning;
    const familyIsVerified = pickerFamilyIsVerified
      && closedPill.closedPillFamilyStatus !== "unverified"
      && familyCorroborated;
    const effortIsVerified = pickerEffortIsVerified && closedPill.closedPillEffortStatus !== "unverified";
    const items = state?.effortItems || [];
    const checked = items.find((item) => isChecked(item));
    const checkedLabel = checked ? fold(textOf(checked)) : null;
    const verifiedEffort = checked && checkedLabel === "pro" ? textOf(checked) : null;
    const verifiedEffortDisplay = state?.shape === "slider"
      ? state?.verifiedEffortDisplay
      : state?.shape === "personal" ? state?.effortLabel : verifiedEffort;
    const modelUsed = resultStatus === "current"
      ? (pill ? textOf(pill) : "")
      : (resultStatus === "selected" && familyIsVerified && effortIsVerified && verifiedEffortDisplay
        ? `GPT-5.6 Sol ${{verifiedEffortDisplay}}`
        : null);
    return {{
      requested,
      status: resultStatus,
      modelUsed,
      familyStatus: familyIsVerified ? "verified" : "unverified",
      effortStatus: effortIsVerified ? "verified" : "unverified",
      pickerFamilyStatus: pickerFamilyIsVerified ? "verified" : "unverified",
      pickerEffortStatus: pickerEffortIsVerified ? "verified" : "unverified",
      ...closedPill,
      ...postCloseVerification,
      pickerShape: state?.shape || null,
      pickerCloseMethod,
      pickerCloseVerification,
      pillText: textOf(pill),
      familyLabel: state?.familyLabel || null,
      availableItems: (state?.effortItems || []).map(textOf).filter(Boolean),
      availableFamilies: families || [],
      warning: resultWarning,
      ...surfaceFields(),
      url: window.location.href || "",
      title: document.title || ""
    }};
  }}

  let surface = null;
  let surfaceEvidenceSeen = {prior_surface_evidence_seen};
  function surfaceFields() {{
    return {{
      surfaceElapsedMs: surface?.elapsedMs ?? null,
      surfaceAttempts: surface?.attempts ?? 0,
      surfaceVerificationAttempts: surface?.verificationAttempts ?? 0,
      surfaceState: surface?.state ?? surfaceState(null),
      surfaceObservedValues: surface?.observedValues ?? [],
      surfaceProofKind: surface?.surfaceProofKind ?? null,
      surfaceChatState: surface?.surfaceChatState ?? surfaceState(null),
      surfaceWorkState: surface?.surfaceWorkState ?? surfaceState(null),
      surfaceVisibleToggleCount: surface?.surfaceVisibleToggleCount ?? 0,
      surfaceComposerAria: surface?.surfaceComposerAria ?? null,
      surfaceEvidenceSeen
    }};
  }}

  surface = await ensureChatSurface();
  if (!surface.ok) return result("not-found", null, null, [], surface.warning);
  if (strategy === "current") {{
    const pill = await waitForPill();
    return {{
      requested,
      status: "current",
      modelUsed: pill ? textOf(pill) : "",
      familyStatus: "skipped",
      effortStatus: "skipped",
      pillText: pill ? textOf(pill) : "",
      familyLabel: null,
      availableItems: [],
      availableFamilies: [],
      warning: "model pinning bypassed — answer may come from any model",
      ...surfaceFields(),
      url: window.location.href || "",
      title: document.title || ""
    }};
  }}

  if (requested !== supported) {{
    return result("not-found", null, null, [], "this recipe supports only GPT-5.6 Sol at Chat effort Pro");
  }}
  const legacy = legacyPickerMarkers();
  if (legacy.length > 0) {{
    const failure = result("legacy-picker", null, null, [], "legacy ChatGPT picker detected; this yoetz version requires the GPT-5.6 UI");
    failure.legacyPicker = legacy.slice(0, 10);
    return failure;
  }}

  let pill = await waitForPill();
  if (!pill) {{
    const lateLegacy = legacyPickerMarkers();
    if (lateLegacy.length > 0) {{
      const failure = result("legacy-picker", null, null, [], "legacy ChatGPT picker detected; this yoetz version requires the GPT-5.6 UI");
      failure.legacyPicker = lateLegacy.slice(0, 10);
      return failure;
    }}
    return result("missing-selector", null, null, [], "ChatGPT GPT-5.6 composer model pill not found");
  }}

  let menu = await openMain(pill);
  let state = menu ? readState(menu) : null;
  let families = [];
  if (!state) {{
    const personal = await openPersonal(pill);
    if (personal) {{
      const personalState = readPersonalState(personal);
      if (personalState) return await selectPersonal(pill, personalState);
      return result("not-found", pill, null, families, "ChatGPT personal picker did not expose Model and Effort controls");
    }}
    const hybrid = await openHybrid(pill);
    if (hybrid) return await selectHybrid(pill, hybrid);
    return result("not-found", pill, null, families, "ChatGPT GPT-5.6 model picker did not open");
  }}

  let familyProof = await readFamilyProof(state);
  families = familyProof.families;
  if (!familyProof.ok) {{
    if (familyProof.checkedItems.length !== 1 || !familyProof.sol) {{
      await closeMenus(pill);
      return result(
        "not-found",
        pill,
        state,
        families,
        familyProof.checkedItems.length === 1
          ? "GPT-5.6 Sol was not visible in the family submenu"
          : "GPT-5.6 Sol family menu did not expose one checked model"
      );
    }}
    realClick(familyProof.sol);
    await wait(250);
    pill = await waitForPill();
    if (!pill) return result("selection-mismatch", null, null, families, "ChatGPT composer model pill did not remount after selecting GPT-5.6 Sol");
    menu = await openMain(pill);
    state = menu ? readState(menu) : null;
    if (!state) return result("selection-mismatch", pill, null, families, "picker did not reopen after selecting GPT-5.6 Sol");
    familyProof = await readFamilyProof(state);
    families = familyProof.families.length > 0 ? familyProof.families : families;
    if (!familyProof.ok) {{
      await closeMenus(pill);
      return result("selection-mismatch", pill, state, families, "GPT-5.6 Sol family menu selection could not be verified");
    }}
  }}
  if (!effortVerified(state)) {{
    const proTier = state.effortItems.find((item) => fold(textOf(item)) === "pro") || null;
    if (!proTier) {{
      await closeMenus(pill);
      return result("not-found", pill, state, families, "Pro was not visible as a GPT-5.6 Sol effort tier");
    }}
    realClick(proTier);
    await wait(250);
    pill = await waitForPill();
    if (!pill) return result("selection-mismatch", null, null, families, "ChatGPT composer model pill did not remount after selecting Pro effort");
    menu = await openMain(pill);
    state = menu ? readState(menu) : null;
    if (!state) return result("selection-mismatch", pill, null, families, "picker did not reopen after selecting Pro effort");
  }}

  familyProof = await readFamilyProof(state);
  families = familyProof.families.length > 0 ? familyProof.families : families;
  if (!familyProof.ok) {{
    await closeMenus(pill);
    return result("selection-mismatch", pill, state, families, "GPT-5.6 Sol family menu could not be re-verified after effort selection");
  }}
  const familyIsVerified = familyVerified(state);
  const effortIsVerified = effortVerified(state);
  if (!familyIsVerified || !effortIsVerified) {{
    await closeMenus(pill);
    return result("selection-mismatch", pill, state, families, "GPT-5.6 Sol at verified Pro effort could not be confirmed in one picker pass");
  }}
  if (!await closeMenus(pill, state, {{ requireProPill: true }})) {{
    return result("selection-mismatch", pill, state, families, "ChatGPT model picker remained open after verification");
  }}
  const closedPill = closedPillDiagnostics(pill, state);
  if (closedPill.closedPillFamilyStatus === "unverified") {{
    return result("selection-mismatch", pill, state, families, "ChatGPT composer model pill reported another model family after closing the picker");
  }}
  if (closedPill.closedPillEffortStatus !== "verified") {{
    return result("selection-mismatch", pill, state, families, "ChatGPT composer model pill did not confirm verified Pro effort");
  }}
  if (closedPill.closedPillFamilyStatus === "skipped") {{
    const postClose = await reverifyAfterClose(pill, state);
    if (!postClose.ok) {{
      return result("selection-mismatch", pill, state, families, "ChatGPT model family was not independently re-read after the closed composer pill omitted it");
    }}
  }}
  return result("selected", pill, state, families);
}}
"##,
        requested_model = requested_model,
        model_button_selector = model_button_selector,
        composer_selector = composer_selector,
        visibility_helpers = JS_VISIBILITY_HELPERS,
        prior_surface_evidence_seen = prior_surface_evidence_seen,
    )
}

pub fn build_attachment_probe_function(file_name: &str) -> Result<String> {
    let file_name_json = serde_json::to_string(file_name)?;
    let file_stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(file_name);
    let file_stem_json = serde_json::to_string(file_stem)?;
    let attachment_tile_selector_json = attachment_tile_selector_json();
    Ok(format!(
        r##"
() => {{
  const fileName = {file_name_json};
  const fileStem = {file_stem_json};
  const ATTACHMENT_TILE_SELECTOR = {attachment_tile_selector_json};
  const COMPOSER_SELECTOR = {composer_selector_json};
  const clip = (value, max = 160) => String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
{visibility_helpers}
{composer_scope_helpers}
  const escapeRegExp = (value) => String(value || "").replace(/[.*+?^${{}}()|[\]\\]/g, "\\$&");
  const stripExpectedName = (value) => {{
    let text = String(value || "");
    [fileName, fileStem].filter(Boolean).forEach((name) => {{
      text = text.replace(new RegExp(escapeRegExp(name), "gi"), " ");
    }});
    return clip(text, 220);
  }};
  const includesName = (value) => {{
    const text = String(value || "");
    return !!fileName && text.includes(fileName);
  }};
  const includesStem = (value) => {{
    const text = String(value || "");
    return !!fileStem && text.includes(fileStem);
  }};
  const failurePattern = /\b(upload failed|failed|failure|error uploading|upload error|attachment error|file error|something went wrong|could not|couldn't|cannot|can't|unsupported|too large|blocked|try again)\b/i;
  const busyPattern = /\b(uploading|processing|scanning|attaching|preparing|loading)\b/i;
  const busySelector = "[aria-busy='true'], [role='progressbar'], [data-state='uploading'], [data-state='loading'], [class*='progress'], [class*='spinner'], [class*='animate-spin']";
  const {{ composerForm, composerRoot }} = getComposerScope();
  const readFields = (tile) => [
    clip(tile.innerText || tile.textContent || ""),
    clip(tile.getAttribute?.("aria-label") || ""),
    clip(tile.getAttribute?.("title") || ""),
    clip(tile.getAttribute?.("data-testid") || "", 80),
  ].filter(Boolean);
  const childHasExactName = (tile) => Array.from(tile.querySelectorAll("*")).some((node) => {{
    const text = clip(node.innerText || node.textContent || "", 220);
    return text === fileName ||
      clip(node.getAttribute?.("aria-label") || "", 220) === fileName ||
      clip(node.getAttribute?.("title") || "", 220) === fileName;
  }});
  const busyInfo = (tile) => {{
    const busyNodes = [
      ...(tile.matches?.(busySelector) ? [tile] : []),
      ...Array.from(tile.querySelectorAll(busySelector)),
    ].filter((node) => isVisible(node));
    const text = stripExpectedName(tile.innerText || tile.textContent || "");
    return {{
      busy: busyNodes.length > 0 || busyPattern.test(text),
      busyMarkerCount: busyNodes.length,
      busyText: busyPattern.test(text) ? text : "",
    }};
  }};
  const describeTile = (tile, scope, index) => {{
    const fields = readFields(tile);
    const combined = fields.join(" ");
    const failureText = fields.map(stripExpectedName).join(" ");
    const combinedNameMatched = includesName(combined) || includesStem(combined);
    const exactNameMatched = fields.some((field) => field === fileName) ||
      (!combinedNameMatched && childHasExactName(tile));
    const nameMatched = exactNameMatched || combinedNameMatched;
    const busy = busyInfo(tile);
    const failed = failurePattern.test(failureText);
    return {{
      scope,
      index,
      text: fields[0] || "",
      ariaLabel: fields[1] || "",
      title: fields[2] || "",
      testId: fields[3] || "",
      nameMatched,
      exactNameMatched,
      stemMatched: !exactNameMatched && includesStem(combined),
      busy: busy.busy,
      busyMarkerCount: busy.busyMarkerCount,
      busyText: busy.busyText,
      failure: failed,
      failureText: failed ? clip(failureText, 180) : "",
      ready: nameMatched && !busy.busy && !failed,
    }};
  }};
  const collectTiles = (root, scope) => root
    ? Array.from(root.querySelectorAll(ATTACHMENT_TILE_SELECTOR))
      .filter((tile) => isVisible(tile))
      .map((tile, index) => describeTile(tile, scope, index))
    : [];
  const scopedTiles = collectTiles(composerRoot, composerForm ? "form" : "composer-root");
  const documentTiles = collectTiles(document, "document");
  const scopedMatches = scopedTiles.filter((entry) => entry.nameMatched);
  const documentMatches = documentTiles.filter((entry) => entry.nameMatched);
  const matchedTiles = scopedMatches.some((entry) => entry.exactNameMatched)
    ? scopedMatches.filter((entry) => entry.exactNameMatched)
    : scopedMatches.length > 0
      ? scopedMatches
      : documentMatches.some((entry) => entry.exactNameMatched)
        ? documentMatches.filter((entry) => entry.exactNameMatched)
        : documentMatches;
  const alertEvidence = Array.from(document.querySelectorAll("[role='alert'], [aria-live], [class*='error'], [data-testid*='error']"))
    .filter((el) => isVisible(el))
    .map((el) => clip(el.innerText || el.textContent || el.getAttribute?.("aria-label") || "", 180))
    .filter((text) => failurePattern.test(text) && (includesName(text) || includesStem(text) || /\bupload|attachment|file\b/i.test(text)))
    .slice(0, 4);
  const failureEvidence = [
    ...matchedTiles.filter((entry) => entry.failure),
    ...alertEvidence.map((text) => ({{ scope: "alert", text, failure: true }})),
  ].slice(0, 6);
  const busyEvidence = matchedTiles.filter((entry) => entry.busy).slice(0, 6);
  const readinessEvidence = matchedTiles.filter((entry) => entry.ready).slice(0, 6);
  const inputs = Array.from(document.querySelectorAll("input[type='file']")).map((input) => ({{
    fileNames: Array.from(input.files || []).map((file) => file.name),
    multiple: !!input.multiple,
    inComposer: !!(composerRoot && composerRoot.contains(input)),
  }}));
  const inputMatched = inputs.some((input) => input.fileNames.some((name) => name === fileName));
  const attachmentFailure = failureEvidence.length > 0;
  const exactNameMatched = matchedTiles.some((entry) => entry.exactNameMatched);
  const readyNow = readinessEvidence.length > 0 && busyEvidence.length === 0 && !attachmentFailure;
  const readyCounts = window.__yoetzAttachmentReadyCounts || (window.__yoetzAttachmentReadyCounts = Object.create(null));
  const readyKey = `file:${{fileName}}`;
  readyCounts[readyKey] = readyNow ? (Number(readyCounts[readyKey] || 0) + 1) : 0;
  const status = attachmentFailure
    ? "failed"
    : readyNow
      ? "done"
      : matchedTiles.length > 0 || inputMatched
        ? "uploading"
        : (scopedTiles.length > 0 || documentTiles.length > 0 ? "no_match" : "no_tile");
  if (readyNow) {{
    return {{
      ok: true,
      status,
      visibleEvidence: matchedTiles.slice(0, 6),
      readinessEvidence,
      busyEvidence,
      failureEvidence,
      inputMatched,
      exactNameMatched,
      stableReadyCount: readyCounts[readyKey],
      composerScoped: !!composerRoot,
      scopeUsed: matchedTiles[0]?.scope || null,
      tileCount: scopedTiles.length || documentTiles.length,
    }};
  }}
  return {{
    ok: false,
    status,
    visibleEvidence: matchedTiles.slice(0, 6),
    readinessEvidence,
    busyEvidence,
    failureEvidence,
    attachmentFailure,
    exactNameMatched,
    stableReadyCount: readyCounts[readyKey],
    inputMatched,
    inputs,
    composerScoped: !!composerRoot,
    scopeUsed: matchedTiles[0]?.scope || null,
    tileCount: scopedTiles.length || documentTiles.length,
    matchedTileCount: matchedTiles.length,
    fallbackMatches: scopedMatches.length > 0 ? [] : documentMatches.slice(0, 4),
  }};
}}
"##,
        attachment_tile_selector_json = attachment_tile_selector_json,
        composer_selector_json = composer_selector_json(),
        visibility_helpers = JS_VISIBILITY_HELPERS,
        composer_scope_helpers = JS_COMPOSER_SCOPE_HELPERS,
    ))
}

pub(crate) fn build_open_attachment_ui_function() -> String {
    let attachment_trigger_selector_json = attachment_trigger_selector_json();
    format!(
        r##"
() => {{
    const ATTACHMENT_TRIGGER_SELECTOR = {attachment_trigger_selector_json};
    const clip = (value, max = 120) => String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
{visibility_helpers}
    const button = Array.from(document.querySelectorAll(ATTACHMENT_TRIGGER_SELECTOR)).find((el) => isVisible(el))
    || document.querySelector(ATTACHMENT_TRIGGER_SELECTOR);
  if (!button) {{
    return {{
      status: "not-found",
      url: window.location.href || "",
      title: document.title || "",
    }};
  }}
  button.click();
  return {{
    status: "opened",
    label: clip(button.getAttribute?.("aria-label") || button.getAttribute?.("title") || button.innerText || button.textContent || ""),
    testId: clip(button.getAttribute?.("data-testid") || "", 80),
  }};
}}
"##,
        attachment_trigger_selector_json = attachment_trigger_selector_json,
        visibility_helpers = JS_VISIBILITY_HELPERS,
    )
}

pub(crate) fn build_upload_menu_item_click_function() -> String {
    let upload_menu_text_pattern_json = upload_menu_text_pattern_json();
    format!(
        r##"
() => {{
    const TEXT_PATTERN = new RegExp({upload_menu_text_pattern_json}, "i");
    const clip = (value, max = 120) => String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
{visibility_helpers}
  const selectors = ["[role='menuitem']", "button", "[role='button']", "label", "li"];
  const nodes = Array.from(document.querySelectorAll(selectors.join(",")));
  const target = nodes.find((el) => {{
    const text = `${{el.innerText || ""}} ${{el.getAttribute?.("aria-label") || ""}} ${{el.getAttribute?.("title") || ""}}`
      .replace(/\s+/g, " ")
      .trim();
    return isVisible(el) && TEXT_PATTERN.test(text);
  }});
  if (!target) {{
    return {{ status: "not-found" }};
  }}
  target.click();
  return {{
    status: "clicked",
    label: clip(target.getAttribute?.("aria-label") || target.getAttribute?.("title") || target.innerText || target.textContent || ""),
  }};
}}
"##,
        upload_menu_text_pattern_json = upload_menu_text_pattern_json,
        visibility_helpers = JS_VISIBILITY_HELPERS,
    )
}

/// Marker value written to `title` on the composer-scoped file input so the
/// snapshot walker (and `upload_file`'s fallback) can identify the correct
/// target. Using the composer form's own file input — never a page-wide
/// first-match — keeps the bundle from landing on unrelated hidden inputs.
pub(crate) const COMPOSER_FILE_INPUT_MARKER: &str = "yoetz-upload-target";

pub(crate) fn build_scope_composer_file_input_function() -> String {
    let composer_selector_json = composer_selector_json();
    let marker_json = serde_json::to_string(COMPOSER_FILE_INPUT_MARKER)
        .expect("serialize composer-file-input marker");
    format!(
        r##"
() => {{
  const COMPOSER_SELECTOR = {composer_selector_json};
  const MARKER = {marker_json};
  const composer = document.querySelector(COMPOSER_SELECTOR);
  if (!composer) return {{ status: "no-composer" }};
  const form = composer.closest("form");
  if (!form) return {{ status: "no-form" }};
  const input = form.querySelector("input[type='file']");
  if (!input) return {{ status: "no-input" }};
  // Clean up any stale marker from prior runs so we always mark the current
  // composer input.
  document
    .querySelectorAll(`input[type='file'][title='${{MARKER}}']`)
    .forEach((el) => {{ if (el !== input) el.removeAttribute("title"); }});
  input.setAttribute("title", MARKER);
  return {{ status: "marked" }};
}}
"##,
    )
}

#[cfg(test)]
pub fn build_send_button_click_function() -> String {
    build_send_button_click_function_with_surface_evidence(false)
}

pub fn build_send_button_click_function_with_surface_evidence(
    surface_evidence_seen: bool,
) -> String {
    let send_button_selector_json = send_button_selector_json();
    let composer_selector_json = composer_selector_json();
    let attachment_tile_selector_json = attachment_tile_selector_json();
    format!(
        r##"
({{ surfaceEvidenceSeen: surfaceEvidenceSeenOverride = null }} = {{}}) => {{
  const SEND_BUTTON_SELECTOR = {send_button_selector_json};
    const COMPOSER_SELECTOR = {composer_selector_json};
    const ATTACHMENT_TILE_SELECTOR = {attachment_tile_selector_json};
    const clip = (value, max = 120) => String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
{visibility_helpers}
{composer_scope_helpers}
  const CHAT_SURFACE_GROUP_SELECTOR = '[role="radiogroup"][aria-label="Select chat surface"]';
  const CHAT_SURFACE_CHAT_SELECTOR = '[role="radio"][data-tpp-toggle-value="chatgpt"]';
  const CHAT_SURFACE_WORK_SELECTOR = '[role="radio"][data-tpp-toggle-value="work"]';
  const PRIOR_SURFACE_EVIDENCE_SEEN = {surface_evidence_seen};
  const priorSurfaceEvidenceSeen = surfaceEvidenceSeenOverride === true || PRIOR_SURFACE_EVIDENCE_SEEN;
  const surfaceControls = () => {{
    const candidates = [];
    for (const group of Array.from(document.querySelectorAll(CHAT_SURFACE_GROUP_SELECTOR))) {{
      if (!isVisibleWithoutLayout(group)) continue;
      const chats = Array.from(group.querySelectorAll(CHAT_SURFACE_CHAT_SELECTOR)).filter((node) => isVisible(node));
      const works = Array.from(group.querySelectorAll(CHAT_SURFACE_WORK_SELECTOR)).filter((node) => isVisible(node));
      if (chats.length !== 1 || works.length !== 1) return null;
      candidates.push({{ group, chat: chats[0], work: works[0] }});
    }}
    return candidates.length === 1 ? candidates[0] : null;
  }};
  const surfaceState = (node) => ({{
    ariaChecked: node?.getAttribute?.("aria-checked") || null,
    dataState: node?.getAttribute?.("data-state") || null,
  }});
  const surfaceSelectionIsChat = (controls) => {{
    const chat = surfaceState(controls?.chat);
    const work = surfaceState(controls?.work);
    return chat.ariaChecked === "true" && work.ariaChecked === "false";
  }};
  const surfaceObservedToggleNodes = (group = null) => Array.from(
    (group || document).querySelectorAll?.('[role="radio"][data-tpp-toggle-value]') || []
  )
    .filter((node) => isVisible(node));
  const surfaceObservedValues = (group = null) => surfaceObservedToggleNodes(group)
    .map((node) => node.getAttribute("data-tpp-toggle-value"))
    .filter(Boolean)
    .filter((value, index, values) => values.indexOf(value) === index)
    .slice(0, 10);
  const surfaceEvidencePresent = () => document.querySelector(CHAT_SURFACE_GROUP_SELECTOR) !== null
    || document.querySelector('[role="radio"][data-tpp-toggle-value]') !== null;
  const ensureChatSurfaceBeforeSend = () => {{
    const controls = surfaceControls();
    const observedValues = surfaceObservedValues();
    const visibleSurfaceToggleCount = surfaceObservedToggleNodes().length;
    const currentSurfaceEvidenceSeen = surfaceEvidencePresent();
    const evidenceSeen = priorSurfaceEvidenceSeen || currentSurfaceEvidenceSeen;
    const composer = findVisible(document, COMPOSER_SELECTOR);
    const chatState = surfaceState(controls?.chat);
    const workState = surfaceState(controls?.work);
    if (controls && surfaceSelectionIsChat(controls) && visibleSurfaceToggleCount === 2) {{
      return {{
        ok: true,
        evidenceSeen,
        state: chatState,
        observedValues,
        surfaceProofKind: "explicit_chat_work_radios",
        surfaceChatState: chatState,
        surfaceWorkState: workState,
        surfaceVisibleToggleCount,
        surfaceComposerAria: null,
      }};
    }}
    if (evidenceSeen) {{
      return {{
        ok: false,
        reason: controls ? "chat_surface_selection_mismatch" : "chat_surface_control_not_found",
        observedValues,
        evidenceSeen,
        state: chatState,
        surfaceProofKind: null,
        surfaceChatState: chatState,
        surfaceWorkState: workState,
        surfaceVisibleToggleCount,
        surfaceComposerAria: composer?.getAttribute("aria-label") || null,
      }};
    }}
    if (!controls && composer?.getAttribute("aria-label") === "Chat with ChatGPT") {{
      return {{
        ok: true,
        evidenceSeen: false,
        state: surfaceState(null),
        observedValues,
        surfaceProofKind: "implicit_chat_composer_aria",
        surfaceChatState: surfaceState(null),
        surfaceWorkState: surfaceState(null),
        surfaceVisibleToggleCount,
        surfaceComposerAria: "Chat with ChatGPT",
      }};
    }}
    return {{
      ok: false,
      reason: "chat_surface_control_not_found",
      observedValues,
      evidenceSeen,
      state: chatState,
      composerAria: composer?.getAttribute("aria-label") || null,
      surfaceProofKind: null,
      surfaceChatState: chatState,
      surfaceWorkState: workState,
      surfaceVisibleToggleCount,
      surfaceComposerAria: composer?.getAttribute("aria-label") || null,
    }};
  }};
  const {{ composerEl, composerForm, composerRoot, roots: searchRoots }} = getComposerScope();
  const seenButtons = new Set();
  const buttonEntries = [];
  searchRoots.forEach((root) => {{
    const scope = root === composerForm ? "form" : root === composerRoot ? "composer-root" : "document";
    Array.from(root.querySelectorAll(SEND_BUTTON_SELECTOR)).forEach((button) => {{
      if (!seenButtons.has(button) && isVisible(button)) {{
        seenButtons.add(button);
        buttonEntries.push({{ button, scope }});
      }}
    }});
  }});
  const enabledEntry = buttonEntries.find((entry) => !entry.button.disabled) || null;
  const assistantMessages = Array.from(document.querySelectorAll("[data-message-author-role='assistant']"));
  const lastAssistant = assistantMessages.length > 0 ? assistantMessages[assistantMessages.length - 1] : null;
  const attachmentRoot = composerRoot || document;
  const diagnostics = {{
    url: window.location.href || "",
    title: document.title || "",
    attachmentPresent: !!attachmentRoot.querySelector(ATTACHMENT_TILE_SELECTOR),
    composerTextLength: ((composerEl?.innerText || composerEl?.textContent || "").trim()).length,
    composerScoped: !!composerRoot,
    buttonCount: buttonEntries.length,
    buttons: buttonEntries.slice(0, 6).map((entry) => ({{
      scope: entry.scope,
      text: clip(entry.button.innerText || entry.button.textContent || ""),
      testId: clip(entry.button.getAttribute?.("data-testid") || "", 80),
      disabled: !!entry.button.disabled,
      ariaLabel: clip(entry.button.getAttribute?.("aria-label") || ""),
    }})),
  }};
  const surface = ensureChatSurfaceBeforeSend();
  if (!surface.ok) {{
    return {{
      status: "surface-not-ready",
      diagnostics: {{
        ...diagnostics,
        chatSurface: surface,
      }},
      surfaceEvidenceSeen: surface.evidenceSeen,
    }};
  }}
  if (!enabledEntry) {{
    return {{
      status: "not-ready",
      diagnostics,
      surfaceEvidenceSeen: surface.evidenceSeen,
    }};
  }}
  const enabledButton = enabledEntry.button;
  enabledButton.click();
  return {{
    status: "sent",
    sendScope: enabledEntry.scope,
    selectorLabel: clip(enabledButton.getAttribute?.("data-testid") || enabledButton.getAttribute?.("aria-label") || enabledButton.innerText || enabledButton.textContent || "", 80),
    assistantCountBeforeSend: assistantMessages.length,
    assistantLastLenBeforeSend: (lastAssistant?.innerText || "").length,
      surfaceEvidenceSeen: surface.evidenceSeen,
      surfaceState: surface.state,
      surfaceObservedValues: surface.observedValues,
      surfaceProofKind: surface.surfaceProofKind,
      surfaceChatState: surface.surfaceChatState,
      surfaceWorkState: surface.surfaceWorkState,
      surfaceVisibleToggleCount: surface.surfaceVisibleToggleCount,
      surfaceComposerAria: surface.surfaceComposerAria,
  }};
}}
"##,
        send_button_selector_json = send_button_selector_json,
        composer_selector_json = composer_selector_json,
        attachment_tile_selector_json = attachment_tile_selector_json,
        visibility_helpers = JS_VISIBILITY_HELPERS,
        composer_scope_helpers = JS_COMPOSER_SCOPE_HELPERS,
        surface_evidence_seen = surface_evidence_seen,
    )
}

#[cfg(test)]
pub fn build_send_button_click_function_with_model_selection(
    surface_evidence_seen: bool,
) -> String {
    build_send_button_click_function_with_model_selection_for(
        "gpt-5-6-sol-chat-pro",
        ChatgptModelStrategy::Select,
        surface_evidence_seen,
    )
}

pub fn build_send_button_click_function_with_model_selection_for(
    requested_model: &str,
    model_strategy: ChatgptModelStrategy,
    surface_evidence_seen: bool,
) -> String {
    let model_source = build_model_selection_function_with_surface_evidence(
        requested_model,
        model_strategy,
        surface_evidence_seen,
    );
    let send_source = build_send_button_click_function_with_surface_evidence(surface_evidence_seen);
    let model_source_json = serde_json::to_string(&model_source).expect("serialize model source");
    let send_source_json = serde_json::to_string(&send_source).expect("serialize send source");
    let model_button_selector_json = model_selector_button_selector_json();
    let composer_selector_json = composer_selector_json();
    let requested_model_json =
        serde_json::to_string(requested_model).expect("serialize requested model");
    let model_strategy_json =
        serde_json::to_string(&model_strategy).expect("serialize model strategy");
    format!(
        r##"
async () => {{
  const MODEL_BUTTON_SELECTOR = {model_button_selector_json};
  const COMPOSER_SELECTOR = {composer_selector_json};
{visibility_helpers}
{composer_scope_helpers}
  const modelFn = eval("(" + {model_source_json} + ")");
  const selection = await modelFn();
  const requestedModel = {requested_model_json};
  const modelStrategy = {model_strategy_json};
  const supportedShapes = new Set(["menu", "slider", "personal"]);
  const close = selection?.pickerCloseVerification;
  const familyCorroborated = selection?.closedPillFamilyStatus === "verified"
    || selection?.postCloseFamilyStatus === "verified";
  const selectedProofOk = selection?.status === "selected"
    && modelStrategy === "select"
    && requestedModel === "gpt-5-6-sol-chat-pro"
    && selection?.requested === "gpt-5-6-sol-chat-pro"
    && selection?.modelUsed === "GPT-5.6 Sol Pro"
    && selection?.familyStatus === "verified"
    && selection?.effortStatus === "verified"
    && selection?.pickerFamilyStatus === "verified"
    && selection?.pickerEffortStatus === "verified"
    && supportedShapes.has(selection?.pickerShape)
    && familyCorroborated
    && selection?.closedPillEffortStatus === "verified"
    && selection?.postCloseFamilyStatus !== "unverified"
    && selection?.postCloseEffortStatus !== "unverified"
    && close?.pickerSurfaceClosed === true
    && close?.modelTriggerClosed === true
    && close?.familyTriggerClosed === true
    && close?.closedPillPro === true;
  const currentProofOk = modelStrategy === "current"
    && selection?.status === "current"
    && selection?.requested === "current"
    && selection?.familyStatus === "skipped"
    && selection?.effortStatus === "skipped";
  const modelProofOk = selectedProofOk || currentProofOk;
  if (!modelProofOk) {{
    return {{
      status: "model-not-ready",
      modelSelection: selection,
      surfaceEvidenceSeen: selection?.surfaceEvidenceSeen === true,
    }};
  }}
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const fold = (value) => normalize(value).toLowerCase();
  const pillLabel = (node) => normalize([
    node?.innerText,
    node?.textContent,
    node?.getAttribute?.("aria-label"),
    node?.getAttribute?.("title"),
  ].filter(Boolean).join(" "));
  const foldFamilyLabel = (value) => fold(value)
    .replace(/^gpt[\s-]*/, "")
    .replace(/\s+/g, " ");
  const pillHasModelFamilyToken = (pillText) => {{
    const foldedPill = fold(pillText).replace(/\s+/g, " ");
    return /\bgpt[\s.-]*\d/.test(foldedPill)
      || /\bo\d(?:[\s.-]*\d)?\b/.test(foldedPill)
      || /\b\d+(?:\.\d+)+\b/.test(foldedPill);
  }};
  const pillConfirmsFamilyLabel = (pillText, familyLabel) => {{
    const foldedFamily = foldFamilyLabel(familyLabel);
    if (!foldedFamily) return false;
    return fold(pillText).split(/\n+/).some((line) => {{
      const foldedLine = foldFamilyLabel(line);
      return foldedLine === foldedFamily || foldedLine.startsWith(`${{foldedFamily}} `);
    }});
  }};
  const pillConfirmsEffortLabel = (pillText) => {{
    const foldedPill = fold(pillText).replace(/\s+/g, " ");
    return foldedPill === "pro" || foldedPill.endsWith(" pro");
  }};
  const findCurrentModelPill = () => {{
    const {{ roots }} = getComposerScope();
    const seen = new Set();
    const buttons = [];
    for (const root of roots) {{
      for (const button of Array.from(root.querySelectorAll(MODEL_BUTTON_SELECTOR))) {{
        if (!seen.has(button)) {{
          seen.add(button);
          buttons.push(button);
        }}
      }}
    }}
    const closed = (button) => button.getAttribute("aria-expanded") !== "true"
      && button.getAttribute("data-state") !== "open";
    const visible = buttons.filter((button) => isVisible(button) && closed(button));
    const pills = visible.filter((button) => button.classList?.contains("__composer-pill"));
    const summaryMatches = (value) => {{
      const folded = fold(value);
      return /^(?:instant|medium|high|extra high|pro|max|light)$/.test(folded)
        || /^\d+(?:\.\d+)+(?: sol)? (?:instant|medium|high|extra high|pro|max|light)$/.test(folded)
        || /\bgpt[\s.-]*\d/.test(folded);
    }};
    return pills.find((button) => summaryMatches(pillLabel(button)))
      || pills.find((button) => pillHasModelFamilyToken(pillLabel(button)))
      || pills[0]
      || visible.find((button) => summaryMatches(pillLabel(button)))
      || null;
  }};
  const currentPill = findCurrentModelPill();
  const currentPillText = currentPill ? pillLabel(currentPill) : "";
  const currentPillHasFamily = pillHasModelFamilyToken(currentPillText);
  const currentPillFamilyStatus = currentPillHasFamily
    ? pillConfirmsFamilyLabel(currentPillText, selection?.familyLabel)
      ? "verified"
      : "unverified"
    : (selection?.closedPillFamilyStatus === "verified"
      || selection?.postCloseFamilyStatus === "verified" ? "verified" : "unverified");
  const currentPillEffortStatus = pillConfirmsEffortLabel(currentPillText)
    ? "verified"
    : "unverified";
  const finalModelProof = modelStrategy === "current"
    ? {{
      ok: currentProofOk && Boolean(currentPill && currentPillText),
      currentPillText,
      currentPillFamilyStatus,
      currentPillEffortStatus,
    }}
    : {{
      ok: selectedProofOk
        && Boolean(currentPill && currentPillText)
        && currentPillFamilyStatus === "verified"
        && currentPillEffortStatus === "verified",
      currentPillText,
      currentPillFamilyStatus,
      currentPillEffortStatus,
    }};
  if (!finalModelProof.ok) {{
    return {{
      status: "model-not-ready",
      modelSelection: {{
        ...selection,
        currentClosedPillText: finalModelProof.currentPillText || null,
        currentClosedPillFamilyStatus: finalModelProof.currentPillFamilyStatus,
        currentClosedPillEffortStatus: finalModelProof.currentPillEffortStatus,
      }},
      surfaceEvidenceSeen: selection?.surfaceEvidenceSeen === true,
    }};
  }}
  const sendFn = eval("(" + {send_source_json} + ")");
  const send = sendFn({{ surfaceEvidenceSeen: selection?.surfaceEvidenceSeen === true }});
  const finalModelSelection = {{
    ...selection,
    modelUsed: modelStrategy === "current" ? currentPillText : selection?.modelUsed,
    clickBound: send?.status === "sent",
    clickBoundClosedPillText: currentPillText || null,
    clickBoundClosedPillFamilyStatus: currentPillFamilyStatus,
    clickBoundClosedPillEffortStatus: currentPillEffortStatus,
    surfaceEvidenceSeen: send?.surfaceEvidenceSeen === true || selection?.surfaceEvidenceSeen === true,
    surfaceState: send?.surfaceState ?? selection?.surfaceState ?? null,
    surfaceObservedValues: send?.surfaceObservedValues ?? selection?.surfaceObservedValues ?? [],
    surfaceProofKind: send?.surfaceProofKind ?? selection?.surfaceProofKind ?? null,
    surfaceChatState: send?.surfaceChatState ?? selection?.surfaceChatState ?? null,
    surfaceWorkState: send?.surfaceWorkState ?? selection?.surfaceWorkState ?? null,
    surfaceVisibleToggleCount: send?.surfaceVisibleToggleCount ?? selection?.surfaceVisibleToggleCount ?? 0,
    surfaceComposerAria: send?.surfaceComposerAria ?? selection?.surfaceComposerAria ?? null,
  }};
  return {{
    ...send,
    finalModelSelection,
    surfaceEvidenceSeen: finalModelSelection.surfaceEvidenceSeen,
  }};
}}
"##,
        model_source_json = model_source_json,
        send_source_json = send_source_json,
        model_button_selector_json = model_button_selector_json,
        composer_selector_json = composer_selector_json,
        requested_model_json = requested_model_json,
        model_strategy_json = model_strategy_json,
        visibility_helpers = JS_VISIBILITY_HELPERS,
        composer_scope_helpers = JS_COMPOSER_SCOPE_HELPERS,
    )
}

fn canonical_chatgpt_nested_object(
    value: Option<&serde_json::Value>,
    fields: &[(&str, &str)],
) -> serde_json::Value {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        return serde_json::Value::Null;
    };
    let mut canonical = serde_json::Map::new();
    for (snake_case, camel_case) in fields {
        canonical.insert(
            (*snake_case).to_string(),
            object
                .get(*camel_case)
                .or_else(|| object.get(*snake_case))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(canonical)
}

pub fn canonical_chatgpt_final_model_selection(selection: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "status": selection.get("status").cloned().unwrap_or(serde_json::Value::Null),
        "model_used": selection.get("modelUsed").cloned().unwrap_or(serde_json::Value::Null),
        "requested_model": selection.get("requested").cloned().unwrap_or(serde_json::Value::Null),
        "family_status": selection.get("familyStatus").cloned().unwrap_or(serde_json::Value::Null),
        "effort_status": selection.get("effortStatus").cloned().unwrap_or(serde_json::Value::Null),
        "picker_family_status": selection.get("pickerFamilyStatus").cloned().unwrap_or(serde_json::Value::Null),
        "picker_effort_status": selection.get("pickerEffortStatus").cloned().unwrap_or(serde_json::Value::Null),
        "picker_shape": selection.get("pickerShape").cloned().unwrap_or(serde_json::Value::Null),
        "post_close_family_status": selection.get("postCloseFamilyStatus").cloned().unwrap_or(serde_json::Value::Null),
        "post_close_effort_status": selection.get("postCloseEffortStatus").cloned().unwrap_or(serde_json::Value::Null),
        "post_close_picker_shape": selection.get("postClosePickerShape").cloned().unwrap_or(serde_json::Value::Null),
        "post_close_picker_close_verification": canonical_chatgpt_nested_object(
            selection.get("postClosePickerCloseVerification").or_else(|| selection.get("post_close_picker_close_verification")),
            &[
                ("picker_surface_closed", "pickerSurfaceClosed"),
                ("model_trigger_closed", "modelTriggerClosed"),
                ("family_trigger_closed", "familyTriggerClosed"),
                ("closed_pill_pro", "closedPillPro"),
                ("closed_pill_text", "closedPillText"),
                ("ok", "ok"),
            ],
        ),
        "post_close_closed_pill_family_status": selection.get("postCloseClosedPillFamilyStatus").cloned().unwrap_or(serde_json::Value::Null),
        "post_close_closed_pill_effort_status": selection.get("postCloseClosedPillEffortStatus").cloned().unwrap_or(serde_json::Value::Null),
        "post_close_closed_pill_text": selection.get("postCloseClosedPillText").cloned().unwrap_or(serde_json::Value::Null),
        "post_close_failure_reason": selection.get("postCloseFailureReason").cloned().unwrap_or(serde_json::Value::Null),
        "closed_pill_family_status": selection.get("closedPillFamilyStatus").cloned().unwrap_or(serde_json::Value::Null),
        "closed_pill_effort_status": selection.get("closedPillEffortStatus").cloned().unwrap_or(serde_json::Value::Null),
        "closed_pill_text": selection.get("closedPillText").cloned().unwrap_or(serde_json::Value::Null),
        "surface_evidence_seen": selection.get("surfaceEvidenceSeen").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "surface_state": canonical_chatgpt_nested_object(
            selection.get("surfaceState").or_else(|| selection.get("surface_state")),
            &[("aria_checked", "ariaChecked"), ("data_state", "dataState")],
        ),
        "surface_observed_values": selection.get("surfaceObservedValues").cloned().unwrap_or_else(|| serde_json::json!([])),
        "surface_proof_kind": selection.get("surfaceProofKind").cloned().or_else(|| selection.get("surface_proof_kind").cloned()).unwrap_or(serde_json::Value::Null),
        "surface_chat_state": canonical_chatgpt_nested_object(
            selection.get("surfaceChatState").or_else(|| selection.get("surface_chat_state")),
            &[("aria_checked", "ariaChecked"), ("data_state", "dataState")],
        ),
        "surface_work_state": canonical_chatgpt_nested_object(
            selection.get("surfaceWorkState").or_else(|| selection.get("surface_work_state")),
            &[("aria_checked", "ariaChecked"), ("data_state", "dataState")],
        ),
        "surface_visible_toggle_count": selection.get("surfaceVisibleToggleCount").cloned().or_else(|| selection.get("surface_visible_toggle_count").cloned()).unwrap_or(serde_json::Value::Null),
        "surface_composer_aria": selection.get("surfaceComposerAria").cloned().or_else(|| selection.get("surface_composer_aria").cloned()).unwrap_or(serde_json::Value::Null),
        "picker_close_verification": canonical_chatgpt_nested_object(
            selection.get("pickerCloseVerification").or_else(|| selection.get("picker_close_verification")),
            &[
                ("picker_surface_closed", "pickerSurfaceClosed"),
                ("model_trigger_closed", "modelTriggerClosed"),
                ("family_trigger_closed", "familyTriggerClosed"),
                ("closed_pill_pro", "closedPillPro"),
                ("closed_pill_text", "closedPillText"),
                ("ok", "ok"),
            ],
        ),
        "click_bound": selection.get("clickBound").cloned().unwrap_or(serde_json::Value::Bool(false)),
        "click_bound_closed_pill_text": selection.get("clickBoundClosedPillText").cloned().unwrap_or(serde_json::Value::Null),
        "click_bound_closed_pill_family_status": selection.get("clickBoundClosedPillFamilyStatus").cloned().unwrap_or(serde_json::Value::Null),
        "click_bound_closed_pill_effort_status": selection.get("clickBoundClosedPillEffortStatus").cloned().unwrap_or(serde_json::Value::Null),
    })
}

pub fn validate_chatgpt_final_model_selection(
    selection: &serde_json::Value,
    strategy: ChatgptModelStrategy,
) -> Result<()> {
    let object = selection
        .as_object()
        .ok_or_else(|| anyhow!("ChatGPT final model selection receipt must be an object"))?;
    let expected_status = match strategy {
        ChatgptModelStrategy::Select => "selected",
        ChatgptModelStrategy::Current => "current",
    };
    let expected_requested = match strategy {
        ChatgptModelStrategy::Select => crate::chatgpt_recipe::CHATGPT_SOL_CHAT_PRO_MODEL,
        ChatgptModelStrategy::Current => "current",
    };
    if object
        .get("click_bound")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err(anyhow!("click_bound is not true"));
    }
    if object.get("status").and_then(serde_json::Value::as_str) != Some(expected_status) {
        return Err(anyhow!("receipt status is not {expected_status}"));
    }
    if object
        .get("requested_model")
        .and_then(serde_json::Value::as_str)
        != Some(expected_requested)
    {
        return Err(anyhow!(
            "receipt requested_model is not {expected_requested}"
        ));
    }
    let click_pill = object
        .get("click_bound_closed_pill_text")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("receipt is missing non-empty click-time closed pill text"))?;

    match strategy {
        ChatgptModelStrategy::Select => {
            for (field, expected) in [
                ("model_used", "GPT-5.6 Sol Pro"),
                ("family_status", "verified"),
                ("effort_status", "verified"),
                ("picker_family_status", "verified"),
                ("picker_effort_status", "verified"),
                ("click_bound_closed_pill_effort_status", "verified"),
            ] {
                if object.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
                    return Err(anyhow!("receipt {field} is not {expected}"));
                }
            }
            let click_bound_family_status = object
                .get("click_bound_closed_pill_family_status")
                .and_then(serde_json::Value::as_str);
            let click_bound_family_proven = click_bound_family_status == Some("verified")
                || (click_bound_family_status == Some("skipped")
                    && object
                        .get("family_status")
                        .and_then(serde_json::Value::as_str)
                        == Some("verified")
                    && object
                        .get("picker_family_status")
                        .and_then(serde_json::Value::as_str)
                        == Some("verified")
                    && object
                        .get("closed_pill_family_status")
                        .and_then(serde_json::Value::as_str)
                        != Some("unverified")
                    && object
                        .get("post_close_family_status")
                        .and_then(serde_json::Value::as_str)
                        != Some("unverified"));
            if !click_bound_family_proven {
                return Err(anyhow!(
                    "receipt click_bound_closed_pill_family_status is not a proven value"
                ));
            }
            if !matches!(
                object
                    .get("picker_shape")
                    .and_then(serde_json::Value::as_str),
                Some("menu" | "slider" | "personal")
            ) {
                return Err(anyhow!("receipt picker_shape is unsupported"));
            }
            let close = object
                .get("picker_close_verification")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| anyhow!("receipt picker_close_verification is missing"))?;
            for field in [
                "picker_surface_closed",
                "model_trigger_closed",
                "family_trigger_closed",
                "closed_pill_pro",
            ] {
                if close.get(field).and_then(serde_json::Value::as_bool) != Some(true) {
                    return Err(anyhow!(
                        "receipt picker_close_verification.{field} is not true"
                    ));
                }
            }
        }
        ChatgptModelStrategy::Current => {
            let model_used = object
                .get("model_used")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow!("Current receipt model_used is empty"))?;
            if collapse_label_whitespace(model_used) != collapse_label_whitespace(click_pill) {
                return Err(anyhow!(
                    "Current receipt model_used does not match the click-time closed pill"
                ));
            }
            for (field, expected) in [("family_status", "skipped"), ("effort_status", "skipped")] {
                if object.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
                    return Err(anyhow!("Current receipt {field} is not {expected}"));
                }
            }
        }
    }

    let observed_values = object
        .get("surface_observed_values")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("receipt surface_observed_values is missing"))?;
    let chat_state = object.get("surface_chat_state");
    let work_state = object.get("surface_work_state");
    let surface_kind = object
        .get("surface_proof_kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("receipt surface_proof_kind is missing"))?;
    match surface_kind {
        "explicit_chat_work_radios" => {
            if object
                .get("surface_evidence_seen")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
                || object
                    .get("surface_visible_toggle_count")
                    .and_then(serde_json::Value::as_u64)
                    != Some(2)
                || !surface_state_is(chat_state, "true")
                || !surface_state_is(work_state, "false")
                || !observed_values
                    .iter()
                    .any(|value| value.as_str() == Some("chatgpt"))
                || !observed_values
                    .iter()
                    .any(|value| value.as_str() == Some("work"))
            {
                return Err(anyhow!("explicit Chat/Work surface proof is incomplete"));
            }
            if object
                .get("surface_composer_aria")
                .is_some_and(|value| !value.is_null())
            {
                return Err(anyhow!(
                    "explicit surface proof must not claim implicit composer proof"
                ));
            }
        }
        "implicit_chat_composer_aria" => {
            if object
                .get("surface_evidence_seen")
                .and_then(serde_json::Value::as_bool)
                != Some(false)
                || object
                    .get("surface_visible_toggle_count")
                    .and_then(serde_json::Value::as_u64)
                    != Some(0)
                || !observed_values.is_empty()
                || object
                    .get("surface_composer_aria")
                    .and_then(serde_json::Value::as_str)
                    != Some("Chat with ChatGPT")
                || chat_state.is_some_and(|value| !value.is_null())
                || work_state.is_some_and(|value| !value.is_null())
            {
                return Err(anyhow!("implicit Chat composer proof is incomplete"));
            }
        }
        other => return Err(anyhow!("unsupported surface_proof_kind {other:?}")),
    }
    Ok(())
}

pub fn validate_chatgpt_completion_payload(
    payload: &serde_json::Value,
    strategy: ChatgptModelStrategy,
) -> Result<()> {
    let receipt = payload
        .get("final_model_selection")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            anyhow!("ChatGPT completion did not include a final model selection receipt")
        })?;
    validate_chatgpt_final_model_selection(receipt, strategy)?;
    if let Some(status) = payload
        .get("model_selection_status")
        .and_then(serde_json::Value::as_str)
    {
        if status
            != receipt
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        {
            return Err(anyhow!(
                "ChatGPT completion model_selection_status disagrees with receipt"
            ));
        }
    }
    if let Some(model_used) = payload
        .get("model_used")
        .and_then(serde_json::Value::as_str)
    {
        if model_used
            != receipt
                .get("model_used")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        {
            return Err(anyhow!(
                "ChatGPT completion model_used disagrees with receipt"
            ));
        }
    }
    Ok(())
}

fn collapse_label_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn surface_state_is(value: Option<&serde_json::Value>, expected_aria_checked: &str) -> bool {
    value
        .and_then(serde_json::Value::as_object)
        .and_then(|state| state.get("aria_checked"))
        .and_then(serde_json::Value::as_str)
        == Some(expected_aria_checked)
}

pub fn build_chatgpt_dom_probe_function() -> String {
    let send_button_selector_json = send_button_selector_json();
    let stop_button_selector_json = stop_button_selector_json();
    let composer_selector_json = composer_selector_json();
    format!(
        r##"
() => {{
  const COMPOSER_SELECTOR = {composer_selector_json};
{visibility_helpers}
{composer_scope_helpers}
{turn_root_helpers}
  const {{ roots }} = getComposerScope();
  const send = roots.flatMap((root) => Array.from(root.querySelectorAll({send_button_selector_json}))).find((button) => isVisible(button)) || null;
  const msgs = Array.from(document.querySelectorAll("[data-message-author-role='assistant']")).filter((msg) => isVisible(msg));
  const lastMsg = msgs.length > 0 ? msgs[msgs.length - 1] : null;
  const turnRoot = latestAssistantTurn(lastMsg);
  const globalStopButton = findVisible(document, {stop_button_selector_json});
  const stopButton = (turnRoot ? findVisible(turnRoot, {stop_button_selector_json}) : null) ||
    ((send?.disabled || !lastMsg) ? globalStopButton : null);
  const stopGenerating = !!stopButton && !stopButton.disabled;
  const thinkingSelector = ".result-thinking, [data-testid*='thinking'], [class*='thinking']";
  const visibleThinking = !!((turnRoot ? findVisible(turnRoot, thinkingSelector) : null) ||
    (!turnRoot ? findVisible(document, thinkingSelector) : null));
  const copyButtons = lastMsg
    ? Array.from((turnRoot || lastMsg).querySelectorAll("button[aria-label*='Copy'], button[data-testid*='copy']")).filter((button) => isVisible(button)).length
    : 0;
  const lastLen = lastMsg ? (lastMsg.innerText || "").length : 0;
  const sendState = !send ? "missing" : send.disabled ? "disabled" : "enabled";
  const errEl = findVisible(document, "[class*='error-toast'], [data-testid*='error'], [role='alert']");
  const errText = errEl ? errEl.innerText.substring(0, 100).toLowerCase() : "";
  const markers = ["network error","something went wrong","error generating","attachment failed","upload failed","too many requests"];
  const err = markers.find((marker) => errText.includes(marker)) || "";
  return `send=${{sendState}}|stop=${{stopGenerating ? 1 : 0}}|thinking=${{visibleThinking ? 1 : 0}}|copy=${{copyButtons}}|msgs=${{msgs.length}}|lastlen=${{lastLen}}|err=${{err}}`;
}}
"##,
        send_button_selector_json = send_button_selector_json,
        stop_button_selector_json = stop_button_selector_json,
        composer_selector_json = composer_selector_json,
        visibility_helpers = JS_VISIBILITY_HELPERS,
        composer_scope_helpers = JS_COMPOSER_SCOPE_HELPERS,
        turn_root_helpers = JS_TURN_ROOT_HELPERS,
    )
}

pub fn build_latest_response_probe_function() -> String {
    let mut source = String::from(
        r#"
() => {
"#,
    );
    source.push_str(JS_VISIBILITY_HELPERS);
    source.push_str(JS_TURN_ROOT_HELPERS);
    source.push_str(
        r#"
  const cleanMessageText = (msg) => {
    if (!msg) return "";
    const clone = msg.cloneNode(true);
    clone.querySelectorAll("button, [role='button'], input, textarea").forEach((node) => node.remove());
    return String(clone.textContent || "").replace(/\s+/g, " ").trim();
  };
  const msgs = Array.from(document.querySelectorAll("[data-message-author-role='assistant']")).filter((msg) => isVisible(msg));
  const lastMsg = msgs.length > 0 ? msgs[msgs.length - 1] : null;
  const turnRoot = latestAssistantTurn(lastMsg);
  const visibleCopyButtons = lastMsg
    ? Array.from((turnRoot || lastMsg).querySelectorAll("button[aria-label*='Copy'], button[data-testid*='copy']")).filter((button) => isVisible(button)).length
    : 0;
  return {
    response: cleanMessageText(lastMsg),
    assistantCount: msgs.length,
    visibleCopyButtons,
  };
}
"#,
    );
    source
}

pub(crate) fn wrap_function_source_for_json_eval(function_source: &str) -> Result<String> {
    let function_json = serde_json::to_string(function_source)?;
    Ok(format!(
        r#"(async () => {{
  const fn = eval("(" + {function_json} + ")");
  return JSON.stringify(await fn());
}})()"#
    ))
}

pub fn looks_authenticated_text(haystack: &str) -> bool {
    contains_any(&haystack.to_lowercase(), AUTH_MARKERS)
}

pub fn is_challenge_text(haystack: &str) -> bool {
    if looks_authenticated_text(haystack) {
        return false;
    }
    contains_any(&haystack.to_lowercase(), CHALLENGE_MARKERS)
}

pub fn detect_auth_issue_text(haystack: &str, live_attach: bool) -> Option<&'static str> {
    let haystack = haystack.to_lowercase();
    if is_challenge_text(&haystack) {
        return Some(if live_attach {
            "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again."
        } else {
            "cloudflare challenge detected. Run `yoetz browser sync-cookies` or `yoetz browser login` and try again."
        });
    }
    if contains_any(&haystack, LOGIN_MARKERS) {
        return Some(if live_attach {
            "chatgpt login required in the attached Chrome session. Log in there and try again."
        } else {
            "chatgpt login required. Run `yoetz browser login` and try again."
        });
    }
    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};
    use serde_json::Value;
    use serial_test::serial;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn stable_idle_threshold_floors_and_scales() {
        assert_eq!(stable_idle_threshold_ms(1_000), 90_000);
        assert_eq!(stable_idle_threshold_ms(10_000), 90_000);
        assert_eq!(stable_idle_threshold_ms(30_000), 90_000);
        assert_eq!(stable_idle_threshold_ms(60_000), 180_000);
        assert_eq!(stable_idle_threshold_ms(120_000), 360_000);
    }

    #[test]
    fn reported_chatgpt_model_requires_verified_sol_and_pro_effort() {
        let selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-chat-pro",
            "modelUsed": "GPT-5.6 Sol Pro",
            "familyStatus": "verified",
            "effortStatus": "verified"
        });
        assert_eq!(
            select_reported_chatgpt_model(&selection, "gpt-5-6-sol-chat-pro"),
            Some("GPT-5.6 Sol Pro".to_string())
        );

        let enterprise_selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-chat-pro",
            "modelUsed": "GPT-5.6 Sol Pro",
            "familyStatus": "verified",
            "effortStatus": "verified"
        });
        assert_eq!(
            select_reported_chatgpt_model(&enterprise_selection, "gpt-5-6-sol-chat-pro"),
            Some("GPT-5.6 Sol Pro".to_string())
        );

        let unknown_selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-chat-pro",
            "modelUsed": "GPT-5.6 Sol Expert",
            "familyStatus": "verified",
            "effortStatus": "verified"
        });
        assert_eq!(
            select_reported_chatgpt_model(&unknown_selection, "gpt-5-6-sol-chat-pro"),
            None
        );
    }

    #[test]
    fn reported_chatgpt_model_accepts_sol_pro_tier() {
        let pro_selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-chat-pro",
            "modelUsed": "GPT-5.6 Sol Pro",
            "familyStatus": "verified",
            "effortStatus": "verified"
        });
        assert_eq!(
            select_reported_chatgpt_model(&pro_selection, "gpt-5-6-sol-chat-pro"),
            Some("GPT-5.6 Sol Pro".to_string())
        );
        assert_eq!(
            chatgpt_model_selection_status(&pro_selection, "gpt-5-6-sol-chat-pro"),
            ChatgptModelSelectionStatus::Selected
        );
    }

    #[test]
    fn reported_chatgpt_model_never_echoes_an_unverified_request() {
        let selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-chat-pro",
            "modelUsed": "Pro Extended",
            "extendedStatus": "required"
        });
        assert_eq!(
            select_reported_chatgpt_model(&selection, "gpt-5-6-sol-chat-pro"),
            None
        );
    }

    #[test]
    fn reported_chatgpt_model_returns_current_pill_text_without_picker_proof() {
        let selection = serde_json::json!({
            "status": "current",
            "requested": "current",
            "modelUsed": "5.5 Instant",
            "familyStatus": "skipped",
            "effortStatus": "skipped"
        });
        assert_eq!(
            select_reported_chatgpt_model(&selection, "current"),
            Some("5.5 Instant".to_string())
        );
    }

    #[test]
    fn chatgpt_model_selection_status_reports_contract_values() {
        assert_eq!(
            chatgpt_model_selection_status(
                &serde_json::json!({
                    "status": "selected",
                    "requested": "gpt-5-6-sol-chat-pro",
                    "modelUsed": "GPT-5.6 Sol Pro",
                    "familyStatus": "verified",
                    "effortStatus": "verified"
                }),
                "gpt-5-6-sol-chat-pro"
            ),
            ChatgptModelSelectionStatus::Selected
        );
        assert_eq!(
            chatgpt_model_selection_status(
                &serde_json::json!({
                    "status": "current",
                    "requested": "current",
                    "modelUsed": "5.5 Instant",
                    "familyStatus": "skipped",
                    "effortStatus": "skipped"
                }),
                "current"
            ),
            ChatgptModelSelectionStatus::Current
        );
        assert_eq!(
            chatgpt_model_selection_status(
                &serde_json::json!({
                    "status": "selected",
                    "requested": "extended-pro",
                    "modelUsed": "Pro Extended",
                    "extendedStatus": "required"
                }),
                "gpt-5-6-sol-chat-pro"
            ),
            ChatgptModelSelectionStatus::Mismatch
        );
        assert_eq!(
            chatgpt_model_selection_status(
                &serde_json::json!({"status": "missing-selector"}),
                "current"
            ),
            ChatgptModelSelectionStatus::Unavailable
        );
        assert_eq!(
            chatgpt_model_selection_status(
                &serde_json::json!({"status": "selection-mismatch"}),
                "gpt-5-6-sol-chat-pro"
            ),
            ChatgptModelSelectionStatus::Mismatch
        );
    }

    #[test]
    fn auth_detection_prefers_authenticated_markers_over_challenge_words() {
        let review_text =
            r#"{"ref":"prompt-textarea","text":"verify you are human and security check"}"#;
        assert!(looks_authenticated_text(review_text));
        assert!(!is_challenge_text(review_text));
        assert_eq!(detect_auth_issue_text(review_text, true), None);
    }

    #[test]
    fn auth_detection_distinguishes_challenge_and_login() {
        assert_eq!(
            detect_auth_issue_text("Verify you are human", true),
            Some(
                "cloudflare challenge detected in the attached Chrome session. Solve it in your browser window and try again."
            )
        );
        assert_eq!(
            detect_auth_issue_text("Please sign in", false),
            Some("chatgpt login required. Run `yoetz browser login` and try again.")
        );
    }

    #[test]
    fn model_selection_function_requires_verified_sol_family_and_pro_effort() {
        let script =
            build_model_selection_function("gpt-5-6-sol-chat-pro", ChatgptModelStrategy::Select);
        assert!(script.contains(r#"const requested = "gpt-5-6-sol-chat-pro";"#));
        assert!(script.contains(
            "classList.contains(\"__composer-pill\") && summaryMatches(pillLabel(button))"
        ));
        assert!(script.contains("function leftoverTriggers()"));
        assert!(script.contains("function hybridMenu()"));
        assert!(script.contains("function selectModelViewToggle(menu)"));
        assert!(script.contains("async function activateHybridFamilyView(menu)"));
        assert!(script.contains(
            "classList.contains(\"__composer-pill\") && pillHasModelFamilyToken(pillLabel(button))"
        ));
        assert!(script.contains("async function selectHybrid(pill, menu)"));
        assert!(script.contains("menu = await activateHybridFamilyView(menu);"));
        assert!(script.contains("state.verifiedEffortDisplay = liveSnap.display || \"Pro\""));
        assert!(script.contains("closedPill.closedPillEffortStatus !== \"verified\""));
        assert!(script.contains("classList.contains(\"__composer-pill\")"));
        assert!(script.contains(r#"[role="radiogroup"][aria-label="Select chat surface"]"#));
        assert!(script.contains(r#"[role="radio"][data-tpp-toggle-value="chatgpt"]"#));
        assert!(script.contains("if (!isVisibleWithoutLayout(group)) continue;"));
        let surface_guard = script
            .find("surface = await ensureChatSurface();")
            .expect("generated picker includes the Chat surface guard");
        let family_picker = script
            .find("let familyProof = await readFamilyProof(state);")
            .expect("generated picker includes family selection logic");
        assert!(surface_guard < family_picker);
        assert!(script.contains("function surfaceSelectionIsChat(controls)"));
        assert!(script.contains("work.ariaChecked === \"false\""));
        assert!(script.contains(".filter((node) => isVisible(node))"));
        assert!(script.contains("return candidates.length === 1 ? candidates[0] : null;"));
        assert!(script.contains("function implicitChatSurfaceProof(observedValues)"));
        assert!(script.contains("function surfaceEvidencePresent()"));
        assert!(script.contains("pickerSurfaceIsOpen(menu)"));
        assert!(script.contains("state === \"closed\""));
        assert!(script.contains("const SURFACE_SETTLE_TIMEOUT_MS = 2000;"));
        assert!(script.contains("function surfaceObservedValues(group = null)"));
        assert!(script.contains("const SURFACE_REQUIRED_STABLE_OBSERVATIONS = 2;"));
        assert!(script.contains("let surfaceEvidenceSeen = false;"));
        assert!(script.contains("shape: \"personal\""));
        assert!(script.contains("async function selectPersonal(pill, state)"));
        assert!(script.contains("function surfaceObservedToggleNodes(group = null)"));
        assert!(script.contains("surfaceVerificationAttempts"));
        assert!(script.contains("surfaceObservedValues"));
        assert!(script.contains("surfaceEvidenceSeen"));
        assert!(script.contains("ariaChecked"));
        assert!(script.contains("dataState"));
        assert!(!script.contains("surfacePollTimeline"));
        assert!(!script.contains("surfaceEnvironment"));
        assert!(script.contains(
            "legacy ChatGPT picker detected; this yoetz version requires the GPT-5.6 UI"
        ));
        assert!(script.contains(r#"familyStatus: familyIsVerified ? "verified" : "unverified""#));
        assert!(script.contains(r#"effortStatus: effortIsVerified ? "verified" : "unverified""#));
        assert!(script.contains(r#"fold(textOf(item)) === "gpt-5.6 sol""#));
        assert!(script.contains("async function readFamilyProof(state)"));
        assert!(script.contains("item.getAttribute(\"aria-checked\") === \"true\""));
        assert!(script.contains("state.familyProof = checkedItems.length === 1"));
        assert!(script.contains("async function closeMenus(pill, state = null"));
        assert!(script.contains("dispatchHoverLeaveEvents"));
        assert!(script.contains("pickerCloseMethod"));
        assert!(script.contains("function closedPillDiagnostics(pill, state)"));
        assert!(script.contains("closedPillFamilyStatus"));
        assert!(script.contains("closedPillEffortStatus"));
        // Ladder-aware effortVerified (yz-7p3.3 finding D): the verified set now
        // requires the visible Pro effort tier.
        assert!(script.contains("return checkedLabel === \"pro\";"));
        assert!(!script.contains(r#"fold(textOf(item)) === "max""#));
        assert!(script.contains("Pro was not visible as a GPT-5.6 Sol effort tier"));
        assert!(script.contains("GPT-5.6 Sol at verified Pro effort could not be confirmed"));
        assert!(script.contains("this recipe supports only GPT-5.6 Sol at Chat effort Pro"));
        assert!(script.contains(r#"/^(?:gpt|o\d)\b/i.test(textOf(item))"#));
        assert!(script.contains("await openFamilyMenu"));
        assert!(script.contains("async function waitForPill()"));
        assert!(script.contains("pill = await waitForPill();"));
        assert!(script
            .contains("ChatGPT composer model pill did not remount after selecting GPT-5.6 Sol"));
        assert!(script
            .contains("ChatGPT composer model pill did not remount after selecting Pro effort"));
        assert!(script.contains("return verification.ok;"));
        assert!(script.contains("if (!await closeMenus(pill, state"));
        assert!(!script.contains("if (families.length === 0 && state.familyTrigger)"));
        assert!(script.contains("await closeMenus"));
        assert!(!script.contains("model-switcher-gpt-5-4"));
    }

    #[test]
    fn send_click_binds_model_proof_to_the_actual_click() {
        let script = build_send_button_click_function_with_model_selection(false);
        assert!(script.contains("const selection = await modelFn();"));
        assert!(script.contains("const selectedProofOk ="));
        assert!(script.contains("const modelProofOk = selectedProofOk || currentProofOk;"));
        assert!(script.contains("if (!modelProofOk)"));
        assert!(script.contains(
            "const send = sendFn({ surfaceEvidenceSeen: selection?.surfaceEvidenceSeen === true });"
        ));
        assert!(script.contains("clickBound: send?.status === \"sent\""));
        assert!(script.contains("clickBoundClosedPillText: currentPillText || null"));
        assert!(script.contains("surfaceEvidenceSeenOverride"));
        assert!(script.contains(
            "const evidenceSeen = priorSurfaceEvidenceSeen || currentSurfaceEvidenceSeen;"
        ));
        assert!(script.contains("finalModelSelection"));

        let current = build_send_button_click_function_with_model_selection_for(
            "current",
            ChatgptModelStrategy::Current,
            true,
        );
        assert!(current.contains("const modelStrategy = \"current\";"));
        assert!(current.contains("const currentProofOk = modelStrategy === \"current\""));
        assert!(current.contains("const PRIOR_SURFACE_EVIDENCE_SEEN = true;"));
    }

    #[test]
    fn canonical_final_model_selection_preserves_click_receipt() {
        let receipt = canonical_chatgpt_final_model_selection(&serde_json::json!({
            "status": "selected",
            "modelUsed": "GPT-5.6 Sol Pro",
            "requested": "gpt-5-6-sol-chat-pro",
            "familyStatus": "verified",
            "effortStatus": "verified",
            "pickerFamilyStatus": "verified",
            "pickerEffortStatus": "verified",
            "pickerShape": "personal",
            "postCloseFamilyStatus": "verified",
            "postCloseEffortStatus": "verified",
            "closedPillFamilyStatus": "skipped",
            "closedPillEffortStatus": "verified",
            "closedPillText": "Pro",
            "clickBoundClosedPillFamilyStatus": "verified",
            "clickBoundClosedPillEffortStatus": "verified",
            "clickBoundClosedPillText": "GPT-5.6 Sol Pro",
            "surfaceEvidenceSeen": true,
            "surfaceState": {"ariaChecked": "true", "dataState": "on"},
            "surfaceObservedValues": ["chatgpt", "work"],
            "surfaceProofKind": "explicit_chat_work_radios",
            "surfaceChatState": {"ariaChecked": "true", "dataState": "on"},
            "surfaceWorkState": {"ariaChecked": "false", "dataState": "off"},
            "surfaceVisibleToggleCount": 2,
            "surfaceComposerAria": null,
            "pickerCloseVerification": {
                "pickerSurfaceClosed": true,
                "modelTriggerClosed": true,
                "familyTriggerClosed": true,
                "closedPillPro": true
            },
            "clickBound": true
        }));
        assert_eq!(receipt["click_bound"], true);
        assert_eq!(receipt["surface_evidence_seen"], true);
        assert_eq!(receipt["picker_shape"], "personal");
        assert_eq!(receipt["post_close_family_status"], "verified");
        assert_eq!(receipt["click_bound_closed_pill_text"], "GPT-5.6 Sol Pro");
        assert_eq!(receipt["click_bound_closed_pill_family_status"], "verified");
        assert_eq!(receipt["click_bound_closed_pill_effort_status"], "verified");
        assert_eq!(
            receipt["picker_close_verification"]["picker_surface_closed"],
            true
        );
        assert_eq!(receipt["surface_state"]["aria_checked"], "true");
        assert_eq!(receipt["surface_chat_state"]["data_state"], "on");
        assert_eq!(receipt["surface_work_state"]["aria_checked"], "false");
        validate_chatgpt_final_model_selection(&receipt, ChatgptModelStrategy::Select).unwrap();
    }

    #[test]
    fn final_model_selection_validator_accepts_select_and_current_receipts() {
        let mut selected = serde_json::json!({
            "status": "selected",
            "model_used": "GPT-5.6 Sol Pro",
            "requested_model": "gpt-5-6-sol-chat-pro",
            "family_status": "verified",
            "effort_status": "verified",
            "picker_family_status": "verified",
            "picker_effort_status": "verified",
            "picker_shape": "personal",
            "picker_close_verification": {
                "picker_surface_closed": true,
                "model_trigger_closed": true,
                "family_trigger_closed": true,
                "closed_pill_pro": true
            },
            "click_bound": true,
            "click_bound_closed_pill_text": "GPT-5.6 Sol Pro",
            "click_bound_closed_pill_family_status": "verified",
            "click_bound_closed_pill_effort_status": "verified",
            "surface_evidence_seen": true,
            "surface_proof_kind": "explicit_chat_work_radios",
            "surface_chat_state": {"aria_checked": "true"},
            "surface_work_state": {"aria_checked": "false"},
            "surface_visible_toggle_count": 2,
            "surface_composer_aria": null,
            "surface_observed_values": ["chatgpt", "work"]
        });
        validate_chatgpt_final_model_selection(&selected, ChatgptModelStrategy::Select).unwrap();

        selected["closed_pill_family_status"] = serde_json::json!("skipped");
        selected["closed_pill_effort_status"] = serde_json::json!("verified");
        selected["closed_pill_text"] = serde_json::json!("Pro");
        selected["post_close_family_status"] = serde_json::json!("verified");
        selected["post_close_effort_status"] = serde_json::json!("verified");
        selected["click_bound_closed_pill_family_status"] = serde_json::json!("skipped");
        selected["click_bound_closed_pill_text"] = serde_json::json!("Pro");
        validate_chatgpt_final_model_selection(&selected, ChatgptModelStrategy::Select).unwrap();

        selected["picker_family_status"] = serde_json::json!("unverified");
        assert!(
            validate_chatgpt_final_model_selection(&selected, ChatgptModelStrategy::Select)
                .unwrap_err()
                .to_string()
                .contains("picker_family_status")
        );

        let current = serde_json::json!({
            "status": "current",
            "model_used": "5.5 Instant",
            "requested_model": "current",
            "family_status": "skipped",
            "effort_status": "skipped",
            "click_bound": true,
            "click_bound_closed_pill_text": "5.5 Instant",
            "surface_evidence_seen": false,
            "surface_proof_kind": "implicit_chat_composer_aria",
            "surface_visible_toggle_count": 0,
            "surface_composer_aria": "Chat with ChatGPT",
            "surface_observed_values": [],
            "surface_chat_state": null,
            "surface_work_state": null
        });
        validate_chatgpt_final_model_selection(&current, ChatgptModelStrategy::Current).unwrap();
    }

    #[test]
    fn final_model_selection_validator_rejects_incomplete_surface_proof() {
        let mut receipt = serde_json::json!({
            "status": "selected",
            "model_used": "GPT-5.6 Sol Pro",
            "requested_model": "gpt-5-6-sol-chat-pro",
            "family_status": "verified",
            "effort_status": "verified",
            "picker_family_status": "verified",
            "picker_effort_status": "verified",
            "picker_shape": "personal",
            "picker_close_verification": {
                "picker_surface_closed": true,
                "model_trigger_closed": true,
                "family_trigger_closed": true,
                "closed_pill_pro": true
            },
            "click_bound": true,
            "click_bound_closed_pill_text": "GPT-5.6 Sol Pro",
            "click_bound_closed_pill_family_status": "verified",
            "click_bound_closed_pill_effort_status": "verified",
            "surface_evidence_seen": true,
            "surface_proof_kind": "explicit_chat_work_radios",
            "surface_chat_state": {"aria_checked": "true"},
            "surface_work_state": {"aria_checked": "false"},
            "surface_visible_toggle_count": 2,
            "surface_composer_aria": null,
            "surface_observed_values": ["chatgpt", "work"]
        });
        receipt["surface_work_state"]["aria_checked"] = "true".into();
        let error = validate_chatgpt_final_model_selection(&receipt, ChatgptModelStrategy::Select)
            .unwrap_err();
        assert!(error.to_string().contains("surface proof"));
    }

    #[test]
    fn model_selection_function_current_bypasses_picker() {
        let script = build_model_selection_function("current", ChatgptModelStrategy::Current);
        assert!(script.contains(r#"const requested = "current";"#));
        assert!(script.contains(r#"const strategy = "current";"#));
        assert!(script.contains(r#"familyStatus: "skipped""#));
        assert!(script.contains(r#"effortStatus: "skipped""#));
        assert!(script.contains("model pinning bypassed — answer may come from any model"));
        assert!(script.contains(r#"if (strategy === "current")"#));
    }

    #[test]
    fn scope_composer_file_input_function_targets_composer_form_only() {
        let script = build_scope_composer_file_input_function();
        assert!(script.contains("composer.closest(\"form\")"));
        assert!(script.contains("form.querySelector(\"input[type='file']\")"));
        assert!(script.contains(&format!("\"{}\"", COMPOSER_FILE_INPUT_MARKER)));
        assert!(script.contains("status: \"marked\""));
    }

    #[test]
    fn attachment_probe_function_scopes_by_filename() {
        let script = build_attachment_probe_function("bundle.txt").unwrap();
        assert!(script.contains(r#"const fileName = "bundle.txt";"#));
        assert!(script.contains(r#"const fileStem = "bundle";"#));
        assert!(script.contains("readinessEvidence"));
        assert!(script.contains("stableReadyCount"));
        assert!(script.contains("attachmentFailure"));
        assert!(script.contains("exactNameMatched"));
        assert!(script.contains("busyEvidence"));
    }

    #[test]
    fn attachment_probe_function_ignores_failure_words_inside_filename() {
        let script = build_attachment_probe_function("review-failed-cases.md").unwrap();
        assert!(script.contains(r#"const fileName = "review-failed-cases.md";"#));
        assert!(script.contains("stripExpectedName"));
        assert!(script.contains("const failed = failurePattern.test(failureText);"));
        assert!(
            !script.contains("failurePattern.test(combined)"),
            "tile failure matching must not run against text that still includes the filename"
        );
    }

    #[test]
    fn shared_dom_helper_functions_cover_attachment_and_send_controls() {
        let attachment_ui = build_open_attachment_ui_function();
        assert!(attachment_ui.contains("ATTACHMENT_TRIGGER_SELECTOR"));
        assert!(attachment_ui.contains("status: \"opened\""));

        let upload_menu = build_upload_menu_item_click_function();
        assert!(upload_menu.contains("TEXT_PATTERN"));
        assert!(upload_menu.contains("status: \"clicked\""));

        let send_click = build_send_button_click_function();
        assert!(send_click.contains("SEND_BUTTON_SELECTOR"));
        assert!(send_click.contains("status: \"sent\""));
        assert!(send_click.contains("status: \"not-ready\""));
        assert!(send_click.contains("status: \"surface-not-ready\""));
        assert!(send_click.contains("Chat with ChatGPT"));
        assert!(send_click.contains("data-tpp-toggle-value"));
        assert!(!send_click.contains("controls.chat.click()"));
        assert!(send_click.contains("PRIOR_SURFACE_EVIDENCE_SEEN"));
        assert!(send_click.contains("surfaceEvidenceSeen"));
        assert!(send_click.contains("roots: searchRoots"));
        assert!(send_click.contains("const scope = root === composerForm ? \"form\""));

        let dom_probe = build_chatgpt_dom_probe_function();
        assert!(dom_probe.contains("send="));
        assert!(dom_probe.contains("copyButtons"));
        assert!(dom_probe.contains("stopGenerating = !!stopButton && !stopButton.disabled"));
        assert!(dom_probe.contains("latestAssistantTurn"));
        assert!(dom_probe.contains("copyButtons"));
        assert!(dom_probe.contains("visibleThinking"));

        let latest_response = build_latest_response_probe_function();
        assert!(latest_response.contains("data-message-author-role"));
        assert!(latest_response.contains("response: cleanMessageText(lastMsg)"));
        assert!(latest_response.contains("visibleCopyButtons"));
    }

    #[test]
    fn validate_thread_mode_accepts_fresh_and_empty() {
        validate_thread_mode(None).unwrap();
        validate_thread_mode(Some("")).unwrap();
        validate_thread_mode(Some("fresh")).unwrap();
    }

    #[test]
    fn validate_thread_mode_rejects_reuse_with_migration_message() {
        let err = validate_thread_mode(Some("reuse")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("thread=reuse is no longer supported"));
        assert!(msg.contains("fresh ChatGPT tab"));
    }

    #[test]
    fn validate_thread_mode_rejects_unknown_values() {
        let err = validate_thread_mode(Some("sideways")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unsupported `thread` value `sideways`"));
        assert!(msg.contains("fresh"));
    }

    #[test]
    fn normalize_conversation_accepts_ids_and_chatgpt_urls() {
        let cases = [
            (
                "6a0228a7-4994-832d-8bb0-ea6b35d1b7af",
                "6a0228a7-4994-832d-8bb0-ea6b35d1b7af",
            ),
            (
                "https://chatgpt.com/c/6a0228a7-4994-832d-8bb0-ea6b35d1b7af?_yoetz=run-1",
                "6a0228a7-4994-832d-8bb0-ea6b35d1b7af",
            ),
            ("https://chat.openai.com/c/legacy_123#thread", "legacy_123"),
        ];

        for (raw, expected_id) in cases {
            let conversation = normalize_conversation(raw).unwrap();
            assert_eq!(conversation.id, expected_id);
            assert_eq!(
                conversation.url,
                format!("https://chatgpt.com/c/{expected_id}")
            );
        }
    }

    #[test]
    fn normalize_conversation_rejects_unsafe_or_wrong_targets() {
        for raw in [
            "",
            "   ",
            "https://example.com/c/conv-123",
            "http://chatgpt.com/c/conv-123",
            "https://chatgpt.com/",
            "https://chatgpt.com/g/g-123",
            "https://chatgpt.com/c/conv-123/extra",
            "conv/123",
            "conv?123",
            "conv 123",
            "conv%20123",
            ".",
            "..",
        ] {
            assert!(
                normalize_conversation(raw).is_err(),
                "expected {raw:?} to be rejected"
            );
        }
    }

    #[test]
    fn generate_run_id_is_timestamped_with_hex_suffix() {
        let run_id = generate_run_id();
        let (timestamp, suffix) = run_id.split_once('_').unwrap();
        assert_eq!(timestamp.len(), 16);
        assert!(timestamp.ends_with('Z'));
        assert_eq!(suffix.len(), 6);
        assert!(suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
        validate_run_id(&run_id).unwrap();
    }

    #[test]
    fn validate_run_id_rejects_url_and_log_injection_characters() {
        validate_run_id("run:abc.123_ok-9").unwrap();
        assert!(validate_run_id("").is_err());
        assert!(validate_run_id("run&evil=1").is_err());
        assert!(validate_run_id("run\nnext").is_err());
        assert!(validate_run_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn mark_chatgpt_url_and_window_name_use_run_id() {
        assert_eq!(
            mark_chatgpt_url("20260417T071228Z_ab12cd"),
            "https://chatgpt.com/?_yoetz=20260417T071228Z_ab12cd"
        );
        assert_eq!(
            mark_chatgpt_url("run:abc.123"),
            "https://chatgpt.com/?_yoetz=run%3Aabc.123"
        );
        let script = build_set_window_name_js("20260417T071228Z_ab12cd");
        assert!(script.contains(r#"window.name = "yoetz:20260417T071228Z_ab12cd""#));
    }

    fn fake_chatgpt_fixture_html() -> &'static str {
        r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Fake ChatGPT Fixture</title>
    <style>
      body { font-family: sans-serif; }
      .file-tile { border: 1px solid #ccc; padding: 8px; margin-top: 8px; }
    </style>
  </head>
  <body>
    <main>
      <form id="chat-form">
        <div id="prompt-textarea" role="textbox" aria-label="Chat with ChatGPT" contenteditable="true">Review this bundle.</div>
        <button type="button" data-testid="send-button" aria-label="Send prompt">Send</button>
        <button type="button" data-testid="composer-plus-btn" aria-label="Attach files">Attach</button>
        <input id="fixture-upload" type="file" multiple />
        <div class="file-tile" data-testid="attachment-item" aria-busy="true">
          <span class="name">fixture-bundle.txt</span>
          <span class="status">Uploading</span>
          <div role="progressbar">Uploading…</div>
        </div>
      </form>
      <section id="transcript"></section>
    </main>
    <script>
      const transcript = document.getElementById("transcript");
      const sendButton = document.querySelector("[data-testid='send-button']");
      const appendAssistantMessage = () => {
        const message = document.createElement("div");
        message.setAttribute("data-message-author-role", "assistant");
        message.textContent = "Fixture assistant response";
        const copy = document.createElement("button");
        copy.setAttribute("aria-label", "Copy");
        copy.textContent = "Copy";
        message.appendChild(copy);
        transcript.appendChild(message);
      };
      sendButton.addEventListener("click", () => {
        sendButton.disabled = true;
        const stop = document.createElement("button");
        stop.type = "button";
        stop.setAttribute("data-testid", "stop-button");
        stop.setAttribute("aria-label", "Stop generating");
        stop.textContent = "Stop";
        stop.id = "fixture-stop-button";
        document.body.appendChild(stop);

        const thinking = document.createElement("div");
        thinking.className = "result-thinking";
        thinking.id = "fixture-thinking";
        thinking.textContent = "Thinking…";
        document.body.appendChild(thinking);

        setTimeout(() => {
          appendAssistantMessage();
          document.getElementById("fixture-stop-button")?.remove();
          document.getElementById("fixture-thinking")?.remove();
          sendButton.disabled = false;
        }, 150);
      });

      window.fixtureMarkUploadDone = () => {
        const tile = document.querySelector(".file-tile");
        tile.removeAttribute("aria-busy");
        tile.querySelector("[role='progressbar']").style.display = "none";
        tile.querySelector(".status").textContent = "Ready";
      };
    </script>
  </body>
</html>
"#
    }

    fn reserve_loopback_debug_port() -> anyhow::Result<u16> {
        // Intentionally drop the listener immediately so Chrome can rebind it.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        Ok(listener.local_addr()?.port())
    }

    fn should_disable_fixture_sandbox(is_linux: bool, is_ci: bool) -> bool {
        is_linux && is_ci
    }

    fn launch_fake_chatgpt_fixture() -> anyhow::Result<(Browser, Arc<Tab>)> {
        let mut builder = LaunchOptionsBuilder::default();
        builder.headless(true);
        builder.port(Some(reserve_loopback_debug_port()?));
        if should_disable_fixture_sandbox(
            cfg!(target_os = "linux"),
            std::env::var_os("CI").is_some() || std::env::var_os("GITHUB_ACTIONS").is_some(),
        ) {
            // Chrome for Testing on hosted Linux runners can fail before
            // exposing its DevTools websocket unless the sandbox is disabled.
            builder.sandbox(false);
        }
        if let Some(path) = std::env::var_os("YOETZ_CHROME_BIN") {
            builder.path(Some(PathBuf::from(path)));
        }
        let browser = Browser::new(builder.build()?)?;
        let tab = browser.new_tab()?;
        let html = fake_chatgpt_fixture_html();
        let encoded = base64::engine::general_purpose::STANDARD.encode(html);
        tab.navigate_to(&format!("data:text/html;base64,{encoded}"))?;
        tab.wait_until_navigated()?;
        tab.wait_for_element("#prompt-textarea")?;
        Ok((browser, tab))
    }

    fn eval_fixture_function(tab: &Arc<Tab>, function_source: &str) -> anyhow::Result<Value> {
        let expression = wrap_function_source_for_json_eval(function_source)?;
        let result = tab.evaluate(&expression, true)?;
        let json = result
            .value
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| anyhow!("fixture eval did not return a JSON string"))?;
        Ok(serde_json::from_str(&json)?)
    }

    fn set_fixture_input_file(tab: &Arc<Tab>, file_name: &str) -> anyhow::Result<()> {
        let function_source = format!(
            r##"() => {{
  const input = document.querySelector("#fixture-upload");
  const dt = new DataTransfer();
  dt.items.add(new File(["fixture"], {file_name:?}, {{ type: "text/plain" }}));
  input.files = dt.files;
  return Array.from(input.files || []).map((file) => file.name);
}}"##
        );
        let names = eval_fixture_function(tab, &function_source)?;
        assert_eq!(names, serde_json::json!([file_name]));
        Ok(())
    }

    #[test]
    #[ignore = "requires Chrome"]
    #[serial]
    fn fake_chatgpt_fixture_upload_probe_tracks_uploading_then_ready() -> anyhow::Result<()> {
        let (_browser, tab) = launch_fake_chatgpt_fixture()?;
        set_fixture_input_file(&tab, "fixture-bundle.txt")?;

        let uploading = eval_fixture_function(
            &tab,
            &build_attachment_probe_function("fixture-bundle.txt")?,
        )?;
        assert_eq!(uploading["status"], "uploading");
        assert_eq!(uploading["ok"], false);
        assert_eq!(uploading["inputMatched"], true);
        assert_eq!(uploading["composerScoped"], true);

        tab.evaluate("window.fixtureMarkUploadDone()", true)?;

        let ready = eval_fixture_function(
            &tab,
            &build_attachment_probe_function("fixture-bundle.txt")?,
        )?;
        assert_eq!(ready["status"], "done");
        assert_eq!(ready["ok"], true);
        assert_eq!(ready["inputMatched"], true);
        assert_eq!(ready["composerScoped"], true);
        Ok(())
    }

    #[test]
    #[ignore = "requires Chrome"]
    #[serial]
    fn fake_chatgpt_fixture_send_and_poll_probes_follow_scripted_dom() -> anyhow::Result<()> {
        let (_browser, tab) = launch_fake_chatgpt_fixture()?;

        let sent = eval_fixture_function(&tab, &build_send_button_click_function())?;
        assert_eq!(sent["status"], "sent");
        assert_eq!(sent["assistantCountBeforeSend"], 0);

        thread::sleep(Duration::from_millis(60));
        let during = eval_fixture_function(&tab, &build_chatgpt_dom_probe_function())?;
        let during_raw = during.as_str().expect("dom probe string");
        assert!(during_raw.contains("send=disabled"));
        assert!(during_raw.contains("stop=1"));
        assert!(during_raw.contains("thinking=1"));

        thread::sleep(Duration::from_millis(180));
        let finished = eval_fixture_function(&tab, &build_chatgpt_dom_probe_function())?;
        let finished_raw = finished.as_str().expect("dom probe string");
        assert!(finished_raw.contains("send=enabled"));
        assert!(finished_raw.contains("stop=0"));
        assert!(finished_raw.contains("thinking=0"));
        assert!(finished_raw.contains("copy=1"));
        assert!(finished_raw.contains("msgs=1"));

        let response = eval_fixture_function(&tab, &build_latest_response_probe_function())?;
        assert_eq!(response["response"], "Fixture assistant response");
        Ok(())
    }

    #[test]
    #[ignore = "requires Chrome"]
    #[serial]
    fn fake_chatgpt_fixture_model_selection_and_send_preserve_surface_evidence(
    ) -> anyhow::Result<()> {
        let (_browser, tab) = launch_fake_chatgpt_fixture()?;
        tab.evaluate(
            r#"(() => {
              const group = document.createElement("div");
              group.setAttribute("role", "radiogroup");
              group.setAttribute("aria-label", "Select chat surface");
              group.style.cssText = "width: 200px; height: 36px";
              for (const [value, checked] of [["chatgpt", "true"], ["work", "false"]]) {
                const radio = document.createElement("button");
                radio.setAttribute("role", "radio");
                radio.setAttribute("data-tpp-toggle-value", value);
                radio.setAttribute("aria-checked", checked);
                radio.style.cssText = "width: 100px; height: 20px";
                radio.textContent = value;
                group.appendChild(radio);
              }
              document.body.appendChild(group);
              return "mounted";
            })()"#,
            true,
        )?;

        let selection = eval_fixture_function(
            &tab,
            &build_model_selection_function("gpt-5-6-sol-chat-pro", ChatgptModelStrategy::Select),
        )?;
        assert_eq!(selection["status"], "missing-selector");
        assert_eq!(selection["surfaceEvidenceSeen"], true);

        let sent = eval_fixture_function(
            &tab,
            &build_send_button_click_function_with_surface_evidence(false),
        )?;
        assert_eq!(sent["status"], "sent");
        assert_eq!(sent["surfaceEvidenceSeen"], true);

        tab.evaluate(
            "document.querySelector('[role=\"radiogroup\"][aria-label=\"Select chat surface\"]')?.remove()",
            true,
        )?;
        let blocked_after_hide = eval_fixture_function(
            &tab,
            &build_send_button_click_function_with_surface_evidence(true),
        )?;
        assert_eq!(blocked_after_hide["status"], "surface-not-ready");
        assert_eq!(
            tab.evaluate(
                "document.querySelector('[data-testid=\"stop-button\"]') !== null",
                true
            )?
            .value
            .and_then(|value| value.as_bool()),
            Some(false)
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires Chrome"]
    #[serial]
    fn fake_chatgpt_fixture_selects_personal_picker_pro() -> anyhow::Result<()> {
        let (_browser, tab) = launch_fake_chatgpt_fixture()?;
        tab.evaluate(
            r##"(() => {
              const form = document.querySelector("#chat-form");
              const pill = document.createElement("button");
              pill.className = "__composer-pill";
              pill.setAttribute("aria-haspopup", "menu");
              pill.textContent = "High";
              let picker = null;
              let familyMenu = null;
              let effortMenu = null;
              const remove = (node) => {
                if (!node) return;
                node.remove();
                if (node === familyMenu) familyMenu = null;
                if (node === effortMenu) effortMenu = null;
              };
              const close = () => {
                remove(familyMenu);
                remove(effortMenu);
                remove(picker);
                pill.setAttribute("aria-expanded", "false");
                pill.setAttribute("data-state", "closed");
              };
              const openFamily = () => {
                remove(familyMenu);
                familyMenu = document.createElement("div");
                familyMenu.setAttribute("role", "menu");
                for (const [label, checked] of [["GPT-5.6 Sol", true], ["GPT-5.5", false]]) {
                  const item = document.createElement("div");
                  item.setAttribute("role", "menuitemradio");
                  item.setAttribute("aria-checked", String(checked));
                  item.textContent = label;
                  familyMenu.appendChild(item);
                }
                document.body.appendChild(familyMenu);
              };
              const openEffort = () => {
                remove(effortMenu);
                effortMenu = document.createElement("div");
                effortMenu.setAttribute("role", "menu");
                const pro = document.createElement("div");
                pro.setAttribute("role", "menuitemradio");
                pro.setAttribute("aria-checked", "false");
                pro.textContent = "Pro";
                pro.addEventListener("click", () => {
                  effort.textContent = "Effort Pro";
                  pill.textContent = "Pro";
                  remove(effortMenu);
                });
                effortMenu.appendChild(pro);
                document.body.appendChild(effortMenu);
              };
              const family = document.createElement("div");
              family.setAttribute("role", "menuitem");
              family.setAttribute("aria-haspopup", "menu");
              family.textContent = "Model GPT-5.6 Sol";
              family.addEventListener("pointerdown", openFamily);
              const effort = document.createElement("div");
              effort.setAttribute("role", "menuitem");
              effort.textContent = "Effort High";
              effort.addEventListener("pointerdown", openEffort);
              pill.addEventListener("pointerdown", () => {
                picker = document.createElement("div");
                picker.setAttribute("role", "dialog");
                picker.textContent = "Faster Smarter Model GPT-5.6 Sol Effort High Speed Standard";
                picker.append(family, effort);
                document.body.appendChild(picker);
                pill.setAttribute("aria-expanded", "true");
                pill.setAttribute("data-state", "open");
              });
              pill.addEventListener("keydown", (event) => {
                if (event.key === "Escape") close();
              });
              form.appendChild(pill);
              return "mounted";
            })()"##,
            true,
        )?;

        let selection = eval_fixture_function(
            &tab,
            &build_model_selection_function("gpt-5-6-sol-chat-pro", ChatgptModelStrategy::Select),
        )?;
        assert_eq!(selection["status"], "selected", "{selection}");
        assert_eq!(selection["modelUsed"], "GPT-5.6 Sol Pro");
        assert_eq!(selection["pickerShape"], "personal");
        assert_eq!(selection["familyStatus"], "verified");
        assert_eq!(selection["effortStatus"], "verified");
        Ok(())
    }

    #[test]
    fn fixture_sandbox_policy_only_disables_on_linux_ci() {
        assert!(should_disable_fixture_sandbox(true, true));
        assert!(!should_disable_fixture_sandbox(true, false));
        assert!(!should_disable_fixture_sandbox(false, true));
        assert!(!should_disable_fixture_sandbox(false, false));
    }
}

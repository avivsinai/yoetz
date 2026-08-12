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
  const isVisible = (el) => {
    if (!el) return false;
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return rect.width > 0 &&
      rect.height > 0 &&
      style.visibility !== "hidden" &&
      style.display !== "none";
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
        || is_verified_sol_extra_high_selection(selection, requested_model)
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
        "selected" if is_verified_sol_extra_high_selection(selection, requested_model) => {
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

fn is_verified_sol_extra_high_selection(
    selection: &serde_json::Value,
    requested_model: &str,
) -> bool {
    if selection.get("status").and_then(serde_json::Value::as_str) != Some("selected")
        || requested_model.trim() != crate::chatgpt_recipe::CHATGPT_SOL_EXTRA_HIGH_MODEL
        || selection
            .get("requested")
            .and_then(serde_json::Value::as_str)
            != Some(crate::chatgpt_recipe::CHATGPT_SOL_EXTRA_HIGH_MODEL)
        || selection
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
        "gpt-5.6 sol pro" | "gpt-5.6 sol extra high"
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
  const supported = "gpt-5-6-sol-extra-high";
  const MODEL_BUTTON_SELECTOR = {model_button_selector};
  const COMPOSER_SELECTOR = {composer_selector};
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const fold = (value) => normalize(value).toLowerCase();
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const textOf = (node) => normalize(node?.innerText || node?.textContent || "");
{visibility_helpers}

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

  function summaryMatches(value) {{
    const valueFolded = fold(value);
    return /^(instant|medium|high|extra high|pro)$/.test(valueFolded)
      || /^\d+(?:\.\d+)+ (instant|medium|high|extra high|pro)$/.test(valueFolded)
      || /\bgpt[\s.-]*\d/.test(valueFolded);
  }}

  function findPill() {{
    for (const scope of composerScopes()) {{
      const buttons = Array.from(scope.querySelectorAll(MODEL_BUTTON_SELECTOR)).filter(isVisible);
      const exact = buttons.find((button) => button.classList.contains("__composer-pill"));
      if (exact) return exact;
      const fallback = buttons.find((button) => summaryMatches(textOf(button)));
      if (fallback) return fallback;
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
    return Array.from(document.querySelectorAll("[role='menu']")).filter(isVisible);
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
        && (labels.includes("pro") || labels.includes("pro extended"));
    }}) || null;
  }}

  function familyMenu(main) {{
    return visibleMenus().find((menu) => menu !== main
      && radios(menu).some((item) => fold(textOf(item)) === "gpt-5.6 sol")) || null;
  }}

  function readState(menu) {{
    const effortItems = radios(menu);
    const familyTrigger = Array.from(menu?.querySelectorAll?.("[role='menuitem']") || [])
      .find((item) => item.getAttribute("aria-haspopup") === "menu" && /^(?:gpt|o\d)\b/i.test(textOf(item))) || null;
    return {{ menu, effortItems, familyTrigger, familyLabel: textOf(familyTrigger) }};
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
      if (mode === "main" ? mainMenu() : familyMenu(main)) return true;
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

  async function closeMenus(pill) {{
    for (let attempt = 0; attempt < 3 && visibleMenus().length > 0; attempt += 1) {{
      keyPress(pill, "Escape", "Escape");
      await wait(100);
    }}
    return visibleMenus().length === 0;
  }}

  function familyVerified(state) {{
    return fold(state?.familyLabel) === "gpt-5.6 sol";
  }}

  function effortVerified(state) {{
    return state?.effortItems?.some((item) => ["pro", "extra high"].includes(fold(textOf(item))) && isChecked(item)) || false;
  }}

  function result(status, pill, state, families, warning = null) {{
    const familyIsVerified = familyVerified(state);
    const effortIsVerified = effortVerified(state);
    const verifiedEffort = state?.effortItems?.find((item) => ["pro", "extra high"].includes(fold(textOf(item))) && isChecked(item));
    const modelUsed = status === "current"
      ? (pill ? textOf(pill) : "")
      : (familyIsVerified && effortIsVerified ? `GPT-5.6 Sol ${{textOf(verifiedEffort)}}` : null);
    return {{
      requested,
      status,
      modelUsed,
      familyStatus: familyIsVerified ? "verified" : "unverified",
      effortStatus: effortIsVerified ? "verified" : "unverified",
      pillText: textOf(pill),
      familyLabel: state?.familyLabel || null,
      availableItems: (state?.effortItems || []).map(textOf).filter(Boolean),
      availableFamilies: families || [],
      warning,
      url: window.location.href || "",
      title: document.title || ""
    }};
  }}

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
      url: window.location.href || "",
      title: document.title || ""
    }};
  }}

  if (requested !== supported) {{
    return result("not-found", null, null, [], "this recipe supports only GPT-5.6 Sol at the account maximum tier (Pro or Extra High)");
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
  if (!state) return result("not-found", pill, null, families, "ChatGPT GPT-5.6 model picker did not open");

  if (!familyVerified(state)) {{
    const submenu = await openFamilyMenu(state);
    const familyItems = radios(submenu);
    families = familyItems.map(textOf).filter(Boolean);
    const sol = familyItems.find((item) => fold(textOf(item)) === "gpt-5.6 sol") || null;
    if (!sol) {{
      await closeMenus(pill);
      return result("not-found", pill, state, families, "GPT-5.6 Sol was not visible in the family submenu");
    }}
    realClick(sol);
    await wait(250);
    pill = await waitForPill();
    if (!pill) return result("selection-mismatch", null, null, families, "ChatGPT composer model pill did not remount after selecting GPT-5.6 Sol");
    menu = await openMain(pill);
    state = menu ? readState(menu) : null;
    if (!state) return result("selection-mismatch", pill, null, families, "picker did not reopen after selecting GPT-5.6 Sol");
  }}

  if (!effortVerified(state)) {{
    const maxTier = state.effortItems.find((item) => fold(textOf(item)) === "extra high")
      || state.effortItems.find((item) => fold(textOf(item)) === "pro") || null;
    if (!maxTier) {{
      await closeMenus(pill);
      return result("not-found", pill, state, families, "Neither Pro nor Extra High was visible as the GPT-5.6 Sol maximum tier");
    }}
    realClick(maxTier);
    await wait(250);
    pill = await waitForPill();
    if (!pill) return result("selection-mismatch", null, null, families, "ChatGPT composer model pill did not remount after selecting the maximum effort tier");
    menu = await openMain(pill);
    state = menu ? readState(menu) : null;
    if (!state) return result("selection-mismatch", pill, null, families, "picker did not reopen after selecting the maximum effort tier");
  }}

  const familyIsVerified = familyVerified(state);
  const effortIsVerified = effortVerified(state);
  if (!familyIsVerified || !effortIsVerified) {{
    await closeMenus(pill);
    return result("selection-mismatch", pill, state, families, "GPT-5.6 Sol at a verified maximum tier (Pro or Extra High) could not be confirmed in one picker pass");
  }}
  if (!await closeMenus(pill)) {{
    return result("selection-mismatch", pill, state, families, "ChatGPT model picker remained open after verification");
  }}
  return result("selected", pill, state, families);
}}
"##,
        requested_model = requested_model,
        model_button_selector = model_button_selector,
        composer_selector = composer_selector,
        visibility_helpers = JS_VISIBILITY_HELPERS,
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

pub fn build_send_button_click_function() -> String {
    let send_button_selector_json = send_button_selector_json();
    let composer_selector_json = composer_selector_json();
    let attachment_tile_selector_json = attachment_tile_selector_json();
    format!(
        r##"
() => {{
  const SEND_BUTTON_SELECTOR = {send_button_selector_json};
    const COMPOSER_SELECTOR = {composer_selector_json};
    const ATTACHMENT_TILE_SELECTOR = {attachment_tile_selector_json};
    const clip = (value, max = 120) => String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
{visibility_helpers}
{composer_scope_helpers}
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
  if (!enabledEntry) {{
    return {{
      status: "not-ready",
      diagnostics,
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
  }};
}}
"##,
        send_button_selector_json = send_button_selector_json,
        composer_selector_json = composer_selector_json,
        attachment_tile_selector_json = attachment_tile_selector_json,
        visibility_helpers = JS_VISIBILITY_HELPERS,
        composer_scope_helpers = JS_COMPOSER_SCOPE_HELPERS,
    )
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
    fn reported_chatgpt_model_requires_verified_sol_and_extra_high_proofs() {
        let selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-extra-high",
            "modelUsed": "GPT-5.6 Sol Extra High",
            "familyStatus": "verified",
            "effortStatus": "verified"
        });
        assert_eq!(
            select_reported_chatgpt_model(&selection, "gpt-5-6-sol-extra-high"),
            Some("GPT-5.6 Sol Extra High".to_string())
        );

        let enterprise_selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-extra-high",
            "modelUsed": "GPT-5.6 Sol Pro",
            "familyStatus": "verified",
            "effortStatus": "verified"
        });
        assert_eq!(
            select_reported_chatgpt_model(&enterprise_selection, "gpt-5-6-sol-extra-high"),
            Some("GPT-5.6 Sol Pro".to_string())
        );

        let unknown_selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-extra-high",
            "modelUsed": "GPT-5.6 Sol Expert",
            "familyStatus": "verified",
            "effortStatus": "verified"
        });
        assert_eq!(
            select_reported_chatgpt_model(&unknown_selection, "gpt-5-6-sol-extra-high"),
            None
        );
    }

    #[test]
    fn reported_chatgpt_model_never_echoes_an_unverified_request() {
        let selection = serde_json::json!({
            "status": "selected",
            "requested": "gpt-5-6-sol-extra-high",
            "modelUsed": "Pro Extended",
            "extendedStatus": "required"
        });
        assert_eq!(
            select_reported_chatgpt_model(&selection, "gpt-5-6-sol-extra-high"),
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
                    "requested": "gpt-5-6-sol-extra-high",
                    "modelUsed": "GPT-5.6 Sol Extra High",
                    "familyStatus": "verified",
                    "effortStatus": "verified"
                }),
                "gpt-5-6-sol-extra-high"
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
                "gpt-5-6-sol-extra-high"
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
                "gpt-5-6-sol-extra-high"
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
    fn model_selection_function_requires_verified_sol_family_and_known_maximum_effort() {
        let script =
            build_model_selection_function("gpt-5-6-sol-extra-high", ChatgptModelStrategy::Select);
        assert!(script.contains(r#"const requested = "gpt-5-6-sol-extra-high";"#));
        assert!(script.contains("classList.contains(\"__composer-pill\")"));
        assert!(script.contains(
            "legacy ChatGPT picker detected; this yoetz version requires the GPT-5.6 UI"
        ));
        assert!(script.contains(r#"familyStatus: familyIsVerified ? "verified" : "unverified""#));
        assert!(script.contains(r#"effortStatus: effortIsVerified ? "verified" : "unverified""#));
        assert!(script.contains(r#"fold(textOf(item)) === "gpt-5.6 sol""#));
        assert!(script.contains(r#"["pro", "extra high"].includes(fold(textOf(item)))"#));
        assert!(script.contains(r#"/^(?:gpt|o\d)\b/i.test(textOf(item))"#));
        assert!(script.contains("await openFamilyMenu"));
        assert!(script.contains("async function waitForPill()"));
        assert!(script.contains("pill = await waitForPill();"));
        assert!(script
            .contains("ChatGPT composer model pill did not remount after selecting GPT-5.6 Sol"));
        assert!(script.contains(
            "ChatGPT composer model pill did not remount after selecting the maximum effort tier"
        ));
        assert!(script.contains("return visibleMenus().length === 0;"));
        assert!(script.contains("if (!await closeMenus(pill))"));
        assert!(!script.contains("if (families.length === 0 && state.familyTrigger)"));
        assert!(script.contains("await closeMenus"));
        assert!(!script.contains("model-switcher-gpt-5-4"));
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
        <div id="prompt-textarea" role="textbox" contenteditable="true">Review this bundle.</div>
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
    fn fixture_sandbox_policy_only_disables_on_linux_ci() {
        assert!(should_disable_fixture_sandbox(true, true));
        assert!(!should_disable_fixture_sandbox(true, false));
        assert!(!should_disable_fixture_sandbox(false, true));
        assert!(!should_disable_fixture_sandbox(false, false));
    }
}

//! Shared ChatGPT web contracts and DOM-script builders used by browser transports.

use anyhow::{anyhow, Result};
use rand::random;
use std::collections::BTreeMap;
use std::path::Path;
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

use crate::chatgpt_recipe::ChatgptModelSelectionStatus;

pub const CHATGPT_URL: &str = "https://chatgpt.com/";
pub const COMPOSER_SELECTOR: &str =
    "#prompt-textarea, div[contenteditable='true'][role='textbox'], [role='textbox']";
pub const MODEL_SELECTOR_BUTTON_SELECTOR: &str = "[data-testid='model-switcher-dropdown-button'], button[aria-label='Model selector'], button[aria-label='Model selector menu'], button:has([data-testid='selected-model']), button:has([data-testid='model-switcher-selected-model'])";
pub const MODEL_ITEM_SELECTOR: &str = "[role='menuitem'], [role='menuitemradio'], [data-testid^='model-switcher-']:not([data-testid='model-switcher-selected-model'])";
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
    "model-switcher-dropdown-button",
    "model-switcher-selected-model",
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

pub fn model_testid_alias_map() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("extended-pro", "extended-pro"),
        ("pro", "gpt-5-4-pro"),
        ("gpt-5-pro", "gpt-5-4-pro"),
        ("thinking", "gpt-5-4-thinking"),
        ("gpt-5-thinking", "gpt-5-4-thinking"),
        ("instant", "gpt-5-3"),
    ])
}

pub fn model_testid_candidate_map() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        ("extended-pro", vec!["extended-pro"]),
        ("pro", vec!["gpt-5-4-pro"]),
        ("gpt-5-pro", vec!["gpt-5-4-pro"]),
        ("gpt-5-4-pro", vec!["gpt-5-4-pro"]),
        ("thinking", vec!["gpt-5-4-thinking"]),
        ("gpt-5-thinking", vec!["gpt-5-4-thinking"]),
        ("gpt-5-4-thinking", vec!["gpt-5-4-thinking"]),
        ("instant", vec!["gpt-5-3"]),
        ("gpt-5-3", vec!["gpt-5-3"]),
    ])
}

fn normalize_chatgpt_model_key(value: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut last_was_separator = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !normalized.is_empty() && !last_was_separator {
            normalized.push('-');
            last_was_separator = true;
        }
    }
    let normalized = normalized.trim_matches('-').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn is_low_signal_chatgpt_model_key(key: &str) -> bool {
    matches!(
        key,
        "chatgpt"
            | "chat-gpt"
            | "openai"
            | "configure"
            | "model-selector"
            | "selected-model"
            | "dropdown-button"
    )
}

pub fn canonical_chatgpt_model_slug(value: &str) -> Option<String> {
    let normalized = normalize_chatgpt_model_key(value)?;
    if let Some(stripped) = normalized.strip_prefix("model-switcher-") {
        return canonical_chatgpt_model_slug(stripped);
    }
    if let Some(candidates) = model_testid_candidate_map().get(normalized.as_str()) {
        return candidates.first().map(|value| (*value).to_string());
    }
    if let Some(alias) = model_testid_alias_map().get(normalized.as_str()) {
        return Some((*alias).to_string());
    }
    if is_low_signal_chatgpt_model_key(&normalized) {
        return None;
    }
    Some(normalized)
}

pub fn select_reported_chatgpt_model(
    selection: &serde_json::Value,
    requested_model: &str,
) -> Option<String> {
    for field in ["targetTestId", "modelUsed", "selectedLabel", "currentLabel"] {
        if let Some(model) = selection
            .get(field)
            .and_then(serde_json::Value::as_str)
            .and_then(canonical_chatgpt_model_slug)
        {
            return Some(model);
        }
    }

    let requested_model = requested_model.trim();
    if should_keep_current_chatgpt_model(requested_model) {
        None
    } else {
        canonical_chatgpt_model_slug(requested_model)
    }
}

pub fn should_keep_current_chatgpt_model(requested_model: &str) -> bool {
    let requested_model = requested_model.trim();
    requested_model.is_empty()
        || requested_model.eq_ignore_ascii_case("current")
        || requested_model.eq_ignore_ascii_case("keep-current")
}

pub(crate) fn chatgpt_model_selection_status(
    selection: &serde_json::Value,
    requested_model: &str,
) -> ChatgptModelSelectionStatus {
    let status = selection
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let keep_current_model = should_keep_current_chatgpt_model(requested_model);
    match status {
        "selected" => ChatgptModelSelectionStatus::Selected,
        "already-selected" if keep_current_model => ChatgptModelSelectionStatus::KeptCurrent,
        "already-selected" => ChatgptModelSelectionStatus::Selected,
        "missing-selector" | "not-found" => ChatgptModelSelectionStatus::Unavailable,
        "selection-mismatch" => ChatgptModelSelectionStatus::Mismatch,
        _ => ChatgptModelSelectionStatus::Unavailable,
    }
}

pub fn model_selector_button_selector_json() -> String {
    serde_json::to_string(MODEL_SELECTOR_BUTTON_SELECTOR)
        .expect("serialize model selector selector")
}

pub fn composer_selector_json() -> String {
    serde_json::to_string(COMPOSER_SELECTOR).expect("serialize composer selector")
}

pub fn model_item_selector_json() -> String {
    serde_json::to_string(MODEL_ITEM_SELECTOR).expect("serialize model item selector")
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

pub fn model_testid_aliases_json() -> String {
    serde_json::to_string(&model_testid_alias_map()).expect("serialize model alias map")
}

pub fn model_testid_candidates_json() -> String {
    serde_json::to_string(&model_testid_candidate_map()).expect("serialize model candidate map")
}

pub fn build_model_selection_function(requested_model: &str) -> String {
    let requested_model =
        serde_json::to_string(requested_model).expect("serialize requested model");
    let model_button_selector = model_selector_button_selector_json();
    let composer_selector = composer_selector_json();
    let model_item_selector = model_item_selector_json();
    let model_testid_aliases = model_testid_aliases_json();
    let model_testid_candidates = model_testid_candidates_json();
    format!(
        r##"
async () => {{
  const requested = {requested_model};
  const MODEL_BUTTON_SELECTOR = {model_button_selector};
  const COMPOSER_SELECTOR = {composer_selector};
  const MODEL_ITEM_SELECTOR = {model_item_selector};
  const MODEL_TESTID_ALIASES = {model_testid_aliases};
  const MODEL_TESTID_CANDIDATES = {model_testid_candidates};
  const normalize = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const isCheckedState = (ariaChecked, dataState) => ariaChecked === "true" || dataState === "checked";
  const hasSelectedCurrentMarker = (ariaSelected, ariaCurrent, dataSelected, dataCurrent) =>
    ariaSelected === "true" ||
    dataSelected === "true" ||
    (!!ariaCurrent && ariaCurrent !== "false") ||
    (!!dataCurrent && dataCurrent !== "false");
    const itemIsSelected = (item) => !!item && (
      isCheckedState(item.ariaChecked || "", item.dataState || "") ||
      hasSelectedCurrentMarker(item.ariaSelected || "", item.ariaCurrent || "", item.dataSelected || "", item.dataCurrent || "")
    );
{visibility_helpers}
  const composerScopes = () => {{
    const composer = document.querySelector(COMPOSER_SELECTOR);
    const scopes = [];
    const add = (scope) => {{
      if (scope && !scopes.includes(scope)) scopes.push(scope);
    }};
    add(composer?.closest("form"));
    add(composer?.closest("[data-testid*='composer'], [class*='composer'], main, [role='main']"));
    add(composer?.parentElement);
    return scopes;
  }};
  const modelCandidateText = (node, target = node) => normalize([
    node?.getAttribute?.("aria-label"),
    node?.getAttribute?.("title"),
    node?.getAttribute?.("data-testid"),
    node?.getAttribute?.("class"),
    node?.innerText,
    node?.textContent,
    target?.getAttribute?.("aria-label"),
    target?.getAttribute?.("title"),
    target?.getAttribute?.("data-testid"),
    target?.getAttribute?.("class"),
    target?.innerText,
    target?.textContent,
  ].filter(Boolean).join(" ")).toLowerCase();
  const isModelActionableElement = (node) => {{
    const tag = String(node?.tagName || "").toLowerCase();
    const role = normalize(node?.getAttribute?.("role") || "").toLowerCase();
    return tag === "button" ||
      role === "button" ||
      node?.getAttribute?.("aria-haspopup") !== null ||
      node?.getAttribute?.("tabindex") !== null;
  }};
  const isModelChipLike = (node) => {{
    const marker = normalize([
      node?.getAttribute?.("data-testid"),
      node?.getAttribute?.("class"),
      node?.getAttribute?.("aria-label"),
      node?.getAttribute?.("title"),
    ].filter(Boolean).join(" ")).toLowerCase();
    return /\b(model|model-switcher)\b/.test(marker) && /\b(chip|pill|token|button|menu|dropdown|switcher)\b/.test(marker);
  }};
  const modelClickTarget = (node, stopAt) => {{
    for (let current = node; current; current = current.parentElement) {{
      if (isModelActionableElement(current) || isModelChipLike(current)) return current;
      if (current === stopAt) return null;
    }}
    return null;
  }};
  const looksLikeModelControl = (text) =>
    /\bextended\s+pro\b/.test(text) ||
    /\bgpt[\s.-]*\d/.test(text) ||
    /\b(pro|instant|thinking|model)\b/.test(text);
  const findComposerModelControl = () => {{
    for (const scope of composerScopes()) {{
      const candidates = Array.from(scope.querySelectorAll("button, [role='button'], [aria-haspopup], [tabindex], [aria-label], [title], [data-testid], span, div"));
      for (const node of candidates) {{
        const target = modelClickTarget(node, scope);
        if (!target || !isVisible(target)) continue;
        const haystack = modelCandidateText(node, target);
        if (!looksLikeModelControl(haystack)) continue;
        if (/\b(send|stop|copy|share|new chat|attach|upload|search|history|dictation|voice|microphone|account|profile|settings|upgrade)\b/.test(haystack)) continue;
        return target;
      }}
    }}
    return null;
  }};
    const slugify = (value) => normalize(value).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  const isLowSignalSelectorLabel = (value) => {{
    const key = slugify(value);
    return !key || key === "chatgpt" || key === "chat-gpt" || key === "openai" || key === "model-selector" || key === "selected-model";
  }};
  const selectorLabelMatchesTarget = (label, item, tierSlug, targetTestId, candidateTestIds) => {{
    const labelText = normalize(label).toLowerCase();
    const labelSlug = slugify(labelText);
    if (isLowSignalSelectorLabel(labelText)) return false;
    const candidates = [];
    const add = (value) => {{
      const raw = normalize(value || "");
      const key = slugify(raw);
      if (!key || isLowSignalSelectorLabel(raw)) return;
      candidates.push({{ raw: raw.toLowerCase(), key }});
    }};
    add(tierSlug);
    add(item?.text || "");
    add((item?.testId || "").replace(/^model-switcher-/, ""));
    add((targetTestId || "").replace(/^model-switcher-/, ""));
    (candidateTestIds || []).forEach((value) => add(value));
    return candidates.some((candidate) => {{
      if (candidate.key === labelSlug) return true;
      if (tierSlug && candidate.key === slugify(tierSlug)) {{
        const escaped = candidate.raw.replace(/[.*+?^$()|[\]\\]/g, "\\$&");
        return new RegExp(`(^|[^a-z0-9])${{escaped}}([^a-z0-9]|$)`).test(labelText);
      }}
      return candidate.key.length >= 4 && labelSlug.includes(candidate.key);
    }});
  }};
  const realClick = (el) => {{
    el.dispatchEvent(new PointerEvent("pointerdown", {{ bubbles: true, cancelable: true, pointerId: 1 }}));
    el.dispatchEvent(new MouseEvent("mousedown", {{ bubbles: true, cancelable: true }}));
    el.dispatchEvent(new PointerEvent("pointerup", {{ bubbles: true, cancelable: true, pointerId: 1 }}));
    el.dispatchEvent(new MouseEvent("mouseup", {{ bubbles: true, cancelable: true }}));
    el.dispatchEvent(new MouseEvent("click", {{ bubbles: true, cancelable: true }}));
  }};
  const keyPress = (el, key, code) => {{
    el.dispatchEvent(new KeyboardEvent("keydown", {{ key, code, bubbles: true, cancelable: true }}));
    el.dispatchEvent(new KeyboardEvent("keyup", {{ key, code, bubbles: true, cancelable: true }}));
  }};
  const isVersionedModelTestId = (value) => /^model-switcher-gpt-\d/.test(normalize(value).toLowerCase());
  const findPreferredTestIdItem = (entries, orderedTestIds) => {{
    for (const testId of orderedTestIds) {{
      const match = entries.find((item) => item.testId.toLowerCase() === testId);
      if (match) return match;
    }}
    return null;
  }};
  const findGenericNeedleItem = (entries, needles) =>
    entries.find((item) =>
      !isVersionedModelTestId(item.testId) &&
      needles.some((needle) => item.haystack.includes(needle))
    ) || null;
  const hasExactTierLabel = (item, slug) => {{
    const text = normalize(item?.text || "").toLowerCase();
    return text === slug || text.startsWith(`${{slug}} `);
  }};
  const deriveRequestedTier = (value) => {{
    if (!value) return null;
    if (/\bthinking\b/.test(value)) return "thinking";
    if (/\binstant\b/.test(value)) return "instant";
    if (/\bpro\b/.test(value) && !/\b5[- .]?3\b/.test(value)) return "pro";
    return null;
  }};
  const hasTrustedUserFacingTierLabel = (item, slug) => {{
    const haystack = item?.haystack || "";
    if (slug === "pro") return /research-grade intelligence/.test(haystack);
    if (slug === "thinking") return /for complex questions/.test(haystack);
    if (slug === "instant") return /for everyday chats/.test(haystack);
    return false;
  }};
  const classifyTier = (item) => {{
    const haystack = item?.haystack || "";
    if (hasTrustedUserFacingTierLabel(item, "pro") || /\bpro\b/.test(haystack)) return "pro";
    if (hasTrustedUserFacingTierLabel(item, "thinking") || /thinking/.test(haystack)) return "thinking";
    if (hasTrustedUserFacingTierLabel(item, "instant") || /instant/.test(haystack)) return "instant";
    return null;
  }};
  const parseVersionParts = (item) => {{
    const haystack = item?.haystack || "";
    const match = haystack.match(/gpt[- ]?(\d+)(?:[- .]?(\d+))?/i);
    return match ? [Number.parseInt(match[1], 10) || 0, Number.parseInt(match[2] || "0", 10) || 0] : [0, 0];
  }};
  const compareVersionParts = (left, right) => (left[0] - right[0]) || (left[1] - right[1]);
  const maxVersionPartsForTier = (entries, tier) =>
    entries
      .filter((item) => classifyTier(item) === tier)
      .map((item) => parseVersionParts(item))
      .reduce((best, current) => compareVersionParts(current, best) > 0 ? current : best, [0, 0]);
  const effectiveVersionParts = (item, tier, tierMaxVersions) => {{
    const parsed = parseVersionParts(item);
    if (!tier) return parsed;
    if (hasTrustedUserFacingTierLabel(item, tier) && compareVersionParts(parsed, tierMaxVersions[tier] || [0, 0]) < 0) {{
      return tierMaxVersions[tier] || parsed;
    }}
    return parsed;
  }};
  const hasConflictingTierHint = (item, slug) => {{
    const haystack = item?.haystack || "";
    if (hasTrustedUserFacingTierLabel(item, slug) || hasExactTierLabel(item, slug)) return false;
    if (slug === "pro") {{
      return /\b5[- .]?3\b|gpt-5[- .]?3-pro/.test(haystack);
    }}
    if (slug === "thinking") {{
      return /\b5[- .]?3\b|instant/.test(haystack);
    }}
    if (slug === "instant") {{
      return /\bthinking\b|\bpro\b|gpt-5[- .]?4/.test(haystack);
    }}
    return false;
  }};
  const findSingleGenericTierItem = (entries, slug) => {{
    if (!(slug === "pro" || slug === "thinking" || slug === "instant")) return null;
    const matches = entries.filter((item) =>
      item.haystack.includes(slug) && !hasConflictingTierHint(item, slug)
    );
    return matches.length === 1 ? matches[0] : null;
  }};
  const findExactTierLabelItem = (entries, slug) => {{
    if (!(slug === "pro" || slug === "thinking" || slug === "instant")) return null;
    return entries.find((item) => hasExactTierLabel(item, slug) && !hasConflictingTierHint(item, slug)) || null;
  }};
  const modelSlug = (value) => {{
    let key = slugify(value);
    if (key.startsWith("model-switcher-")) key = key.slice("model-switcher-".length);
    if (!key || isLowSignalSelectorLabel(key)) return "";
    if (key === "extended-pro") return "extended-pro";
    return (MODEL_TESTID_CANDIDATES[key] || [MODEL_TESTID_ALIASES[key] || key])[0] || "";
  }};
  const currentLabelSatisfiesRequest = (label) => {{
    const labelText = normalize(label).toLowerCase();
    const labelSlug = modelSlug(labelText);
    if (!labelSlug) return false;
    const isProLabel = /\bextended\s+pro\b/.test(labelText) ||
      /\bgpt[\s.-]*\d+(?:[\s.-]*\d+)*[\s.-]*pro\b/.test(labelText) ||
      /\bpro\b/.test(labelText);
    if (autoMode) return isProLabel;
    if (requestedGenericTier === "pro") return isProLabel;
    if (requestedGenericTier === "thinking") return /\bthinking\b/.test(labelText);
    if (requestedGenericTier === "instant") return /\binstant\b/.test(labelText);
    const requestedKeys = Array.from(new Set(
      (MODEL_TESTID_CANDIDATES[requestedLower] || [MODEL_TESTID_ALIASES[requestedLower] || requestedLower])
        .map((value) => modelSlug(value))
        .filter(Boolean)
    ));
    return requestedKeys.includes(labelSlug);
  }};
  const selectorLabelConfirmsTextTarget = (label, item, tierSlug, needles) => {{
    if (!item || item.testId) return false;
    return selectorLabelMatchesTarget(label, item, tierSlug, null, needles || []);
  }};
  const buildTierRankings = (entries) => {{
    const tierMaxVersions = {{
      pro: maxVersionPartsForTier(entries, "pro"),
      thinking: maxVersionPartsForTier(entries, "thinking"),
      instant: maxVersionPartsForTier(entries, "instant"),
    }};
    const tierScore = (tier) => tier === "pro" ? 300 : tier === "thinking" ? 200 : tier === "instant" ? 100 : 0;
    const candidateScore = (item) => {{
      const tier = classifyTier(item);
      const version = effectiveVersionParts(item, tier, tierMaxVersions);
      const trusted = tier && hasTrustedUserFacingTierLabel(item, tier);
      return {{
        tier,
        version,
        trusted,
        score: tierScore(tier) * 1_000_000 + version[0] * 1_000 + version[1] + (trusted ? 1 : 0),
      }};
    }};
    return {{ tierMaxVersions, candidateScore }};
  }};
  const selectBestTierItem = (entries, slug, rankings) =>
    entries
      .map((item) => ({{ item, meta: rankings.candidateScore(item) }}))
      .filter(({{ item, meta }}) => meta.tier === slug && !hasConflictingTierHint(item, slug))
      .sort((left, right) =>
        right.meta.score - left.meta.score ||
        right.item.text.length - left.item.text.length
      )
      .map(({{ item }}) => item)[0] || null;
  const findSelectorButton = () => document.querySelector(MODEL_BUTTON_SELECTOR) || findComposerModelControl();
  const requestedTrimmed = normalize(requested);
  const requestedLower = requestedTrimmed.toLowerCase();
  const keepCurrentMode = !requestedTrimmed || requestedLower === "current" || requestedLower === "keep-current";
  const autoMode = requestedLower === "auto";
  const requestedGenericTier = deriveRequestedTier(requestedLower);
  const waitForSelectorButton = async () => {{
    let button = findSelectorButton();
    if (button || keepCurrentMode) return button;
    for (let attempt = 0; attempt < 5 && !button; attempt += 1) {{
      await wait(1000);
      button = findSelectorButton();
    }}
    return button;
  }};
  const selectorButton = await waitForSelectorButton();
  const currentLabel = normalize(
    selectorButton?.querySelector?.("[data-testid='selected-model'], [data-testid='model-switcher-selected-model']")?.textContent ||
    selectorButton?.innerText ||
    selectorButton?.textContent ||
    selectorButton?.getAttribute?.("aria-label") ||
    selectorButton?.getAttribute?.("title") ||
    ""
  );
  const responseBase = {{
    requested,
    currentLabel,
    url: window.location.href || "",
    title: document.title || "",
    bodyText: normalize(document.body?.innerText || "").slice(0, 240),
  }};

  if (keepCurrentMode) {{
    return {{
      ...responseBase,
      status: "already-selected",
      modelUsed: currentLabel || null,
      keepCurrent: true,
    }};
  }}

  if (currentLabelSatisfiesRequest(currentLabel)) {{
    return {{
      ...responseBase,
      status: "already-selected",
      modelUsed: currentLabel,
    }};
  }}

  if (!selectorButton) {{
    return {{
      ...responseBase,
      status: "missing-selector",
      modelUsed: currentLabel || null,
    }};
  }}

  const popupRoots = () => {{
    const selectorButton = findSelectorButton();
    const roots = [];
    const add = (el) => {{
      if (el && !roots.includes(el) && isVisible(el)) roots.push(el);
    }};
    const controlledId = selectorButton.getAttribute("aria-controls") || selectorButton.getAttribute("aria-owns");
    if (controlledId) {{
      add(document.getElementById(controlledId));
    }}
    Array.from(document.querySelectorAll("[role='menu'], [role='listbox'], [data-radix-popper-content-wrapper], [data-state='open']"))
      .filter((el) => isVisible(el))
      .forEach((el) => {{
        if (el.querySelector(MODEL_ITEM_SELECTOR)) add(el);
      }});
    return roots;
  }};

  const readItems = () => {{
    const selectorButton = findSelectorButton();
    const roots = popupRoots();
    let nodes = roots.flatMap((root) => Array.from(root.querySelectorAll(MODEL_ITEM_SELECTOR)));
    if (nodes.length === 0) {{
      nodes = Array.from(document.querySelectorAll(MODEL_ITEM_SELECTOR))
        .filter((el) => !(selectorButton && selectorButton.contains(el)) && isVisible(el));
    }}
    const seen = new Set();
    return nodes
      .filter((el) => isVisible(el))
      .map((el) => {{
        const text = normalize(el.innerText || el.textContent || el.getAttribute?.("aria-label") || el.getAttribute?.("title") || "");
        const testId = normalize(el.getAttribute?.("data-testid") || "");
        const haystack = `${{testId}} ${{text}}`.toLowerCase();
        const key = `${{testId}}|${{text}}`;
        if (seen.has(key)) return null;
        seen.add(key);
        return {{
          text,
          testId,
          haystack,
          ariaChecked: normalize(el.getAttribute?.("aria-checked") || "").toLowerCase(),
          dataState: normalize(el.getAttribute?.("data-state") || "").toLowerCase(),
          ariaSelected: normalize(el.getAttribute?.("aria-selected") || "").toLowerCase(),
          ariaCurrent: normalize(el.getAttribute?.("aria-current") || "").toLowerCase(),
          dataSelected: normalize(el.getAttribute?.("data-selected") || "").toLowerCase(),
          dataCurrent: normalize(el.getAttribute?.("data-current") || "").toLowerCase(),
        }};
      }})
      .filter((item) => item && (item.text || item.testId));
  }};

  const openSelectorMenu = async () => {{
    const button = findSelectorButton();
    if (!button) return readItems();
    const attempts = [
      () => realClick(button),
      () => button.click?.(),
      () => {{
        button.focus?.();
        keyPress(button, "Enter", "Enter");
      }},
      () => {{
        button.focus?.();
        keyPress(button, " ", "Space");
      }},
    ];
    let openedItems = readItems();
    for (const attempt of attempts) {{
      if (openedItems.length > 0) break;
      attempt();
      await wait(250);
      openedItems = readItems();
    }}
    return openedItems;
  }};
  const readSelectorLabel = () => {{
    const liveSelectorButton = findSelectorButton();
    return normalize(
      liveSelectorButton?.querySelector?.("[data-testid='selected-model'], [data-testid='model-switcher-selected-model']")?.textContent ||
      liveSelectorButton?.innerText ||
      liveSelectorButton?.textContent ||
      liveSelectorButton?.getAttribute?.("aria-label") ||
      liveSelectorButton?.getAttribute?.("title") ||
      ""
    ).toLowerCase();
  }};

  let items = readItems();
  if (items.length === 0) {{
    items = await openSelectorMenu();
    for (let attempt = 0; attempt < 20 && items.length === 0; attempt += 1) {{
      await wait(250);
      items = readItems();
    }}
  }}

  if (items.length === 0) {{
    return {{
      ...responseBase,
      status: "not-found",
      availableItems: [],
      selectorExpanded: normalize(findSelectorButton()?.getAttribute?.("aria-expanded") || "").toLowerCase(),
      itemCount: 0,
      modelUsed: currentLabel || null,
    }};
  }}

  const rankings = buildTierRankings(items);

  let target = null;
  let selectionNeedles = [];
  let requestedTestIds = [];
  if (autoMode) {{
    target =
      findExactTierLabelItem(items, "pro") ||
      selectBestTierItem(items, "pro", rankings) ||
      findSingleGenericTierItem(items, "pro") ||
      findExactTierLabelItem(items, "thinking") ||
      selectBestTierItem(items, "thinking", rankings) ||
      findSingleGenericTierItem(items, "thinking") ||
      findExactTierLabelItem(items, "instant") ||
      selectBestTierItem(items, "instant", rankings) ||
      findSingleGenericTierItem(items, "instant") ||
      items
        .map((item) => ({{ item, meta: rankings.candidateScore(item) }}))
        .filter(({{ item, meta }}) => !meta.tier || !hasConflictingTierHint(item, meta.tier))
        .sort((left, right) => right.meta.score - left.meta.score || right.item.text.length - left.item.text.length)
        .map(({{ item }}) => item)[0] || null;
    if (!target || rankings.candidateScore(target).score <= 0) {{
      return {{
        ...responseBase,
        status: "not-found",
        availableItems: items.map((item) => item.text || item.testId).filter(Boolean).slice(0, 12),
        itemCount: items.length,
        modelUsed: currentLabel || null,
      }};
    }}
    selectionNeedles.push(target.testId.toLowerCase(), target.text.toLowerCase());
  }} else {{
    requestedTestIds = Array.from(new Set(
      (MODEL_TESTID_CANDIDATES[requestedLower] || [MODEL_TESTID_ALIASES[requestedLower] || requestedLower])
        .map((value) => normalize(value).toLowerCase())
        .filter(Boolean)
    ));
    const exactTestIds = requestedTestIds.map((value) => `model-switcher-${{value}}`.toLowerCase());
    const fallbackNeedles = Array.from(new Set([
      requestedLower,
      requestedLower.replace(/-/g, " "),
      ...(requestedGenericTier ? [requestedGenericTier] : []),
      ...requestedTestIds,
      ...requestedTestIds.map((value) => value.replace(/-/g, " ")),
      ...exactTestIds,
    ])).filter(Boolean);
    target = requestedGenericTier
      ? findExactTierLabelItem(items, requestedGenericTier) ||
        selectBestTierItem(items, requestedGenericTier, rankings) ||
        findSingleGenericTierItem(items, requestedGenericTier) ||
        null
      : findPreferredTestIdItem(items, exactTestIds) ||
        findGenericNeedleItem(items, fallbackNeedles) ||
        null;
    if (!target) {{
      return {{
        ...responseBase,
        status: "not-found",
        availableItems: items.map((item) => item.text || item.testId).filter(Boolean).slice(0, 12),
        selectorExpanded: normalize(findSelectorButton()?.getAttribute?.("aria-expanded") || "").toLowerCase(),
        itemCount: items.length,
        modelUsed: currentLabel || null,
      }};
    }}
    selectionNeedles.push(...fallbackNeedles, ...exactTestIds);
  }}

  const targetTestId = target.testId || null;
  if (itemIsSelected(target)) {{
    return {{
      ...responseBase,
      status: "already-selected",
      modelUsed: target.text || currentLabel || requestedTrimmed || null,
      targetTestId,
    }};
  }}

  let selectionConfirmed = false;
  let selectedLabel = "";
  let targetChecked = false;
  let menuReopenAttempts = 0;
  const targetTierSlug = autoMode ? classifyTier(target) : (requestedGenericTier || classifyTier(target));
  for (let attempt = 0; attempt < 20; attempt += 1) {{
    realClick(findSelectorButton());
    await wait(250);
    const currentItems = readItems();
    let currentTarget = targetTierSlug
      ? findExactTierLabelItem(currentItems, targetTierSlug) ||
        selectBestTierItem(currentItems, targetTierSlug, buildTierRankings(currentItems)) ||
        findSingleGenericTierItem(currentItems, targetTierSlug) ||
        null
      : targetTestId
        ? findPreferredTestIdItem(currentItems, [targetTestId.toLowerCase()]) || null
        : findGenericNeedleItem(currentItems, selectionNeedles) || null;
    if (!currentTarget && currentItems.length === 0 && menuReopenAttempts < 3) {{
      await openSelectorMenu();
      menuReopenAttempts += 1;
      continue;
    }}
    const selectorButton = findSelectorButton();
    if (currentTarget) {{
      realClick(document.querySelector(`[data-testid="${{currentTarget.testId}}"]`) || Array.from(document.querySelectorAll(MODEL_ITEM_SELECTOR)).find((el) => {{
        const text = normalize(el.innerText || el.textContent || "");
        const testId = normalize(el.getAttribute?.("data-testid") || "");
        return testId === currentTarget.testId || text === currentTarget.text;
      }}) || selectorButton);
    }}
    // Verify polling: after clicking the menu item, the menu usually closes,
    // Radix propagates aria-checked, and ChatGPT's selector button label
    // updates on its own schedule (sometimes >1s after the click). Poll up to
    // ~3s on this attempt before re-opening/re-clicking on the next outer
    // iteration.
    let verifyConfirmed = false;
    let updatedItems = [];
    let updatedTarget = null;
    for (let verify = 0; verify < 12 && !verifyConfirmed; verify += 1) {{
      await wait(250);
      updatedItems = readItems();
      const updatedRankings = buildTierRankings(updatedItems);
      updatedTarget = targetTierSlug
        ? findExactTierLabelItem(updatedItems, targetTierSlug) ||
          selectBestTierItem(updatedItems, targetTierSlug, updatedRankings) ||
          findSingleGenericTierItem(updatedItems, targetTierSlug) ||
          null
        : targetTestId
          ? findPreferredTestIdItem(updatedItems, [targetTestId.toLowerCase()]) || null
          : findGenericNeedleItem(updatedItems, selectionNeedles) || null;
      const targetNode = targetTestId
        ? Array.from(document.querySelectorAll(`[data-testid="${{targetTestId}}"]`)).find((el) => isVisible(el)) || null
        : null;
      targetChecked = isCheckedState(
        normalize(targetNode?.getAttribute?.("aria-checked") || "").toLowerCase(),
        normalize(targetNode?.getAttribute?.("data-state") || "").toLowerCase(),
      ) ||
        hasSelectedCurrentMarker(
          normalize(targetNode?.getAttribute?.("aria-selected") || "").toLowerCase(),
          normalize(targetNode?.getAttribute?.("aria-current") || "").toLowerCase(),
          normalize(targetNode?.getAttribute?.("data-selected") || "").toLowerCase(),
          normalize(targetNode?.getAttribute?.("data-current") || "").toLowerCase(),
        ) ||
        itemIsSelected(updatedTarget);
      selectedLabel = readSelectorLabel();
      const textTargetLabelConfirmed = selectorLabelConfirmsTextTarget(
        selectedLabel,
        updatedTarget || currentTarget || target,
        targetTierSlug,
        selectionNeedles
      );
      if (targetChecked) {{
        verifyConfirmed = true;
      }} else if (textTargetLabelConfirmed) {{
        verifyConfirmed = true;
      }}
    }}
    if (verifyConfirmed) {{
      selectionConfirmed = true;
      return {{
        ...responseBase,
        status: "selected",
        modelUsed: updatedTarget?.text || target.text || requestedTrimmed || currentLabel || null,
        selectedLabel: selectedLabel || null,
        targetTestId,
        targetChecked,
        menuReopenAttempts,
        selectorExpanded: normalize(findSelectorButton()?.getAttribute?.("aria-expanded") || "").toLowerCase(),
        availableItemsAfter: updatedItems.map((item) => item.text || item.testId).filter(Boolean).slice(0, 12),
      }};
    }}
    const reopenedItems = await openSelectorMenu();
    if (reopenedItems.length > 0) {{
      menuReopenAttempts += 1;
    }}
    const reopenedRankings = buildTierRankings(reopenedItems);
    const reopenedTarget = targetTierSlug
      ? findExactTierLabelItem(reopenedItems, targetTierSlug) ||
        selectBestTierItem(reopenedItems, targetTierSlug, reopenedRankings) ||
        findSingleGenericTierItem(reopenedItems, targetTierSlug) ||
        null
      : targetTestId
        ? findPreferredTestIdItem(reopenedItems, [targetTestId.toLowerCase()]) ||
          findGenericNeedleItem(reopenedItems, selectionNeedles) ||
          null
        : findGenericNeedleItem(reopenedItems, selectionNeedles) || null;
    const reopenedChecked = isCheckedState(
      reopenedTarget?.ariaChecked || "",
      reopenedTarget?.dataState || "",
    ) || itemIsSelected(reopenedTarget);
    const reopenedLabel = readSelectorLabel();
    if (reopenedChecked) {{
      selectionConfirmed = true;
      updatedItems = reopenedItems;
      updatedTarget = reopenedTarget;
      targetChecked = reopenedChecked;
      selectedLabel = reopenedLabel;
      return {{
        ...responseBase,
        status: "selected",
        modelUsed: reopenedTarget?.text || target.text || requestedTrimmed || currentLabel || null,
        selectedLabel: reopenedLabel || null,
        targetTestId,
        targetChecked: reopenedChecked,
        menuReopenAttempts,
        selectorExpanded: normalize(findSelectorButton()?.getAttribute?.("aria-expanded") || "").toLowerCase(),
        availableItemsAfter: reopenedItems.map((item) => item.text || item.testId).filter(Boolean).slice(0, 12),
      }};
    }}
  }}

  return {{
    ...responseBase,
    status: selectionConfirmed ? "selected" : "selection-mismatch",
    modelUsed: selectedLabel || target.text || null,
    selectedLabel: selectedLabel || null,
    targetTestId,
    targetChecked,
    menuReopenAttempts,
    selectorExpanded: normalize(findSelectorButton()?.getAttribute?.("aria-expanded") || "").toLowerCase(),
    itemCount: items.length,
    availableItems: items.map((item) => item.text || item.testId).filter(Boolean).slice(0, 12),
  }};
}}
"##,
        model_button_selector = model_button_selector,
        composer_selector = composer_selector,
        model_item_selector = model_item_selector,
        model_testid_aliases = model_testid_aliases,
        model_testid_candidates = model_testid_candidates,
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
    fn model_aliases_cover_shortcuts() {
        let aliases = model_testid_alias_map();
        assert_eq!(aliases.get("extended-pro"), Some(&"extended-pro"));
        assert_eq!(aliases.get("pro"), Some(&"gpt-5-4-pro"));
        assert_eq!(aliases.get("gpt-5-thinking"), Some(&"gpt-5-4-thinking"));
        assert_eq!(aliases.get("instant"), Some(&"gpt-5-3"));
        let candidates = model_testid_candidate_map();
        assert_eq!(candidates.get("extended-pro"), Some(&vec!["extended-pro"]));
        assert_eq!(candidates.get("pro"), Some(&vec!["gpt-5-4-pro"]));
        assert_eq!(candidates.get("gpt-5-3-pro"), None);
    }

    #[test]
    fn canonical_chatgpt_model_slug_rejects_generic_labels() {
        assert_eq!(canonical_chatgpt_model_slug("ChatGPT"), None);
        assert_eq!(canonical_chatgpt_model_slug("Configure..."), None);
        assert_eq!(
            canonical_chatgpt_model_slug("Pro"),
            Some("gpt-5-4-pro".to_string())
        );
        assert_eq!(
            canonical_chatgpt_model_slug("Extended Pro"),
            Some("extended-pro".to_string())
        );
        assert_eq!(
            canonical_chatgpt_model_slug("model-switcher-gpt-5-4-thinking"),
            Some("gpt-5-4-thinking".to_string())
        );
    }

    #[test]
    fn reported_chatgpt_model_prefers_target_test_id_over_generic_button_label() {
        let selection = serde_json::json!({
            "status": "selected",
            "modelUsed": "chatgpt",
            "selectedLabel": "chatgpt",
            "currentLabel": "chatgpt",
            "targetTestId": "model-switcher-gpt-5-4-pro"
        });
        assert_eq!(
            select_reported_chatgpt_model(&selection, "pro"),
            Some("gpt-5-4-pro".to_string())
        );
    }

    #[test]
    fn reported_chatgpt_model_falls_back_to_requested_alias_when_ui_label_is_generic() {
        let selection = serde_json::json!({
            "status": "selected",
            "modelUsed": "chatgpt",
            "selectedLabel": "chatgpt",
            "currentLabel": "chatgpt"
        });
        assert_eq!(
            select_reported_chatgpt_model(&selection, "pro"),
            Some("gpt-5-4-pro".to_string())
        );
    }

    #[test]
    fn keep_current_chatgpt_model_detects_current_and_empty_requests() {
        assert!(should_keep_current_chatgpt_model(""));
        assert!(should_keep_current_chatgpt_model("current"));
        assert!(should_keep_current_chatgpt_model(" KEEP-CURRENT "));
        assert!(!should_keep_current_chatgpt_model("auto"));
        assert!(!should_keep_current_chatgpt_model("pro"));
    }

    #[test]
    fn chatgpt_model_selection_status_reports_contract_values() {
        assert_eq!(
            chatgpt_model_selection_status(&serde_json::json!({"status": "selected"}), "pro"),
            ChatgptModelSelectionStatus::Selected
        );
        assert_eq!(
            chatgpt_model_selection_status(
                &serde_json::json!({"status": "already-selected"}),
                "current"
            ),
            ChatgptModelSelectionStatus::KeptCurrent
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
                "pro"
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
    fn model_selection_auto_mode_filters_conflicting_tier_items() {
        // Regression guard: auto mode must honor hasConflictingTierHint so
        // items like `model-switcher-gpt-5-3-pro` are never auto-selected over
        // the real current Pro. The explicit-model path already did this;
        // the auto branch was missing the filter and would pick 5-3-pro when
        // 5-4-pro wasn't visibly tagged.
        let script = build_model_selection_function("auto");
        assert!(
            script
                .contains(".filter(({ item, meta }) => !meta.tier || !hasConflictingTierHint(item, meta.tier))"),
            "auto-mode ranking missing hasConflictingTierHint filter"
        );
    }

    #[test]
    fn scope_composer_file_input_function_targets_composer_form_only() {
        // The upload scoping helper must bind to the composer's own form and
        // write the shared marker — otherwise yoetz can tag a page-wide hidden
        // file input and inject the bundle there (review finding #10).
        let script = build_scope_composer_file_input_function();
        assert!(
            script.contains("composer.closest(\"form\")"),
            "scope helper must walk to the composer's form ancestor"
        );
        assert!(
            script.contains("form.querySelector(\"input[type='file']\")"),
            "scope helper must restrict search to the composer form"
        );
        assert!(
            script.contains(&format!("\"{}\"", COMPOSER_FILE_INPUT_MARKER)),
            "scope helper must write the shared marker value"
        );
        assert!(
            script.contains("status: \"marked\""),
            "scope helper must report a `marked` status on success"
        );
    }

    #[test]
    fn model_selection_function_polls_verification_beyond_initial_click() {
        // The verify loop must poll up to ~3s per outer attempt to let Radix
        // propagate aria-checked and ChatGPT update the selector button label.
        // A single 250ms wait is not enough on the live Pro UI.
        let script = build_model_selection_function("auto");
        assert!(
            script.contains("verify < 12 && !verifyConfirmed"),
            "verify loop missing or sized wrong; saw: {script}"
        );
        assert!(
            script.contains("let verifyConfirmed = false;"),
            "verify confirmation flag missing"
        );
    }

    #[test]
    fn model_selection_function_waits_for_selector_button_before_failing() {
        // Fresh ChatGPT tabs can render the shell before the model selector
        // exists in the React tree. Explicit model selection should wait
        // briefly for that button instead of failing on the first probe.
        let script = build_model_selection_function("pro");
        assert!(script.contains("const waitForSelectorButton = async () =>"));
        assert!(script.contains("attempt < 5 && !button"));
        assert!(script.contains("await wait(1000);"));
        assert!(script.contains("if (button || keepCurrentMode) return button;"));
        assert!(script.contains("const selectorButton = await waitForSelectorButton();"));
    }

    #[test]
    fn model_selection_function_supports_personal_composer_model_control() {
        // Personal ChatGPT can expose the selected model as a composer pop-up
        // labeled `Extended Pro` instead of the enterprise
        // model-switcher-dropdown-button. The shared script must support that
        // surface without removing the battle-tested enterprise selector.
        let script = build_model_selection_function("auto");
        assert!(script.contains("const findComposerModelControl = () =>"));
        assert!(script.contains(
            "document.querySelector(MODEL_BUTTON_SELECTOR) || findComposerModelControl()"
        ));
        assert!(script.contains("const currentLabelSatisfiesRequest = (label) =>"));
        assert!(script.contains(r#"if (key === "extended-pro") return "extended-pro";"#));
        assert!(script.contains("return isProLabel;"));
    }

    #[test]
    fn model_selection_function_prefers_exact_tier_labels_before_fuzzy_matching() {
        // Live ChatGPT currently exposes generic tier labels like
        // `Pro Research-grade intelligence` while the selector button itself
        // often stays labeled `ChatGPT`. The explicit-model branch must first
        // match the visible tier label before falling back to broader haystack
        // heuristics, otherwise `model=pro` can falsely report not-found.
        let script = build_model_selection_function("gpt-5-4-pro");
        assert!(script.contains("const findExactTierLabelItem = (entries, slug) =>"));
        assert!(script.contains("text === slug || text.startsWith(`${slug} `)"));
        assert!(script.contains("findExactTierLabelItem(items, requestedGenericTier) ||"));
    }

    #[test]
    fn model_selection_function_exact_labels_override_stale_testid_conflicts() {
        // Live ChatGPT currently exposes `Pro` / `Thinking` on menu items whose
        // testids still look like `gpt-5-3-pro` / `gpt-5-3-thinking`. The
        // visible tier label must win over the stale suffix or auto/pro
        // selection falls back to Instant.
        let script = build_model_selection_function("pro");
        assert!(script.contains("const hasExactTierLabel = (item, slug) =>"));
        assert!(script.contains(
            "hasTrustedUserFacingTierLabel(item, slug) || hasExactTierLabel(item, slug)"
        ));
    }

    #[test]
    fn model_selection_function_reopens_menu_when_button_label_stays_generic() {
        // Live ChatGPT can keep the selector button text at `ChatGPT` even
        // after the right radio item is checked. Reopen the menu once more and
        // inspect the visible checked state before returning mismatch.
        let script = build_model_selection_function("auto");
        assert!(script.contains("const reopenedItems = await openSelectorMenu();"));
        assert!(script.contains("const reopenedChecked = isCheckedState("));
        assert!(script.contains("const reopenedLabel = readSelectorLabel();"));
    }

    #[test]
    fn model_selection_function_does_not_confirm_visible_unchecked_pro_with_generic_label() {
        let visible_unchecked_pro_fixture = r#"
<button data-testid="model-switcher-dropdown-button">
  <span data-testid="model-switcher-selected-model">ChatGPT</span>
</button>
<div role="menuitemradio" data-testid="model-switcher-gpt-5-4-pro" aria-checked="false">
  Pro Research-grade intelligence
</div>
"#;
        assert!(visible_unchecked_pro_fixture.contains("aria-checked=\"false\""));
        assert!(visible_unchecked_pro_fixture.contains(">ChatGPT<"));

        let script = build_model_selection_function("pro");
        assert!(
            script.contains("if (targetChecked)"),
            "model selection must require checked/current menu state, not only selector label text"
        );
        assert!(
            !script.contains("targetChecked || selectorLabelConfirmed"),
            "selector label text must not confirm model selection without checked/current state"
        );
        assert!(
            !script.contains("reopenedChecked ||"),
            "reopened selector label text must not confirm model selection without checked/current state"
        );
        assert!(
            !script.contains("trustedTierSelected"),
            "visible trusted tier menu text must not confirm selection by itself"
        );
        assert!(
            !script.contains("reopenedTrustedTierSelected"),
            "visible trusted tier menu text must not confirm selection after reopen"
        );
    }

    #[test]
    fn model_selection_function_supports_auto_and_explicit_modes() {
        let auto_script = build_model_selection_function("auto");
        assert!(auto_script.contains(r#"const requested = "auto";"#));
        assert!(!auto_script.contains(r#""kept-current-no-selector""#));
        assert!(auto_script.contains("const deriveRequestedTier = (value) =>"));
        assert!(auto_script.contains("const classifyTier = (item) =>"));
        assert!(auto_script.contains("const buildTierRankings = (entries) =>"));

        let explicit_script = build_model_selection_function("gpt-5-4-pro");
        assert!(explicit_script.contains(r#"const requested = "gpt-5-4-pro";"#));
        assert!(explicit_script.contains("\"extended-pro\":\"extended-pro\""));
        assert!(explicit_script.contains("\"gpt-5-pro\":\"gpt-5-4-pro\""));
        assert!(!explicit_script.contains("\"gpt-5-3-pro\""));
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

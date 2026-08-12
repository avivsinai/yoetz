import * as dom from "../chatgpt-dom.js";
import { fetchConversationAnswer } from "./chatgpt-backend.js";
export * from "../chatgpt-dom.js";
export { fetchConversationAnswer } from "./chatgpt-backend.js";

const CHATGPT_MODEL = "gpt-5-6-sol-extra-high";
const CHATGPT_ORIGIN = "https://chatgpt.com";

function conversationIdFromUrl(value) {
  try {
    const pathname = new URL(String(value ?? ""), CHATGPT_ORIGIN).pathname;
    const match = pathname.match(/^\/c\/([^/?#]+)$/);
    return match ? decodeURIComponent(match[1]) : null;
  } catch {
    return null;
  }
}

function conversationUrl(conversationId) {
  if (!conversationId) return null;
  return `${CHATGPT_ORIGIN}/c/${encodeURIComponent(conversationId)}`;
}

function normalizeConversationId(value) {
  if (value == null) {
    return { ok: true, id: null };
  }
  if (typeof value !== "string") {
    return { ok: false, message: "invalid `conversation_id`: expected a string ChatGPT conversation id" };
  }
  const id = value.trim();
  if (!id || id === "." || id === "..") {
    return { ok: false, message: "invalid `conversation_id`: expected a non-empty ChatGPT conversation id" };
  }
  if (id.length > 256) {
    return { ok: false, message: "invalid `conversation_id`: expected at most 256 characters" };
  }
  if (!/^[A-Za-z0-9_.-]+$/.test(id)) {
    return { ok: false, message: "invalid `conversation_id`: expected ASCII letters, digits, `_`, `.`, or `-`" };
  }
  return { ok: true, id };
}

function isExpectedConversationIdAssignment(job, expectedConversationId, currentConversationId) {
  const requestedConversationId = String(job?.conversation_id ?? "").trim();
  const submittedConversationId = String(job?.submitted_conversation_id ?? "").trim();
  const expected = String(expectedConversationId ?? "").trim();
  const current = String(currentConversationId ?? "").trim();
  return !requestedConversationId
    && submittedConversationId === expected
    && expected.startsWith("WEB:")
    && normalizeConversationId(current).ok;
}

function modelUsedLooksLikeSolMaximum(value) {
  const folded = String(value ?? "").trim().replace(/\s+/g, " ").toLowerCase();
  return folded === "gpt-5.6 sol pro" || folded === "gpt-5.6 sol extra high";
}

function isAcceptableModelSelection(selection) {
  if (selection?.status === "current") {
    return selection?.requested_model === "current"
      && selection?.family_status === "skipped"
      && selection?.effort_status === "skipped";
  }
  return selection?.status === "selected"
    && selection?.requested_model === CHATGPT_MODEL
    && selection?.family_status === "verified"
    && selection?.effort_status === "verified"
    && modelUsedLooksLikeSolMaximum(selection?.model_used);
}

function normalizedResponseText(value) {
  return String(value ?? "")
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function nonNegativeFiniteNumber(value) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? number : null;
}

function isFreshBackendApiExtraction(extraction) {
  return extraction?.method === "backend_api"
    && extraction.node_fresh === true
    && !extraction.is_generating
    && Boolean(normalizedResponseText(extraction.text));
}

function hasFinalAssistantAffordance(extraction) {
  return Boolean(!extraction?.is_generating && extraction?.has_copy_button);
}

function hasNoVisibleStopControls(extraction) {
  const stopControlCount = nonNegativeFiniteNumber(extraction?.diagnostics?.counts?.stop_controls);
  return stopControlCount === null || stopControlCount === 0;
}

function hasStableIdleUnscopedCopyAffordance(job, extraction, minTextChars) {
  if (hasFinalAssistantAffordance(extraction)
      || !hasNoVisibleStopControls(extraction)
      || normalizedResponseText(extraction?.text).length < minTextChars) {
    return false;
  }
  const baseline = nonNegativeFiniteNumber(job.response_baseline?.copy_button_count);
  const current = nonNegativeFiniteNumber(extraction?.copy_button_count);
  return baseline !== null && current !== null && current > baseline;
}

function hasExplicitlyNoVisibleStopControls(extraction) {
  const stopControlCount = nonNegativeFiniteNumber(extraction?.diagnostics?.counts?.stop_controls);
  return stopControlCount === 0;
}

function isRenderFreezeRefreshCandidate(
  job,
  extraction,
  scopedExtractionCandidate,
  finalAffordance,
  { conversationId, shortResponseMaxChars, maxRefreshAttempts }
) {
  if (!scopedExtractionCandidate
      || finalAffordance
      || extraction?.method === "backend_api"
      || !hasExplicitlyNoVisibleStopControls(extraction)) {
    return false;
  }
  const text = normalizedResponseText(extraction?.text);
  return Boolean(
    text
    && text.length <= shortResponseMaxChars
    && conversationId
    && Number(job.render_refresh_attempts ?? 0) < maxRefreshAttempts
  );
}

function canRefreshFrozenRender(job, maxRefreshAttempts) {
  return Number(job?.render_refresh_attempts ?? 0) < maxRefreshAttempts;
}

function sameRenderRefreshCandidate(candidate, extraction) {
  return Boolean(candidate && extraction)
    && normalizedResponseText(candidate.text) === normalizedResponseText(extraction.text)
    && Number(candidate.assistant_count ?? 0) === Number(extraction.assistant_count ?? 0)
    && Number(candidate.turn_index ?? -1) === Number(extraction.turn_index ?? -1);
}

function selectFinalAffordanceCandidate(candidate, extraction) {
  const candidateText = normalizedResponseText(candidate?.text);
  const nextText = normalizedResponseText(extraction?.text);
  if (!candidate && extraction) {
    return { candidate: extraction, resetTimer: true };
  }
  if (!nextText) {
    return { candidate, resetTimer: false };
  }
  if (!candidateText) {
    return { candidate: extraction, resetTimer: true };
  }
  if (nextText.length < candidateText.length) {
    return { candidate, resetTimer: false };
  }
  if (nextText === candidateText) {
    return { candidate, resetTimer: false };
  }
  return { candidate: extraction, resetTimer: nextText.length > candidateText.length };
}

function finalAffordanceExtractionFailureMessage(job, extraction, stableForMs, inspectCommand) {
  return `ChatGPT rendered a final assistant affordance but Yoetz could not extract scoped assistant text (method=${extraction?.method ?? "none"}, assistant_count=${extraction?.assistant_count ?? 0}, turn_index=${extraction?.turn_index ?? -1}, copy_button_count=${extraction?.copy_button_count ?? 0}, stable_for_ms=${stableForMs}). Inspect the owned tab with \`${inspectCommand}\` before rerunning.`;
}

function completedExtraction(extraction, completionReason, stableForMs) {
  return {
    ...extraction,
    completion_reason: completionReason,
    stable_for_ms: stableForMs,
    assistant_turn_count: Number(extraction.assistant_count ?? 0),
    copy_button_count: Number(extraction.copy_button_count ?? 0)
  };
}

function isBackendApiFallbackError(error) {
  const code = String(error?.code ?? "");
  if (code.startsWith("backend_api_")) {
    return true;
  }
  const message = String(error?.message ?? error);
  return /unknown content-script command yoetz_fetch_conversation|unexpected tab message yoetz_fetch_conversation|401|unauthorized|conversation api|backend api/i.test(message);
}

export const chatgptSiteAdapter = Object.freeze({
  recipe: "chatgpt",
  displayName: "ChatGPT",
  homeUrl: `${CHATGPT_ORIGIN}/`,
  tabQueryPattern: `${CHATGPT_ORIGIN}/*`,
  defaultModel: CHATGPT_MODEL,
  inspectScope: "--chatgpt",
  dom,
  jobUrl: dom.chatgptJobUrl,
  conversationJobUrl: dom.chatgptConversationJobUrl,
  conversationUrl,
  conversationIdFromUrl,
  isExpectedConversationIdAssignment,
  isConversationUrl: (url) => Boolean(conversationIdFromUrl(url)),
  normalizeConversationId,
  isAllowedTabUrl: (url) => String(url ?? "").startsWith(`${CHATGPT_ORIGIN}/`),
  isAcceptableModelSelection,
  fetchConversationAnswer,
  tabActivation: Object.freeze({
    activateOnCreate: false,
    // Explicit policy marker: background jobs never capture or restore focus.
    restorePreviousAfter: null
  }),
  auth: Object.freeze({
    noTabStatus: "no_chatgpt_tab",
    selection(candidate) {
      if (candidate.tab.active && !candidate.yoetzOwned) return "active_non_yoetz_chatgpt_tab";
      if (!candidate.yoetzOwned) return "non_yoetz_chatgpt_tab";
      if (candidate.tab.active) return "active_yoetz_job_tab";
      return "yoetz_job_tab";
    }
  }),
  completion: Object.freeze({
    supportsBackendApiFallback: true,
    renderRefreshMode: "reload_conversation",
    emptyResponseWarning: "empty ChatGPT response extracted",
    isFreshBackendApiExtraction,
    hasFinalAssistantAffordance,
    hasStableIdleUnscopedCopyAffordance,
    isRenderFreezeRefreshCandidate,
    canRefreshFrozenRender,
    sameRenderRefreshCandidate,
    selectFinalAffordanceCandidate,
    finalAffordanceExtractionFailureMessage,
    completedExtraction,
    isBackendApiFallbackError
  })
});

export { chatgptSiteAdapter as siteAdapter };

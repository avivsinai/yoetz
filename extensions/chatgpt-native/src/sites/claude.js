import * as dom from "../claude-dom.js";
export * from "../claude-dom.js";

const CLAUDE_MODEL = "fable-5-max";
const CLAUDE_ORIGIN = "https://claude.ai";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function conversationIdFromUrl(value) {
  try {
    const pathname = new URL(String(value ?? ""), CLAUDE_ORIGIN).pathname;
    const match = pathname.match(/^\/chat\/([^/?#]+)$/);
    const id = match ? decodeURIComponent(match[1]) : null;
    return id && UUID.test(id) ? id : null;
  } catch {
    return null;
  }
}

function conversationUrl(conversationId) {
  return conversationId ? `${CLAUDE_ORIGIN}/chat/${encodeURIComponent(conversationId)}` : null;
}

function normalizeConversationId(value) {
  if (value == null) return { ok: true, id: null };
  if (typeof value !== "string") {
    return { ok: false, message: "invalid `conversation_id`: expected a Claude conversation UUID" };
  }
  const id = conversationIdFromUrl(value.trim()) ?? value.trim();
  return UUID.test(id)
    ? { ok: true, id }
    : { ok: false, message: "invalid `conversation_id`: expected a Claude conversation UUID" };
}

function isAcceptableModelSelection(selection) {
  return selection?.status === "selected"
    && selection?.requested_model === CLAUDE_MODEL
    && selection?.modelVerified === true
    && selection?.maxVerified === true
    && selection?.model_used === "Fable 5 Max";
}

function normalizedResponseText(value) {
  return String(value ?? "")
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function hasFinalAssistantAffordance(extraction) {
  return Boolean(
    !extraction?.is_generating
    && extraction?.method === "assistant_dom"
    && normalizedResponseText(extraction?.text)
  );
}

function selectFinalAffordanceCandidate(candidate, extraction) {
  const candidateText = normalizedResponseText(candidate?.text);
  const nextText = normalizedResponseText(extraction?.text);
  if (!candidate && extraction) return { candidate: extraction, resetTimer: true };
  if (!nextText) return { candidate, resetTimer: false };
  if (!candidateText) return { candidate: extraction, resetTimer: true };
  if (nextText.length < candidateText.length || nextText === candidateText) {
    return { candidate, resetTimer: false };
  }
  return { candidate: extraction, resetTimer: nextText.length > candidateText.length };
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

export function artifactUnextractedWarnings(extraction) {
  const count = Number(extraction?.artifact_blocks?.count ?? 0);
  if (!Number.isInteger(count) || count <= 0) return [];
  const titles = Array.isArray(extraction?.artifact_blocks?.titles)
    ? extraction.artifact_blocks.titles.filter((title) => typeof title === "string" && title)
    : [];
  return [{
    code: "artifact_unextracted",
    count,
    titles
  }];
}

export const claudeSiteAdapter = Object.freeze({
  recipe: "claude",
  displayName: "Claude",
  homeUrl: `${CLAUDE_ORIGIN}/new`,
  tabQueryPattern: `${CLAUDE_ORIGIN}/*`,
  defaultModel: CLAUDE_MODEL,
  inspectScope: "--claude",
  dom,
  jobUrl: dom.claudeJobUrl,
  conversationJobUrl: dom.claudeConversationJobUrl,
  conversationUrl,
  conversationIdFromUrl,
  isConversationUrl: (url) => Boolean(conversationIdFromUrl(url)),
  normalizeConversationId,
  isAllowedTabUrl: (url) => String(url ?? "").startsWith(`${CLAUDE_ORIGIN}/`),
  isAcceptableModelSelection,
  tabActivation: Object.freeze({
    activateOnCreate: false,
    // Explicit policy marker: background jobs never capture or restore focus.
    restorePreviousAfter: null
  }),
  auth: Object.freeze({
    noTabStatus: "no_claude_tab",
    selection(candidate) {
      if (candidate.tab.active && !candidate.yoetzOwned) return "active_non_yoetz_claude_tab";
      if (!candidate.yoetzOwned) return "non_yoetz_claude_tab";
      if (candidate.tab.active) return "active_yoetz_job_tab";
      return "yoetz_job_tab";
    }
  }),
  completion: Object.freeze({
    supportsBackendApiFallback: false,
    renderRefreshMode: "none",
    finalAffordanceRequiresStableIdle: true,
    emptyResponseWarning: "empty Claude response extracted",
    extractionWarnings: artifactUnextractedWarnings,
    isFreshBackendApiExtraction: () => false,
    hasFinalAssistantAffordance,
    hasStableIdleUnscopedCopyAffordance: () => false,
    isRenderFreezeRefreshCandidate: () => false,
    canRefreshFrozenRender: () => false,
    sameRenderRefreshCandidate: () => false,
    selectFinalAffordanceCandidate,
    finalAffordanceExtractionFailureMessage(_job, extraction, stableForMs, inspectCommand) {
      return `Claude rendered final assistant controls but Yoetz could not extract scoped assistant text (method=${extraction?.method ?? "none"}, assistant_count=${extraction?.assistant_count ?? 0}, copy_button_count=${extraction?.copy_button_count ?? 0}, stable_for_ms=${stableForMs}). Inspect the owned tab with \`${inspectCommand}\` before rerunning.`;
    },
    completedExtraction,
    isBackendApiFallbackError: () => false
  })
});

export { claudeSiteAdapter as siteAdapter };

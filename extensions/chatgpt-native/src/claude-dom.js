export const YOETZ_WINDOW_PREFIX = "yoetz-claude-native:";
export const OWNERSHIP_ATTR = "data-yoetz-claude-native-job";

const CLAUDE_ORIGIN = "https://claude.ai";
const COMPOSER_SELECTOR = "[data-testid='chat-input']";
const MODEL_SELECTOR = "[data-testid='model-selector-dropdown']";
const FILE_INPUT_SELECTOR = "input[data-testid='file-upload']";
const ATTACHMENT_SELECTOR = "[data-testid='file-thumbnail']";
const COPY_ACTION_SELECTOR = "[data-testid='action-bar-copy']";
const ASSISTANT_SELECTOR = "[data-is-streaming]";
const MODEL_MENU_SETTLE_MS = 300;
const BLOCKING_STATE_SCAN_INTERVAL_MS = 1000;
const SWITCH_MODELS_CONTROL_SELECTOR = "button, a, [role='button'], [role='link']";
// Keep control corroboration inside a local notice subtree rather than allowing
// an unrelated page-level model control to validate matching text elsewhere.
const MAX_CONTROL_SURFACE_DEPTH = 8;
// A provider notice is short; this rejects broad page/sidebar wrappers when the
// control-backed fallback is needed because no semantic live region exists.
const MAX_CONTROL_SURFACE_TEXT_CHARS = 1000;
const CONTENT_SCRIPT_INSTANCE_ID = globalThis.crypto?.randomUUID?.()
  ?? `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
const assistantIdentityCache = new WeakMap();
let nextAssistantIdentity = 0;
const CONVERSATION_CONTENT_SELECTOR = [
  COMPOSER_SELECTOR,
  "[data-testid='user-message']",
  "[data-testid*='turn']",
  "[data-is-streaming]",
  "article"
].join(", ");
// Visible claude.ai web banner observed in an Aviv-provided screenshot on
// 2026-07-27. This is screenshot provenance, not a captured DOM fixture; match
// stable credit-exhaustion language while the structural control/container
// guards carry false-positive resistance.
const USAGE_CREDITS_PROVIDER_MESSAGE = "Your org is out of usage credits for the month. We let your admin know. Switch models to continue chatting.";
const USAGE_CREDITS_TEXT_PATTERN = /\bout of usage credits\b/i;
const blockingStateScanCache = new WeakMap();

export function ownedWindowName(job) {
  return `${YOETZ_WINDOW_PREFIX}${job.run_id}:${job.job_id}`;
}

export function parseOwnedWindowName(value) {
  if (typeof value !== "string" || !value.startsWith(YOETZ_WINDOW_PREFIX)) {
    return null;
  }
  const rest = value.slice(YOETZ_WINDOW_PREFIX.length);
  const separator = rest.lastIndexOf(":");
  return separator > 0
    ? { run_id: rest.slice(0, separator), job_id: rest.slice(separator + 1) }
    : null;
}

export function claudeJobUrl(runId) {
  const url = new URL("/new", CLAUDE_ORIGIN);
  url.searchParams.set("_yoetz", runId);
  return url.toString();
}

export function claudeConversationJobUrl(conversationId, runId) {
  const url = new URL(`/chat/${encodeURIComponent(conversationId)}`, CLAUDE_ORIGIN);
  url.searchParams.set("_yoetz", runId);
  return url.toString();
}

export function classifyManualHandoff({ url = "", title = "", text = "" } = {}) {
  const haystack = normalizeText(`${title} ${text} ${url}`).toLowerCase();
  if (/cloudflare|checking your browser|attention required|security check|just a moment|verify you are human|cf-chl/.test(haystack)) {
    return {
      state: "challenge_required",
      message: "Claude browser verification is required in this Chrome profile"
    };
  }
  if (/\blog in\b|\blogin\b|\bsign in\b|\bsign up\b|continue with google|\/login|\/oauth/.test(haystack)) {
    return {
      state: "login_required",
      message: "Claude login is required in this Chrome profile"
    };
  }
  return null;
}

export function classifyWaitManualHandoff({ url = "", title = "" } = {}) {
  return classifyManualHandoff({ url, title, text: "" });
}

export function manualHandoffContext(root = document) {
  return {
    authenticated: Boolean(findComposer(root)),
    title: String(root.title ?? ""),
    text: getPageText(root)
  };
}

export function classifyBlockingState(root = document, { forceScan = false } = {}) {
  const pageText = normalizeText(root.body?.textContent || root.documentElement?.textContent);
  if (pageText && !USAGE_CREDITS_TEXT_PATTERN.test(pageText)) {
    return null;
  }
  const now = Date.now();
  const cached = blockingStateScanCache.get(root);
  if (!forceScan && cached && now - cached.scanned_at_ms < BLOCKING_STATE_SCAN_INTERVAL_MS) {
    return cached.value;
  }
  const candidates = Array.from(root.querySelectorAll?.("body *") ?? []);
  const switchControls = Array.from(root.querySelectorAll?.(SWITCH_MODELS_CONTROL_SELECTOR) ?? [])
    .filter((element) => /^switch models?$/i.test(normalizeText(element.innerText || element.textContent)))
    .filter((element) => isStrictlyVisibleBannerElement(root, element));
  const surfaces = candidates
    .filter((element) => USAGE_CREDITS_TEXT_PATTERN.test(normalizeText(element.textContent)))
    .filter((element) => !element.closest?.(CONVERSATION_CONTENT_SELECTOR))
    .map((element) => ({
      surface: notificationSurfaceFor(element, root)
        ?? controlBackedSurfaceFor(element, switchControls, root)
    }))
    .filter(({ surface }) => Boolean(surface))
    .filter(({ surface }, index, all) => all.findIndex((entry) => entry.surface === surface) === index)
    .map(({ surface }) => ({
      surface,
      controlCorroborated: switchControls.some((control) => isDescendantOrSelf(control, surface)),
      contributors: deepestUsageCreditsContributors(surface, candidates)
    }))
    .filter(({ surface }) => !surface.closest?.(CONVERSATION_CONTENT_SELECTOR))
    .filter(({ surface }) => !surface.querySelector?.(CONVERSATION_CONTENT_SELECTOR))
    .filter(({ surface }) => isStrictlyVisibleBannerElement(root, surface))
    .filter(({ contributors }) => contributors.some((element) => isStrictlyVisibleBannerElement(root, element)))
    .map(({ surface, controlCorroborated }) => ({
      surface,
      controlCorroborated,
      text: normalizeText(surface.innerText)
    }))
    .filter(({ text, controlCorroborated }) => isUsageCreditsMessage(text, { controlCorroborated }))
    .sort((left, right) => left.text.length - right.text.length);
  const match = surfaces.at(0);
  if (!match) {
    blockingStateScanCache.set(root, { scanned_at_ms: now, value: null });
    return null;
  }
  const container = match.surface;
  const switchControl = switchControls
    .filter((element) => isDescendantOrSelf(element, container))
    .at(0);
  const blockingState = {
    state: "usage_credits_exhausted",
    code: "usage_credits_exhausted",
    requested_model: "fable-5-max",
    provider_message: match.text || USAGE_CREDITS_PROVIDER_MESSAGE,
    provider_dom: {
      container: elementDiagnostic(container),
      switch_models_control: switchControl ? elementDiagnostic(switchControl) : { found: false }
    },
    message: "Claude cannot run Fable 5 Max because this organization is out of monthly usage credits. Yoetz did not switch models."
  };
  blockingStateScanCache.set(root, { scanned_at_ms: now, value: blockingState });
  return blockingState;
}

export function findComposer(root = document) {
  return root.querySelector(COMPOSER_SELECTOR);
}

export function findFileInput(root = document) {
  return root.querySelector(FILE_INPUT_SELECTOR);
}

export function findSendButton(root = document) {
  return root.querySelector("button[aria-label='Send message']");
}

export function getPageText(root = document) {
  return normalizeText(root.body?.innerText ?? root.body?.textContent ?? "");
}

export function markOwnership(root, job) {
  root.documentElement?.setAttribute(OWNERSHIP_ATTR, `${job.run_id}:${job.job_id}`);
}

export function assertOwnedPage(win, job) {
  const parsed = parseOwnedWindowName(win.name);
  if (parsed?.run_id !== job.run_id || parsed?.job_id !== job.job_id) {
    throw new Error(`tab ownership marker mismatch for job ${job.job_id}`);
  }
}

export async function insertPrompt(root, prompt, options = {}) {
  const composer = await waitForClaudeState(
    root,
    () => findComposer(root),
    options.timeoutMs ?? 20000,
    { phase: "send", side_effect_started: true, send_committed: false }
  );
  composer.focus();
  const text = String(prompt ?? "");
  if (composer.isContentEditable || composer.getAttribute("contenteditable") === "true") {
    if (typeof root.execCommand !== "function") {
      throw new Error("Claude ProseMirror composer does not expose document.execCommand");
    }
    const selection = root.getSelection?.();
    const range = root.createRange?.();
    if (selection && range) {
      range.selectNodeContents(composer);
      selection.removeAllRanges();
      selection.addRange(range);
    }
    const inserted = root.execCommand("insertText", false, text);
    if (inserted === false && normalizeText(composer.textContent) !== normalizeText(text)) {
      throw new Error("Claude ProseMirror composer rejected insertText");
    }
  } else if ("value" in composer) {
    composer.value = text;
    composer.dispatchEvent(new Event("input", { bubbles: true }));
  } else {
    throw new Error("Claude composer is neither contenteditable nor a value control");
  }
  composer.dispatchEvent(new Event("change", { bubbles: true }));
  return true;
}

export async function uploadFile(root, file, options = {}) {
  const timeoutMs = options.timeoutMs ?? 120000;
  const stallTimeoutMs = Math.max(timeoutMs, options.stallTimeoutMs ?? timeoutMs);
  const attachmentTrace = initialAttachmentTrace(options.initialAttachmentTrace);
  let timeoutStage = "file_input";
  try {
    const input = await waitFor(() => findFileInput(root), Math.min(timeoutMs, 20000));
    attachmentTrace.input_resolved_at_ms = Date.now();
    const transfer = new DataTransfer();
    transfer.items.add(file);
    input.files = transfer.files;
    attachmentTrace.files_assigned_at_ms = Date.now();
    input.dispatchEvent(new Event("change", { bubbles: true }));
    attachmentTrace.change_dispatched_at_ms = Date.now();
    timeoutStage = "attachment_readiness";
    await waitForAttachmentReadiness(root, file.name, attachmentTrace, timeoutMs, stallTimeoutMs);
    return true;
  } catch (error) {
    if (error?.isClaudeWaitTimeout) {
      error.message = `${error.message}; upload diagnostics: ${claudeUploadDiagnosticSummary(root, file.name, timeoutStage)}`;
    }
    throw error;
  }
}

const ATTACHMENT_READINESS_LEGS = Object.freeze([
  "matching_thumbnail",
  "remove_control",
  "send_present",
  "send_enabled"
]);

function initialAttachmentTrace(value) {
  const trace = {};
  const finalChunkAckAtMs = Number(value?.final_chunk_ack_at_ms);
  if (Number.isFinite(finalChunkAckAtMs) && finalChunkAckAtMs >= 0) {
    trace.final_chunk_ack_at_ms = finalChunkAckAtMs;
  }
  return trace;
}

function attachmentReadiness(root, filename) {
  const matchingThumbnails = Array.from(root.querySelectorAll(ATTACHMENT_SELECTOR)).filter((node) =>
    normalizeText(node.querySelector("h3")?.textContent) === filename
  );
  const removeControl = matchingThumbnails.some((node) =>
    Boolean(node.querySelector("button[aria-label='Remove']"))
  );
  const send = findSendButton(root);
  const sendPresent = Boolean(send);
  const sendEnabled = sendPresent
    && !send.disabled
    && send.getAttribute("aria-disabled") !== "true";
  return {
    matching_thumbnail: matchingThumbnails.length > 0,
    remove_control: removeControl,
    send_present: sendPresent,
    send_enabled: sendEnabled
  };
}

function recordAttachmentReadiness(trace, readiness, now) {
  for (const leg of ATTACHMENT_READINESS_LEGS) {
    const timestampKey = `${leg}_at_ms`;
    if (readiness[leg] && trace[timestampKey] === undefined) {
      trace[timestampKey] = now;
    }
  }
}

function pendingAttachmentLegs(readiness) {
  return ATTACHMENT_READINESS_LEGS.filter((leg) => !readiness[leg]);
}

async function waitForAttachmentReadiness(root, filename, trace, softTimeoutMs, stallTimeoutMs) {
  const dispatchedAtMs = trace.change_dispatched_at_ms ?? Date.now();
  const softDeadline = dispatchedAtMs + softTimeoutMs;
  const hardDeadline = dispatchedAtMs + stallTimeoutMs;
  while (true) {
    const now = Date.now();
    const readiness = attachmentReadiness(root, filename);
    recordAttachmentReadiness(trace, readiness, now);
    if (ATTACHMENT_READINESS_LEGS.every((leg) => readiness[leg])) {
      return true;
    }
    if (now >= softDeadline && trace.soft_timeout_at_ms === undefined) {
      trace.soft_timeout_at_ms = now;
      trace.soft_timeout_pending_legs = pendingAttachmentLegs(readiness);
    }
    if (now >= hardDeadline) {
      if (hardDeadline === softDeadline) {
        const error = new Error(`Claude page did not reach the requested state within ${softTimeoutMs}ms`);
        error.isClaudeWaitTimeout = true;
        throw error;
      }
      trace.hard_timeout_at_ms = now;
      trace.hard_timeout_pending_legs = pendingAttachmentLegs(readiness);
      throw commandError(
        "attachment_stalled",
        `Claude attachment stalled before readiness within ${stallTimeoutMs}ms`,
        {
          phase: "upload",
          side_effect_started: true,
          attachment_trace: trace
        }
      );
    }
    await sleep(Math.min(100, Math.max(1, hardDeadline - now)));
  }
}

export async function ensureFreshChat(root = document, job = {}, options = {}) {
  await waitForReadyComposer(root, options.timeoutMs ?? 20000);
  const pathname = globalThis.location?.pathname ?? "";
  if (/^\/chat\//.test(pathname)) {
    throw commandError(
      "fresh_chat_lost",
      `Claude fresh job ${job.job_id ?? "(unknown)"} opened an existing conversation`,
      { phase: "upload", side_effect_started: false }
    );
  }
  return { status: "fresh", pathname };
}

export async function ensureConversationLoaded(root = document, conversationId, options = {}) {
  const timeoutMs = options.timeoutMs ?? 120000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() <= deadline) {
    throwIfBlockingState(root, {
      phase: "upload",
      side_effect_started: false,
      send_committed: false
    });
    const currentId = conversationIdFromLocation();
    if (currentId && currentId !== conversationId) {
      throw commandError(
        "conversation_changed",
        `Claude conversation changed from ${conversationId} to ${currentId}`,
        {
          phase: "upload",
          side_effect_started: false,
          requested_conversation_id: conversationId,
          current_conversation_id: currentId,
          current_url: globalThis.location?.href ?? ""
        }
      );
    }
    if (currentId === conversationId && findComposer(root)) {
      return { status: "loaded", conversation_id: conversationId, pathname: globalThis.location?.pathname ?? "" };
    }
    const handoff = classifyManualHandoff({
      url: globalThis.location?.href,
      title: root.title,
      text: getPageText(root).slice(0, 500)
    });
    if (handoff) {
      throw commandError(handoff.state, handoff.message, {
        phase: "upload",
        side_effect_started: false,
        requested_conversation_id: conversationId,
        current_url: globalThis.location?.href ?? ""
      });
    }
    const pageText = getPageText(root).toLowerCase();
    if (/conversation (?:not found|unavailable)|unable to load|does not exist|you do not have access/.test(pageText)) {
      throw commandError(
        "conversation_unavailable",
        `Claude conversation ${conversationId} is unavailable in this profile`,
        {
          phase: "upload",
          side_effect_started: false,
          requested_conversation_id: conversationId,
          current_url: globalThis.location?.href ?? ""
        }
      );
    }
    await sleep(options.intervalMs ?? 200);
  }
  throw commandError(
    "conversation_not_loaded",
    `Claude conversation ${conversationId} did not load before timeout`,
    {
      phase: "upload",
      side_effect_started: false,
      requested_conversation_id: conversationId,
      current_conversation_id: conversationIdFromLocation(),
      current_url: globalThis.location?.href ?? ""
    }
  );
}

export async function configureModelState(root, job = {}) {
  const timeoutMs = Number(job.model_selection_timeout_ms) || 10000;
  const modelButton = await waitForModelState(root, () => root.querySelector(MODEL_SELECTOR), 20000);
  await openModelMenu(root, modelButton, timeoutMs);
  const fable = await waitForModelOptional(root, () => visibleElements(root, "[role='menuitemradio']")
    .find((element) => normalizeText(element.innerText || element.textContent).toLowerCase().startsWith("fable 5")), timeoutMs);
  if (!fable) {
    const diagnostics = modelSelectionDiagnostics(root);
    const menuClosed = await closeModelMenu(root, modelButton);
    return {
      status: "unavailable",
      requested_model: "fable-5-max",
      model_used: diagnostics.modelChip || null,
      warning: `Claude Fable 5 is unavailable; live options: ${diagnostics.options.join(", ") || "none"}${menuClosed ? "" : "; model menu remained open"}`,
      menuClosed,
      ...diagnostics
    };
  }

  const alreadySelected = await verifyAlreadySelectedModel(root, modelButton, fable, timeoutMs);
  if (alreadySelected) {
    return alreadySelected;
  }

  throwIfModelBlocked(root);
  fable.click();
  await settleAfterMenuSelection(root, modelButton, timeoutMs);

  await openModelMenu(root, modelButton, timeoutMs);
  const effortTrigger = await waitForModelState(root, () => root.querySelector("[data-testid='effort-menu-trigger']"), timeoutMs);
  dispatchHover(effortTrigger);
  const max = await waitForModelState(root, () => root.querySelector("[role='menuitemradio'][data-testid='effort-option-max']"), timeoutMs);
  throwIfModelBlocked(root);
  max.click();
  await settleAfterMenuSelection(root, modelButton, timeoutMs);

  await closeModelMenu(root, modelButton);
  await sleep(MODEL_MENU_SETTLE_MS);
  await openModelMenu(root, modelButton, timeoutMs);
  const verificationEffort = await waitForModelState(root, () => root.querySelector("[data-testid='effort-menu-trigger']"), timeoutMs);
  dispatchHover(verificationEffort);
  // Live picker DOM captured 2026-07-21 exposes Fable 5 as a checked
  // menuitemradio and Max as effort-option-max; it has no independent Thinking
  // control. Keep both remaining legs as positive, post-click re-reads.
  await waitForModelState(root, () => root.querySelector("[role='menuitemradio'][data-testid='effort-option-max']"), timeoutMs);
  const diagnostics = modelSelectionDiagnostics(root);
  const menuClosed = await closeModelMenu(root, modelButton);
  await sleep(MODEL_MENU_SETTLE_MS);
  return {
    status: diagnostics.modelVerified && diagnostics.maxVerified && menuClosed
      ? "selected"
      : "mismatch",
    requested_model: "fable-5-max",
    model_used: diagnostics.modelVerified && diagnostics.maxVerified ? "Fable 5 Max" : diagnostics.modelChip || null,
    menuClosed,
    ...(menuClosed ? {} : { warning: "Claude model menu remained open after Escape; refusing to send" }),
    ...diagnostics
  };
}

export async function clickSend(root, options = {}) {
  const send = await waitForClaudeState(root, () => {
    const candidate = findSendButton(root);
    return candidate && !candidate.disabled && candidate.getAttribute("aria-disabled") !== "true"
      ? candidate
      : null;
  }, options.timeoutMs ?? 120000, {
    phase: "send",
    side_effect_started: true,
    send_committed: false
  });
  throwIfBlockingState(root, {
    phase: "send",
    side_effect_started: true,
    send_committed: false
  }, { forceScan: true });
  send.click();
  return true;
}

export function sendAcceptanceBaseline(root = document) {
  return {
    user_count: userTurnCount(root),
    assistant_count: assistantRoots(root).length
  };
}

export async function waitForSendAccepted(root, baseline = {}, options = {}) {
  const timeoutMs = options.timeoutMs ?? 120000;
  return waitFor(() => {
    const blockingState = classifyBlockingState(root);
    if (blockingState) {
      throw blockingStateError(blockingState, {
        phase: "send",
        side_effect_started: true,
        send_committed: true
      });
    }
    const userCount = userTurnCount(root);
    const assistantCount = assistantRoots(root).length;
    const generating = isResponseGenerating(root);
    if (userCount > Number(baseline.user_count ?? 0)
        || assistantCount > Number(baseline.assistant_count ?? 0)
        || generating) {
      return { accepted: true, user_count: userCount, assistant_count: assistantCount };
    }
    return null;
  }, timeoutMs);
}

export function extractResponse(root = document) {
  const assistants = assistantRoots(root);
  const last = assistants.at(-1) ?? null;
  const turn = last?.closest?.("[data-testid*='turn'], article") ?? last;
  const body = last?.querySelector?.(".font-claude-response") ?? null;
  const text = responseBodyText(body);
  const artifactCards = Array.from(
    last?.querySelectorAll?.("[class*='group/artifact-block']") ?? []
  );
  const artifactBlocks = {
    count: artifactCards.length,
    titles: artifactCards
      .map((element) => normalizeText(element.innerText || element.textContent))
      .filter(Boolean)
  };
  const globalCopyButtons = root.querySelectorAll(COPY_ACTION_SELECTOR).length;
  const scopedCopyButtons = turn?.querySelectorAll?.(COPY_ACTION_SELECTOR)?.length ?? 0;
  const lastTurnStreaming = last?.getAttribute?.("data-is-streaming") ?? null;
  const assistantIdentity = last ? assistantIdentityFor(last) : null;
  const isGenerating = isResponseGenerating(root);
  const precedingUserCount = last ? precedingUserTurnCount(root, last) : 0;
  const diagnostics = {
    counts: {
      assistant_turns: assistants.length,
      copy_buttons: globalCopyButtons,
      scoped_copy_buttons: scopedCopyButtons,
      stop_controls: root.querySelectorAll("button[aria-label='Stop response']").length,
      thinking_rows: root.querySelectorAll("button[class*='group/status']").length,
      artifact_blocks: artifactBlocks.count
    },
    finality: {
      last_turn_streaming: lastTurnStreaming
    },
    assistant_turn_snippets: assistants.slice(-3).map((element) => elementSummary(element)),
    page_text_chars: String(root.body?.innerText ?? "").length,
    page_text_content_chars: String(root.body?.textContent ?? "").length
  };
  if (!last || !body || !text) {
    return {
      method: "none",
      text: "",
      is_generating: isGenerating,
      assistant_count: assistants.length,
      assistant_identity: assistantIdentity,
      turn_index: assistants.length - 1,
      preceding_user_count: precedingUserCount,
      copy_button_count: globalCopyButtons,
      has_copy_button: scopedCopyButtons > 0,
      artifact_blocks: artifactBlocks,
      diagnostics
    };
  }
  return {
    method: "assistant_dom",
    text,
    is_generating: isGenerating,
    assistant_count: assistants.length,
    assistant_identity: assistantIdentity,
    turn_index: assistants.length - 1,
    preceding_user_count: precedingUserCount,
    copy_button_count: globalCopyButtons,
    has_copy_button: scopedCopyButtons > 0,
    artifact_blocks: artifactBlocks,
    diagnostics
  };
}

export function isResponseGenerating(root = document) {
  const last = assistantRoots(root).at(-1);
  return Boolean(
    root.querySelector("button[aria-label='Stop response']")
    || last?.getAttribute?.("data-is-streaming") === "true"
  );
}

export function clickStopGenerating(root = document) {
  const stop = root.querySelector("button[aria-label='Stop response']");
  if (!stop) return false;
  stop.click();
  return true;
}

export async function confirmGenerationStopped(root = document, options = {}) {
  const startedAt = Date.now();
  const stopped = clickStopGenerating(root);
  const timeoutMs = options.timeoutMs ?? 5000;
  while (Date.now() - startedAt <= timeoutMs) {
    if (!isResponseGenerating(root)) {
      return { stopped, confirmed_idle: true, waited_ms: Date.now() - startedAt };
    }
    await sleep(100);
  }
  return { stopped, confirmed_idle: false, waited_ms: Date.now() - startedAt };
}

export function modelSelectionDiagnostics(root = document) {
  const modelButton = root.querySelector(MODEL_SELECTOR);
  const modelChip = normalizeText(modelButton?.innerText || modelButton?.textContent);
  const selectedModels = Array.from(root.querySelectorAll("[role='menuitemradio'][aria-checked='true']"));
  const modelVerified = /\bFable 5\b/i.test(modelChip)
    && selectedModels.some((element) => normalizeText(element.innerText || element.textContent).toLowerCase().startsWith("fable 5"));
  const maxOption = root.querySelector("[role='menuitemradio'][data-testid='effort-option-max']");
  const maxVerified = maxOption?.getAttribute("aria-checked") === "true" && /\bMax\b/i.test(modelChip);
  const options = visibleElements(root, "[role='menuitem'], [role='menuitemradio'], button, [role='switch']")
    .map((element) => normalizeText(element.innerText || element.textContent))
    .filter(Boolean)
    .slice(0, 50);
  return {
    modelVerified,
    maxVerified,
    modelChip,
    options
  };
}

export function normalizeText(value) {
  return String(value ?? "").replace(/\s+/g, " ").trim();
}

function normalizeResponseText(value) {
  return String(value ?? "")
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function responseBodyText(body) {
  if (!body) return "";
  const sanitized = body.cloneNode?.(true);
  if (!sanitized) {
    return normalizeResponseText(body.innerText || body.textContent || "");
  }
  for (const statusRow of sanitized.querySelectorAll?.("button[class*='group/status']") ?? []) {
    // Claude renders the visible status button and its hidden thinking caption as
    // siblings in the first row of a two-row status/answer grid. Remove that
    // structural row instead of trying to match its flattened text, which drifts
    // with punctuation and duration badges.
    const statusSubtree = thinkingStatusSubtree(statusRow, sanitized);
    if (statusSubtree && statusSubtree !== sanitized) {
      statusSubtree.remove();
    } else {
      statusRow.remove();
    }
  }
  return normalizeResponseText(sanitized.innerText || sanitized.textContent || "");
}

function thinkingStatusSubtree(statusRow, body) {
  for (let node = statusRow; node && node !== body; node = node.parentElement) {
    const classTokens = String(node.getAttribute?.("class") ?? "").split(/\s+/);
    if (classTokens.includes("row-start-1") && classTokens.includes("col-start-1")) {
      return node;
    }
  }
  return statusRow;
}

function assistantRoots(root) {
  return Array.from(root.querySelectorAll(ASSISTANT_SELECTOR));
}

function userTurnCount(root) {
  return root.querySelectorAll("[data-testid='user-message']").length;
}

function precedingUserTurnCount(root, target) {
  const users = Array.from(root.querySelectorAll("[data-testid='user-message']"));
  return users.filter((element) => isStrictlyFollowing(element, target)).length;
}

function isStrictlyFollowing(source, target) {
  const position = source?.compareDocumentPosition?.(target) ?? 0;
  const following = globalThis.Node?.DOCUMENT_POSITION_FOLLOWING ?? 4;
  const disconnected = globalThis.Node?.DOCUMENT_POSITION_DISCONNECTED ?? 1;
  const implementationSpecific = globalThis.Node?.DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC ?? 32;
  return !(position & (disconnected | implementationSpecific))
    && Boolean(position & following);
}

function assistantIdentityFor(element) {
  const providerIdentity = [
    element.getAttribute?.("data-message-id"),
    element.getAttribute?.("data-turn-id"),
    element.getAttribute?.("id"),
    element.id
  ].find((value) => typeof value === "string" && value.trim());
  if (providerIdentity) {
    return `provider:${providerIdentity.trim()}`;
  }
  let identity = assistantIdentityCache.get(element);
  if (!identity) {
    nextAssistantIdentity += 1;
    identity = `dom:${CONTENT_SCRIPT_INSTANCE_ID}:${nextAssistantIdentity}`;
    assistantIdentityCache.set(element, identity);
  }
  return identity;
}

function conversationIdFromLocation() {
  const match = String(globalThis.location?.pathname ?? "").match(/^\/chat\/([^/?#]+)$/);
  return match ? decodeURIComponent(match[1]) : null;
}

async function openModelMenu(root, button, timeoutMs) {
  if (button.getAttribute("aria-expanded") === "true") {
    await sleep(MODEL_MENU_SETTLE_MS);
    if (button.getAttribute("aria-expanded") === "true") {
      return;
    }
  }

  const attemptTimeoutMs = Math.max(100, Math.min(timeoutMs, 1000));
  for (let attempt = 0; attempt < 2; attempt += 1) {
    throwIfModelBlocked(root);
    button.click();
    const opened = await waitForModelOptional(
      root,
      () => button.getAttribute("aria-expanded") === "true",
      attemptTimeoutMs
    );
    if (opened) {
      await sleep(MODEL_MENU_SETTLE_MS);
      if (button.getAttribute("aria-expanded") === "true") {
        return;
      }
    }
  }
  throw new Error(`Claude model menu did not open within ${timeoutMs}ms`);
}

async function settleAfterMenuSelection(root, modelButton, timeoutMs) {
  await waitForModelState(root, () => modelButton.getAttribute("aria-expanded") !== "true", timeoutMs);
  await sleep(MODEL_MENU_SETTLE_MS);
}

async function verifyAlreadySelectedModel(root, modelButton, fable, timeoutMs) {
  throwIfModelBlocked(root);
  const modelChip = normalizeText(modelButton.innerText || modelButton.textContent);
  if (fable.getAttribute("aria-checked") !== "true" || !/\bFable 5\b.*\bMax\b/i.test(modelChip)) {
    return null;
  }

  const effortTrigger = root.querySelector("[data-testid='effort-menu-trigger']");
  if (!effortTrigger) {
    return null;
  }
  dispatchHover(effortTrigger);
  const max = await waitForModelOptional(
    root,
    () => root.querySelector("[role='menuitemradio'][data-testid='effort-option-max']"),
    timeoutMs
  );
  if (!max) {
    return null;
  }
  const diagnostics = modelSelectionDiagnostics(root);
  if (!diagnostics.modelVerified || !diagnostics.maxVerified) {
    return null;
  }
  const menuClosed = await closeModelMenu(root, modelButton);
  await sleep(MODEL_MENU_SETTLE_MS);
  return {
    status: menuClosed ? "selected" : "mismatch",
    requested_model: "fable-5-max",
    model_used: "Fable 5 Max",
    menuClosed,
    ...(menuClosed ? {} : { warning: "Claude model menu remained open after Escape; refusing to send" }),
    ...diagnostics
  };
}

function dispatchHover(element) {
  const rect = element.getBoundingClientRect?.() ?? { left: 0, top: 0, width: 1, height: 1 };
  const clientX = Number(rect.left ?? 0) + Math.max(1, Number(rect.width ?? 1) / 2);
  const clientY = Number(rect.top ?? 0) + Math.max(1, Number(rect.height ?? 1) / 2);
  for (const type of ["pointerover", "pointerenter", "pointermove", "mouseover", "mouseenter", "mousemove"]) {
    const pointer = type.startsWith("pointer") && typeof PointerEvent !== "undefined";
    const EventType = pointer ? PointerEvent : MouseEvent;
    element.dispatchEvent(new EventType(type, {
      bubbles: true,
      cancelable: true,
      view: globalThis.window,
      clientX,
      clientY,
      ...(pointer ? { pointerType: "mouse", pointerId: 1, isPrimary: true } : {})
    }));
  }
}

async function closeModelMenu(root, modelButton) {
  if (modelButton.getAttribute("aria-expanded") !== "true") return true;
  const target = root.activeElement ?? modelButton;
  const EventType = root.defaultView?.KeyboardEvent ?? globalThis.KeyboardEvent;
  if (typeof EventType !== "function") {
    return false;
  }
  target.dispatchEvent(new EventType("keydown", {
    key: "Escape",
    code: "Escape",
    bubbles: true,
    cancelable: true
  }));
  const escaped = await waitForModelOptional(
    root,
    () => modelButton.getAttribute("aria-expanded") !== "true",
    1000
  );
  if (escaped) {
    return true;
  }
  throwIfModelBlocked(root);
  modelButton.click();
  return Boolean(await waitForModelOptional(
    root,
    () => modelButton.getAttribute("aria-expanded") !== "true",
    2000
  ));
}

function visibleElements(root, selector) {
  return Array.from(root.querySelectorAll?.(selector) ?? [])
    .filter((element) => element.getClientRects().length > 0);
}

function hasUsageCreditsCore(text) {
  return USAGE_CREDITS_TEXT_PATTERN.test(text);
}

function isUsageCreditsMessage(text, { controlCorroborated = false } = {}) {
  if (!hasUsageCreditsCore(text)) {
    return false;
  }
  if (controlCorroborated) {
    return true;
  }
  const matchIndex = text.toLowerCase().indexOf("out of usage credits");
  const precedingWindow = text.slice(Math.max(0, matchIndex - 80), matchIndex);
  const sentenceBoundary = Math.max(
    precedingWindow.lastIndexOf("."),
    precedingWindow.lastIndexOf("!"),
    precedingWindow.lastIndexOf("?")
  );
  const precedingText = precedingWindow.slice(sentenceBoundary + 1);
  if (/\b(?:almost|nearly|not|no longer|isn't|aren't|about to|running low)\b/i.test(precedingText)) {
    return false;
  }
  return /\b(?:your|this)\s+org(?:anization)?\s+is\s+out of usage credits\b/i.test(text)
    || /\byou(?:'re| are)\s+out of usage credits\b/i.test(text);
}

function notificationSurfaceFor(element, root) {
  for (let node = element; node && node !== root.body && node !== root.documentElement; node = node.parentElement) {
    const role = normalizeText(node.getAttribute?.("role")).toLowerCase();
    const ariaLive = normalizeText(node.getAttribute?.("aria-live")).toLowerCase();
    if (role === "alert" || role === "status" || (ariaLive && ariaLive !== "off")) {
      return node;
    }
  }
  return null;
}

function controlBackedSurfaceFor(element, controls, root) {
  const ancestors = [];
  for (
    let node = element, depth = 0;
    node && node !== root.body && node !== root.documentElement && depth < MAX_CONTROL_SURFACE_DEPTH;
    node = node.parentElement, depth += 1
  ) {
    ancestors.push(node);
  }
  for (const control of controls) {
    for (
      let node = control, depth = 0;
      node && node !== root.body && node !== root.documentElement && depth < MAX_CONTROL_SURFACE_DEPTH;
      node = node.parentElement, depth += 1
    ) {
      if (!ancestors.includes(node)) {
        continue;
      }
      const text = normalizeText(node.innerText);
      if (text.length <= MAX_CONTROL_SURFACE_TEXT_CHARS && hasUsageCreditsCore(text)) {
        return node;
      }
      break;
    }
  }
  return null;
}

function deepestUsageCreditsContributors(surface, candidates) {
  const matching = candidates.filter((element) => (
    isDescendantOrSelf(element, surface)
    && hasUsageCreditsCore(normalizeText(element.textContent))
  ));
  // A phrase split across sibling inline elements can leave the surface itself
  // as the deepest contributor. We knowingly accept that paint edge until a
  // real provider DOM capture justifies text-node range machinery.
  return matching.filter((element) => !matching.some((other) => (
    other !== element && isDescendantOrSelf(other, element)
  )));
}

function isDescendantOrSelf(element, ancestor) {
  for (let node = element; node; node = node.parentElement) {
    if (node === ancestor) {
      return true;
    }
  }
  return false;
}

function elementDiagnostic(element) {
  return {
    found: true,
    tag: String(element?.tagName ?? "").toLowerCase() || null,
    role: element?.getAttribute?.("role") ?? null,
    testid: element?.getAttribute?.("data-testid") ?? null,
    class_fragment: normalizeText(element?.getAttribute?.("class")).slice(0, 160) || null
  };
}

function isStrictlyVisibleBannerElement(root, element) {
  const viewWidth = Number(root.defaultView?.innerWidth);
  const viewHeight = Number(root.defaultView?.innerHeight);
  let paintedIntersection = Number.isFinite(viewWidth) && viewWidth > 0
    && Number.isFinite(viewHeight) && viewHeight > 0
    ? { left: 0, top: 0, right: viewWidth, bottom: viewHeight }
    : null;
  for (let node = element; node; node = node.parentElement) {
    if (node.hidden || node.getAttribute?.("aria-hidden") === "true") {
      return false;
    }
    const style = root.defaultView?.getComputedStyle?.(node);
    if (style && (
      style.display === "none"
      || style.visibility === "hidden"
      || style.visibility === "collapse"
      || (style.opacity !== "" && Number(style.opacity) === 0)
      || style.contentVisibility === "hidden"
    )) {
      return false;
    }
    const displayContents = style?.display === "contents";
    const clientRects = node.getClientRects?.();
    if (node === element && !displayContents && clientRects?.length === 0) {
      return false;
    }
    const boundingRect = node.getBoundingClientRect?.();
    const rect = displayContents
      ? null
      : hasLayoutCoordinates(boundingRect)
        ? boundingRect
        : hasLayoutCoordinates(clientRects?.[0])
          ? clientRects[0]
          : null;
    if (rect && paintedIntersection) {
      const left = Number(rect.left ?? 0);
      const top = Number(rect.top ?? 0);
      const right = Number(rect.right ?? (left + Number(rect.width ?? 0)));
      const bottom = Number(rect.bottom ?? (top + Number(rect.height ?? 0)));
      if (node === element && (!(right > left) || !(bottom > top))) {
        return false;
      }
      const clipsOverflow = node !== element && style && [
        style.overflow,
        style.overflowX,
        style.overflowY
      ].some((value) => ["auto", "scroll", "hidden", "clip"].includes(value));
      if (node === element || clipsOverflow) {
        paintedIntersection = {
          left: Math.max(paintedIntersection.left, left),
          top: Math.max(paintedIntersection.top, top),
          right: Math.min(paintedIntersection.right, right),
          bottom: Math.min(paintedIntersection.bottom, bottom)
        };
        if (!(paintedIntersection.right > paintedIntersection.left)
            || !(paintedIntersection.bottom > paintedIntersection.top)) {
          return false;
        }
      }
    }
    if (node === root.documentElement) {
      break;
    }
  }
  return true;
}

function hasLayoutCoordinates(rect) {
  return Boolean(rect) && ["left", "top", "right", "bottom", "width", "height"]
    .some((key) => Number.isFinite(Number(rect[key])));
}

function elementSummary(element) {
  const innerText = String(element?.innerText ?? "");
  const textContent = String(element?.textContent ?? "");
  return {
    text: normalizeResponseText(innerText || textContent).slice(-500),
    inner_text_chars: innerText.length,
    text_content_chars: textContent.length,
    streaming: element?.getAttribute?.("data-is-streaming") ?? null
  };
}

function claudeUploadDiagnosticSummary(root, filename, timeoutStage) {
  const inputs = Array.from(root.querySelectorAll?.(FILE_INPUT_SELECTOR) ?? []);
  const thumbnails = Array.from(root.querySelectorAll?.(ATTACHMENT_SELECTOR) ?? []);
  const send = findSendButton(root);
  const observations = thumbnails.map((thumbnail) => {
    const label = normalizeText(
      thumbnail.querySelector?.("h3")?.textContent
      || thumbnail.getAttribute?.("aria-label")
      || thumbnail.getAttribute?.("title")
    );
    const text = normalizeText(thumbnail.innerText || thumbnail.textContent);
    const busy = thumbnail.getAttribute?.("aria-busy") === "true"
      || Boolean(thumbnail.querySelector?.([
        "[role='progressbar']",
        "[aria-busy='true']",
        "[data-state*='loading']"
      ].join(", ")))
      || /\b(uploading|attaching|processing|scanning)\b/i.test(text);
    const failureNode = thumbnail.querySelector?.(
      "[role='alert'], [data-testid*='error'], [aria-live='assertive']"
    );
    const failureText = normalizeText(failureNode?.innerText || failureNode?.textContent);
    const failure = failureText || (
      /\b(upload|attach|file)\b.*\b(failed|error)\b/i.test(text) ? text.slice(0, 200) : ""
    );
    return {
      label,
      matchesFilename: label === filename,
      removePresent: Boolean(thumbnail.querySelector?.("button[aria-label='Remove']")),
      busy,
      failure
    };
  });
  const filenameMatch = observations.find((item) => item.matchesFilename);
  return [
    `file_input_count=${inputs.length}`,
    `thumbnail_count=${thumbnails.length}`,
    `thumbnail_labels=${JSON.stringify(observations.map((item) => item.label))}`,
    `filename_match=${Boolean(filenameMatch)}`,
    `remove_present=${Boolean(filenameMatch?.removePresent)}`,
    `attachment_busy=${JSON.stringify(observations.filter((item) => item.busy).map((item) => item.label))}`,
    `attachment_failures=${JSON.stringify(observations.filter((item) => item.failure).map((item) => item.failure))}`,
    `send_present=${Boolean(send)}`,
    `send_disabled=${send ? Boolean(send.disabled) : null}`,
    `send_aria_disabled=${JSON.stringify(send?.getAttribute?.("aria-disabled") ?? null)}`,
    `timeout_stage=${JSON.stringify(timeoutStage)}`
  ].join(", ");
}

async function waitForReadyComposer(root, timeoutMs) {
  try {
    return await waitForClaudeState(root, () => findComposer(root), timeoutMs, {
      phase: "upload",
      side_effect_started: false,
      send_committed: false
    });
  } catch (error) {
    const handoff = classifyManualHandoff({
      url: globalThis.location?.href,
      title: root.title,
      text: getPageText(root).slice(0, 500)
    });
    if (handoff) {
      throw commandError(handoff.state, handoff.message, {
        phase: "upload",
        side_effect_started: false
      });
    }
    throw error;
  }
}

async function waitFor(read, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let value = read();
  while (!value && Date.now() <= deadline) {
    await sleep(100);
    value = read();
  }
  if (!value) {
    const error = new Error(`Claude page did not reach the requested state within ${timeoutMs}ms`);
    error.isClaudeWaitTimeout = true;
    throw error;
  }
  return value;
}

async function waitForClaudeState(root, read, timeoutMs, detail) {
  return waitFor(() => {
    throwIfBlockingState(root, detail);
    return read();
  }, timeoutMs);
}

function waitForModelState(root, read, timeoutMs) {
  return waitForClaudeState(root, read, timeoutMs, {
    phase: "model_selection",
    side_effect_started: false,
    send_committed: false
  });
}

function throwIfModelBlocked(root) {
  throwIfBlockingState(root, {
    phase: "model_selection",
    side_effect_started: false,
    send_committed: false
  }, { forceScan: true });
}

async function waitForModelOptional(root, read, timeoutMs) {
  try {
    return await waitForModelState(root, read, timeoutMs);
  } catch (error) {
    if (error?.code === "usage_credits_exhausted") {
      throw error;
    }
    return null;
  }
}

function commandError(code, message, detail = {}) {
  const error = new Error(message);
  error.code = code;
  Object.assign(error, detail);
  return error;
}

function blockingStateError(blockingState, detail = {}) {
  return commandError(blockingState.code, blockingState.message, {
    ...blockingState,
    ...detail
  });
}

function throwIfBlockingState(root, detail, options) {
  const blockingState = classifyBlockingState(root, options);
  if (blockingState) {
    throw blockingStateError(blockingState, detail);
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

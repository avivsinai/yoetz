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
  const composer = await waitFor(() => findComposer(root), options.timeoutMs ?? 20000);
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
  const input = await waitFor(() => findFileInput(root), Math.min(timeoutMs, 20000));
  const transfer = new DataTransfer();
  transfer.items.add(file);
  Object.defineProperty(input, "files", { configurable: true, value: transfer.files });
  input.dispatchEvent(new Event("change", { bubbles: true }));
  await waitFor(() => {
    const attachment = Array.from(root.querySelectorAll(ATTACHMENT_SELECTOR)).find((node) =>
      normalizeText(node.querySelector("h3")?.textContent) === file.name
      && Boolean(node.querySelector("button[aria-label='Remove']"))
    );
    const send = findSendButton(root);
    return attachment && send && !send.disabled && send.getAttribute("aria-disabled") !== "true";
  }, timeoutMs);
  return true;
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
  const modelButton = await waitFor(() => root.querySelector(MODEL_SELECTOR), 20000);
  await openModelMenu(modelButton, timeoutMs);
  const fable = await waitForOptional(() => visibleElements(root, "[role='menuitemradio']")
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

  fable.click();
  await settleAfterMenuSelection(modelButton, timeoutMs);

  await openModelMenu(modelButton, timeoutMs);
  let effortTrigger = await waitFor(() => root.querySelector("[data-testid='effort-menu-trigger']"), timeoutMs);
  dispatchHover(effortTrigger);
  const max = await waitFor(() => root.querySelector("[role='menuitemradio'][data-testid='effort-option-max']"), timeoutMs);
  max.click();
  await settleAfterMenuSelection(modelButton, timeoutMs);

  await openModelMenu(modelButton, timeoutMs);
  effortTrigger = await waitFor(() => root.querySelector("[data-testid='effort-menu-trigger']"), timeoutMs);
  dispatchHover(effortTrigger);
  const thinking = await waitFor(() => findThinkingSwitch(root), timeoutMs);
  if (thinking.getAttribute("aria-checked") !== "true") {
    thinking.click();
  }
  await sleep(MODEL_MENU_SETTLE_MS);

  await closeModelMenu(root, modelButton);
  await sleep(MODEL_MENU_SETTLE_MS);
  await openModelMenu(modelButton, timeoutMs);
  const verificationEffort = await waitFor(() => root.querySelector("[data-testid='effort-menu-trigger']"), timeoutMs);
  dispatchHover(verificationEffort);
  await waitFor(() => root.querySelector("[role='menuitemradio'][data-testid='effort-option-max']"), timeoutMs);
  await waitFor(() => findThinkingSwitch(root), timeoutMs);
  const diagnostics = modelSelectionDiagnostics(root);
  const menuClosed = await closeModelMenu(root, modelButton);
  await sleep(MODEL_MENU_SETTLE_MS);
  return {
    status: diagnostics.modelVerified && diagnostics.maxVerified && diagnostics.thinkingChecked && menuClosed
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
  const send = await waitFor(() => {
    const candidate = findSendButton(root);
    return candidate && !candidate.disabled && candidate.getAttribute("aria-disabled") !== "true"
      ? candidate
      : null;
  }, options.timeoutMs ?? 120000);
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
  const text = normalizeResponseText(body?.innerText || body?.textContent || "");
  const globalCopyButtons = root.querySelectorAll(COPY_ACTION_SELECTOR).length;
  const scopedCopyButtons = turn?.querySelectorAll?.(COPY_ACTION_SELECTOR)?.length ?? 0;
  const isGenerating = isResponseGenerating(root);
  const precedingUserCount = last ? precedingUserTurnCount(root, last) : 0;
  const diagnostics = {
    counts: {
      assistant_turns: assistants.length,
      copy_buttons: globalCopyButtons,
      stop_controls: root.querySelectorAll("button[aria-label='Stop response']").length,
      thinking_rows: root.querySelectorAll("button[class*='group/status']").length
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
      turn_index: assistants.length - 1,
      preceding_user_count: precedingUserCount,
      copy_button_count: globalCopyButtons,
      has_copy_button: scopedCopyButtons > 0,
      diagnostics
    };
  }
  return {
    method: "assistant_dom",
    text,
    is_generating: isGenerating,
    assistant_count: assistants.length,
    turn_index: assistants.length - 1,
    preceding_user_count: precedingUserCount,
    copy_button_count: globalCopyButtons,
    has_copy_button: scopedCopyButtons > 0,
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
  const thinking = findThinkingSwitch(root);
  const thinkingChecked = thinking?.getAttribute("aria-checked") === "true";
  const options = visibleElements(root, "[role='menuitem'], [role='menuitemradio'], button, [role='switch']")
    .map((element) => normalizeText(element.innerText || element.textContent))
    .filter(Boolean)
    .slice(0, 50);
  return {
    modelVerified,
    maxVerified,
    thinkingChecked,
    modelChip,
    options,
    thinkingAriaChecked: thinking?.getAttribute("aria-checked") ?? null
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

function assistantRoots(root) {
  return Array.from(root.querySelectorAll(ASSISTANT_SELECTOR));
}

function userTurnCount(root) {
  return root.querySelectorAll("[data-testid='user-message']").length;
}

function precedingUserTurnCount(root, target) {
  const users = Array.from(root.querySelectorAll("[data-testid='user-message']"));
  return users.filter((element) => {
    const position = element.compareDocumentPosition?.(target) ?? 0;
    const following = globalThis.Node?.DOCUMENT_POSITION_FOLLOWING ?? 4;
    return Boolean(position & following);
  }).length;
}

function conversationIdFromLocation() {
  const match = String(globalThis.location?.pathname ?? "").match(/^\/chat\/([^/?#]+)$/);
  return match ? decodeURIComponent(match[1]) : null;
}

async function openModelMenu(button, timeoutMs) {
  if (button.getAttribute("aria-expanded") === "true") {
    await sleep(MODEL_MENU_SETTLE_MS);
    if (button.getAttribute("aria-expanded") === "true") {
      return;
    }
  }

  const attemptTimeoutMs = Math.max(100, Math.min(timeoutMs, 1000));
  for (let attempt = 0; attempt < 2; attempt += 1) {
    button.click();
    const opened = await waitForOptional(
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

async function settleAfterMenuSelection(modelButton, timeoutMs) {
  await waitFor(() => modelButton.getAttribute("aria-expanded") !== "true", timeoutMs);
  await sleep(MODEL_MENU_SETTLE_MS);
}

async function verifyAlreadySelectedModel(root, modelButton, fable, timeoutMs) {
  const modelChip = normalizeText(modelButton.innerText || modelButton.textContent);
  if (fable.getAttribute("aria-checked") !== "true" || !/\bFable 5\b.*\bMax\b/i.test(modelChip)) {
    return null;
  }

  const effortTrigger = root.querySelector("[data-testid='effort-menu-trigger']");
  if (!effortTrigger) {
    return null;
  }
  dispatchHover(effortTrigger);
  const max = await waitForOptional(
    () => root.querySelector("[role='menuitemradio'][data-testid='effort-option-max']"),
    timeoutMs
  );
  if (!max) {
    return null;
  }
  const thinking = await waitForOptional(() => findThinkingSwitch(root), timeoutMs);
  if (!thinking) {
    return null;
  }

  const diagnostics = modelSelectionDiagnostics(root);
  if (!diagnostics.modelVerified || !diagnostics.maxVerified || !diagnostics.thinkingChecked) {
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

function findThinkingSwitch(root) {
  return visibleElements(root, "span[role='switch'][aria-checked]")
    .find((element) => /Thinking/i.test(normalizeText(
      element.getAttribute("aria-label")
      || element.closest("[role='menuitem']")?.innerText
      || element.parentElement?.innerText
    )));
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
  const escaped = await waitForOptional(
    () => modelButton.getAttribute("aria-expanded") !== "true",
    1000
  );
  if (escaped) {
    return true;
  }
  modelButton.click();
  return Boolean(await waitForOptional(
    () => modelButton.getAttribute("aria-expanded") !== "true",
    2000
  ));
}

function visibleElements(root, selector) {
  return Array.from(root.querySelectorAll(selector)).filter((element) => element.getClientRects().length > 0);
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

async function waitForReadyComposer(root, timeoutMs) {
  try {
    return await waitFor(() => findComposer(root), timeoutMs);
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
    throw new Error(`Claude page did not reach the requested state within ${timeoutMs}ms`);
  }
  return value;
}

async function waitForOptional(read, timeoutMs) {
  try {
    return await waitFor(read, timeoutMs);
  } catch {
    return null;
  }
}

function commandError(code, message, detail = {}) {
  const error = new Error(message);
  error.code = code;
  Object.assign(error, detail);
  return error;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

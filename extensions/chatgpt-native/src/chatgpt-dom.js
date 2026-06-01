export const YOETZ_WINDOW_PREFIX = "yoetz-chatgpt-native:";
export const OWNERSHIP_ATTR = "data-yoetz-chatgpt-native-job";
const DEFAULT_WAIT_TIMEOUT_MS = 15000;
const DEFAULT_WAIT_INTERVAL_MS = 250;
const DEFAULT_SEND_MIN_TIMEOUT_MS = 120000;

export function ownedWindowName(job) {
  return `${YOETZ_WINDOW_PREFIX}${job.run_id}:${job.job_id}`;
}

export function parseOwnedWindowName(value) {
  if (typeof value !== "string" || !value.startsWith(YOETZ_WINDOW_PREFIX)) {
    return null;
  }
  const rest = value.slice(YOETZ_WINDOW_PREFIX.length);
  const separator = rest.lastIndexOf(":");
  if (separator <= 0 || separator === rest.length - 1) {
    return null;
  }
  return {
    run_id: rest.slice(0, separator),
    job_id: rest.slice(separator + 1)
  };
}

export function chatgptJobUrl(runId) {
  const url = new URL("https://chatgpt.com/");
  url.searchParams.set("_yoetz", runId);
  return url.toString();
}

export function chatgptConversationJobUrl(conversationId, runId) {
  const url = new URL(`https://chatgpt.com/c/${encodeURIComponent(conversationId)}`);
  url.searchParams.set("_yoetz", runId);
  return url.toString();
}

export function classifyManualHandoff({ url = "", title = "", text = "" } = {}) {
  const haystack = `${url}\n${title}\n${text}`.toLowerCase();
  if (/\/auth\/login|log in|sign in/.test(haystack)) {
    return {
      state: "login_required",
      message: "ChatGPT login required in this Chrome profile"
    };
  }
  if (/captcha|cloudflare|verify you are human|security check/.test(haystack)) {
    return {
      state: "challenge_required",
      message: "ChatGPT requires manual challenge completion"
    };
  }
  if (/rate limit|too many requests|try again later/.test(haystack)) {
    return {
      state: "rate_limited",
      message: "ChatGPT is rate limited"
    };
  }
  return null;
}

export function classifyWaitManualHandoff({ url = "", title = "" } = {}) {
  return classifyManualHandoff({ url, title });
}

export function findComposer(root = document) {
  return firstVisible(root, [
    "#prompt-textarea",
    'div[contenteditable="true"][role="textbox"]',
    'textarea[placeholder*="Message"]',
    'textarea[data-testid*="composer"]',
    'textarea',
    'div[contenteditable="true"][data-testid*="composer"]',
    'div[contenteditable="true"]'
  ]);
}

export function findFileInput(root = document) {
  return findFileInputControl(root, { allowHidden: true });
}

function findFileInputControl(root = document, options = {}) {
  return firstInComposerScopes(root, [
    'input[type="file"][accept*="text"]',
    'input[type="file"]'
  ], options);
}

export function findSendButton(root = document) {
  return findSendButtonControl(root, { requireEnabled: true });
}

export function findModelButton(root = document, options = {}) {
  // ChatGPT serves at least two picker families: Enterprise exposes a global
  // model-switcher button, while personal ChatGPT can render a composer-scoped
  // model chip. Keep both paths because either account type may back Pro.
  const enterpriseButton = firstVisibleModelControl(root, [
    'button[data-testid="model-switcher-dropdown-button"]',
    'button:has([data-testid="selected-model"])',
    'button:has([data-testid="model-switcher-selected-model"])',
    'button[aria-label*="model" i]',
    'button[aria-controls*="model" i]',
    'button[id*="model" i]'
  ], options);
  const composerButton = findComposerModelControl(root, options);
  if (options.allowStandaloneFallback === false) {
    return enterpriseButton ?? composerButton;
  }
  return enterpriseButton ?? composerButton ?? findStandaloneProExtendedModelControl(root);
}

export function getPageText(root = document) {
  return String(root.body?.innerText ?? root.documentElement?.innerText ?? "");
}

export function markOwnership(root, job) {
  const target = root.documentElement ?? root.body;
  if (!target) {
    return false;
  }
  target.setAttribute(OWNERSHIP_ATTR, job.job_id);
  target.setAttribute("data-yoetz-run-id", job.run_id);
  return true;
}

export function assertOwnedPage(win, job) {
  const parsed = parseOwnedWindowName(win.name);
  return parsed?.job_id === job.job_id && parsed?.run_id === job.run_id;
}

export async function insertPrompt(root, prompt, options = {}) {
  const composer = await waitForElement(root, findComposer, "ChatGPT composer", options);
  composer.focus();
  if ("value" in composer) {
    setInputValue(composer, prompt);
    dispatchTextInput(composer, "input", prompt);
    composer.dispatchEvent(new Event("change", { bubbles: true }));
  } else {
    insertContenteditableText(root, composer, prompt);
  }
  await waitForCondition(
    () => composerContainsPrompt(findComposer(root), prompt),
    `ChatGPT composer did not accept prompt text (${sendReadinessDiagnostics(root)})`,
    {
      timeoutMs: Number(options.timeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS),
      intervalMs: Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS)
    }
  );
  return true;
}

export async function uploadFile(root, file, options = {}) {
  let input = findFileInputControl(root, { allowHidden: true });
  if (!input) {
    await openAttachmentUi(root, options);
    input = await waitForElement(
      root,
      (scope) => findFileInputControl(scope, { allowHidden: true }),
      "ChatGPT file input",
      options
    );
  }
  const baselineAttachments = attachmentNodeKeys(findAttachmentCandidates(root));
  const dataTransfer = new DataTransfer();
  dataTransfer.items.add(file);
  input.files = dataTransfer.files;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  input.dispatchEvent(new Event("change", { bubbles: true }));
  await waitForUploadComplete(root, file, { ...options, baselineAttachments });
  return true;
}

export async function ensureFreshChat(root = document, job = {}, options = {}) {
  const win = options.window ?? root.defaultView ?? globalThis;
  if (String(win.location?.pathname ?? "").startsWith("/c/")) {
    const newChat = await waitForElement(root, findNewChatControl, "ChatGPT new chat control", {
      timeoutMs: Number(options.timeoutMs ?? 10000),
      intervalMs: Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS)
    });
    newChat.click();
    await waitForCondition(
      () => !String(win.location?.pathname ?? "").startsWith("/c/"),
      "ChatGPT did not leave an existing conversation after New Chat",
      options
    );
  }
  await waitForCondition(
    () => !hasConversationResidue(root),
    "ChatGPT old conversation transcript did not clear before starting a fresh chat",
    {
      timeoutMs: Number(options.timeoutMs ?? 10000),
      intervalMs: Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS)
    }
  );

  const composer = await waitForElement(root, findComposer, "ChatGPT composer", options);
  const composerText = editableText(composer);
  const attachments = findAttachmentTiles(root, { composerOnly: true });
  const residue = conversationResidue(root);
  if (composerText || attachments.length > 0 || residue.user_count > 0 || residue.assistant_count > 0 || residue.copy_button_count > 0) {
    throw new Error(`ChatGPT tab is not a clean fresh chat (${JSON.stringify({
      pathname: String(win.location?.pathname ?? ""),
      run_id: job.run_id ?? null,
      composer_text_chars: composerText.length,
      attachment_count: attachments.length,
      ...residue
    })})`);
  }
  return {
    status: "fresh",
    pathname: String(win.location?.pathname ?? ""),
    composer_text_chars: 0,
    attachment_count: 0,
    ...residue
  };
}

export async function ensureConversationLoaded(root = document, conversationId, options = {}) {
  const win = options.window ?? root.defaultView ?? globalThis;
  const pathname = String(win.location?.pathname ?? "");
  const loadedConversationId = conversationIdFromPathname(pathname);
  if (loadedConversationId !== conversationId) {
    const code = loadedConversationId ? "conversation_not_loaded" : "conversation_unavailable";
    throw chatgptCommandError(
      code,
      code === "conversation_unavailable"
        ? `ChatGPT conversation ${conversationId} is unavailable; current URL is ${currentLocationForError(win)}`
        : `ChatGPT conversation ${conversationId} did not load; current URL is ${currentLocationForError(win)}`,
      {
        phase: "upload",
        side_effect_started: false,
        requested_conversation_id: conversationId,
        current_conversation_id: loadedConversationId,
        current_url: currentLocationForError(win),
        current_pathname: pathname
      }
    );
  }
  const unavailable = conversationUnavailableState(root, win);
  if (unavailable) {
    throw conversationUnavailableError(conversationId, loadedConversationId, win, pathname, unavailable);
  }
  try {
    await waitForElement(root, findComposer, "ChatGPT composer", options);
  } catch (error) {
    throw chatgptCommandError(
      "conversation_unavailable",
      `ChatGPT conversation ${conversationId} is unavailable; composer did not load at ${currentLocationForError(win)}: ${String(error?.message ?? error)}`,
      {
        phase: "upload",
        side_effect_started: false,
        requested_conversation_id: conversationId,
        current_conversation_id: loadedConversationId,
        current_url: currentLocationForError(win),
        current_pathname: pathname
      }
    );
  }
  const currentPathname = String(win.location?.pathname ?? "");
  const currentConversationId = conversationIdFromPathname(currentPathname);
  if (currentConversationId !== conversationId) {
    const code = currentConversationId ? "conversation_not_loaded" : "conversation_unavailable";
    throw chatgptCommandError(
      code,
      code === "conversation_unavailable"
        ? `ChatGPT conversation ${conversationId} is unavailable; current URL is ${currentLocationForError(win)}`
        : `ChatGPT conversation ${conversationId} did not load; current URL is ${currentLocationForError(win)}`,
      {
        phase: "upload",
        side_effect_started: false,
        requested_conversation_id: conversationId,
        current_conversation_id: currentConversationId,
        current_url: currentLocationForError(win),
        current_pathname: currentPathname
      }
    );
  }
  const postComposerUnavailable = conversationUnavailableState(root, win);
  if (postComposerUnavailable) {
    throw conversationUnavailableError(conversationId, currentConversationId, win, currentPathname, postComposerUnavailable);
  }
  return {
    status: "loaded",
    conversation_id: conversationId,
    pathname: currentPathname
  };
}

function conversationUnavailableError(conversationId, currentConversationId, win, pathname, unavailable) {
  return chatgptCommandError(
    "conversation_unavailable",
    `ChatGPT conversation ${conversationId} is unavailable at ${currentLocationForError(win)}${unavailable.reason ? `: ${unavailable.reason}` : ""}`,
    {
      phase: "upload",
      side_effect_started: false,
      requested_conversation_id: conversationId,
      current_conversation_id: currentConversationId,
      current_url: currentLocationForError(win),
      current_pathname: pathname,
      unavailable_reason: unavailable.reason
    }
  );
}

export async function configureModelState(root, job = {}) {
  const requested = proExtendedModelRequest();
  const selection = await selectRequestedModel(root, requested, modelSelectionOptionsForJob(job));
  const warnings = selection.warning ? [selection.warning] : [];
  return {
    status: selection.status,
    model_used: selection.model_used,
    requested_model: requested.raw,
    available_options: selection.available_options ?? [],
    extended_status: "required",
    warning: warnings[0] ?? null,
    warnings
  };
}

function modelSelectionOptionsForJob(job = {}) {
  const options = {};
  if (String(job?.conversation_id ?? "").trim()) {
    options.allowStandaloneFallback = false;
  }
  const timeoutMs = Number(job?.model_selection_timeout_ms);
  if (Number.isFinite(timeoutMs) && timeoutMs > 0) {
    options.timeoutMs = timeoutMs;
  }
  const intervalMs = Number(job?.model_selection_interval_ms);
  if (Number.isFinite(intervalMs) && intervalMs > 0) {
    options.intervalMs = intervalMs;
  }
  return options;
}

function isActionableElement(node) {
  const tag = String(node?.tagName ?? "").toLowerCase();
  const role = String(node?.getAttribute?.("role") ?? "").toLowerCase();
  return tag === "button"
    || ["button", "switch", "checkbox"].includes(role)
    || node?.getAttribute?.("tabindex") !== null;
}

function findComposerModelControl(root, options = {}) {
  for (const scope of modelControlScopes(root)) {
    const candidates = uniqueElements(Array.from(scope.querySelectorAll([
      "button",
      '[role="button"]',
      "[aria-haspopup]",
      "[tabindex]",
      "[aria-label]",
      "[title]",
      "[data-testid]",
      "span",
      "div"
    ].join(","))));
    for (const node of candidates) {
      const target = modelClickTarget(node, scope);
      if (!target || !isVisible(target, { allowDisabled: true })) {
        continue;
      }
      if (isTranscriptModelControl(target, options)) {
        continue;
      }
      const haystack = modelCandidateText(node, target);
      if (!looksLikeModelControl(haystack)) {
        continue;
      }
      if (/\b(send|stop|copy|share|new chat|attach|upload|search|history|dictation|voice|microphone|account|profile|settings|upgrade)\b/.test(haystack)) {
        continue;
      }
      return target;
    }
  }
  return null;
}

function findStandaloneProExtendedModelControl(root) {
  const requested = proExtendedModelRequest();
  const candidates = uniqueElements(Array.from(root.querySelectorAll([
    "button",
    '[role="button"]',
    "[aria-haspopup]"
  ].join(","))));
  return candidates.find((node) => {
    if (!isVisible(node, { allowDisabled: true })) {
      return false;
    }
    const haystack = modelCandidateText(node);
    if (!modelTextMatchesRequest(haystack, requested)) {
      return false;
    }
    return !/\b(send|stop|copy|share|new chat|attach|upload|search|history|dictation|voice|microphone|account|profile|settings|upgrade)\b/.test(haystack);
  }) ?? null;
}

function modelControlScopes(root) {
  const composer = findComposer(root);
  const scopes = [...composerScopes(root, { includeRoot: false })];
  const add = (scope) => {
    if (scope && !scopes.includes(scope)) {
      scopes.push(scope);
    }
  };
  add(composer?.closest("main, [role=\"main\"]"));
  return scopes;
}

function modelClickTarget(node, stopAt) {
  let current = node;
  while (current) {
    if (isModelActionableElement(current) || isModelChipLike(current)) {
      return current;
    }
    if (current === stopAt) {
      return null;
    }
    current = current.parentElement;
  }
  return null;
}

function modelCandidateText(node, target = node) {
  return normalizeText([
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
    target?.textContent
  ].filter(Boolean).join(" ")).toLowerCase();
}

function modelControlLabel(node) {
  return normalizeText([
    textOf(node),
    node?.getAttribute?.("aria-label"),
    node?.getAttribute?.("title")
  ].filter(Boolean).join(" "));
}

function looksLikeModelControl(text) {
  return /\bextended\s+pro\b/.test(text)
    || /\bgpt[\s.-]*\d/.test(text)
    || /\b(pro|instant|thinking|model)\b/.test(text);
}

function isModelActionableElement(node) {
  const tag = String(node?.tagName ?? "").toLowerCase();
  const role = String(node?.getAttribute?.("role") ?? "").toLowerCase();
  return tag === "button"
    || role === "button"
    || node?.getAttribute?.("aria-haspopup") !== null
    || node?.getAttribute?.("tabindex") !== null;
}

function isModelChipLike(node) {
  const marker = normalizeText([
    node?.getAttribute?.("data-testid"),
    node?.getAttribute?.("class"),
    node?.getAttribute?.("aria-label"),
    node?.getAttribute?.("title")
  ].filter(Boolean).join(" ")).toLowerCase();
  return /\b(model|model-switcher)\b/.test(marker) && /\b(chip|pill|token|button|menu|dropdown|switcher)\b/.test(marker);
}

export async function selectRequestedModel(root, requested, options = {}) {
  const readiness = await waitForModelSelectionTarget(root, requested, options);
  if (readiness.selection.selected) {
    return {
      status: "selected",
      model_used: readiness.selection.model_used,
      available_options: []
    };
  }

  const modelButton = readiness.modelButton;
  if (!modelButton) {
    return {
      status: "unavailable",
      model_used: readiness.selection.model_used,
      warning: "ChatGPT model selector button not found"
    };
  }

  const currentSelection = currentRequestedModelSelection(root, requested, options);
  if (currentSelection.selected) {
    return {
      status: "selected",
      model_used: currentSelection.model_used,
      available_options: []
    };
  }

  let selectedOption = null;
  let availableOptions = [];
  const openAttempts = 3;
  for (let attempt = 0; attempt < openAttempts; attempt += 1) {
    await openModelPicker(root, modelButton);
    const result = await waitForRequestedModelOption(root, requested, {
      timeoutMs: attempt === openAttempts - 1 ? 7000 : 2500,
      stableForMs: attempt === openAttempts - 1 ? 0 : 600
    });
    selectedOption = result.option;
    availableOptions = result.availableOptions;
    if (selectedOption) {
      break;
    }
  }
  if (!selectedOption) {
    const verification = await waitForRequestedModelSelected(root, requested, null, options);
    if (verification.selected) {
      return {
        status: "selected",
        model_used: verification.model_used,
        available_options: availableOptions
      };
    }
    return {
      status: "unavailable",
      model_used: currentModelLabel(root, options),
      available_options: availableOptions,
      warning: "ChatGPT Pro Extended was not visible in the model picker"
    };
  }
  realClick(selectedOption);

  const verification = await waitForRequestedModelSelected(root, requested, selectedOption, options);
  if (verification.selected) {
    return {
      status: "selected",
      model_used: verification.model_used || textOf(selectedOption),
      available_options: availableOptions
    };
  }
  return {
    status: "mismatch",
    model_used: verification.model_used || "unknown",
    available_options: availableOptions,
    warning: `ChatGPT Pro Extended was clicked but selected label is ${verification.model_used || "unknown"}`
  };
}

async function waitForModelSelectionTarget(root, requested, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? 30000);
  const intervalMs = Number(options.intervalMs ?? 250);
  const startedAt = Date.now();
  let selection = currentRequestedModelSelection(root, requested, options);
  let modelButton = findModelButton(root, options);

  while (Date.now() - startedAt < timeoutMs) {
    selection = currentRequestedModelSelection(root, requested, options);
    if (selection.selected) {
      return { selection, modelButton: findModelButton(root, options) };
    }
    modelButton = findModelButton(root, options);
    if (modelButton) {
      return { selection, modelButton };
    }
    await sleep(intervalMs);
  }

  return { selection, modelButton };
}

function currentRequestedModelSelection(root, requested, options = {}) {
  const modelUsed = currentModelLabel(root, options);
  return {
    selected: modelTextMatchesRequest(modelUsed, requested),
    model_used: modelUsed
  };
}

async function openModelPicker(root, modelButton, options = {}) {
  const settleMs = Number(options.settleMs ?? 150);
  if (visibleModelOptions(root).length > 0) {
    return true;
  }
  const activators = [openWithPointerEvents, pressEnter, pressSpace];
  for (const activate of activators) {
    try {
      if (await activate(root, modelButton, { settleMs })) {
        return true;
      }
    } catch {
      // Try the next activation path; ChatGPT changes this control frequently.
    }
  }
  return false;
}

async function openWithPointerEvents(root, element, options = {}) {
  element?.focus?.();
  const settleMs = Number(options.settleMs ?? 150);
  const phases = [
    ["pointerdown", "PointerEvent", {
      button: 0,
      buttons: 1,
      pointerId: 1,
      pointerType: "mouse",
      isPrimary: true
    }],
    ["mousedown", "MouseEvent", { button: 0, buttons: 1 }],
    ["pointerup", "PointerEvent", {
      button: 0,
      buttons: 0,
      pointerId: 1,
      pointerType: "mouse",
      isPrimary: true
    }],
    ["mouseup", "MouseEvent", { button: 0, buttons: 0 }],
    ["click", "MouseEvent", { button: 0, buttons: 0, detail: 1 }]
  ];
  for (const [type, constructorName, init] of phases) {
    dispatchSyntheticEvent(element, type, constructorName, init);
    await sleep(settleMs);
    if (visibleModelOptions(root).length > 0) {
      return true;
    }
  }
  return false;
}

async function pressEnter(root, element, options = {}) {
  pressActivationKey(element, "Enter");
  await sleep(Number(options.settleMs ?? 150));
  return visibleModelOptions(root).length > 0;
}

async function pressSpace(root, element, options = {}) {
  pressActivationKey(element, " ");
  await sleep(Number(options.settleMs ?? 150));
  return visibleModelOptions(root).length > 0;
}

function realClick(element) {
  element?.focus?.();
  dispatchSyntheticEvent(element, "pointerdown", "PointerEvent", {
    button: 0,
    buttons: 1,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true
  });
  dispatchSyntheticEvent(element, "mousedown", "MouseEvent", {
    button: 0,
    buttons: 1
  });
  dispatchSyntheticEvent(element, "pointerup", "PointerEvent", {
    button: 0,
    buttons: 0,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true
  });
  dispatchSyntheticEvent(element, "mouseup", "MouseEvent", {
    button: 0,
    buttons: 0
  });
  dispatchSyntheticEvent(element, "click", "MouseEvent", {
    button: 0,
    buttons: 0,
    detail: 1
  });
}

function pressActivationKey(element, key) {
  const code = key === " " ? "Space" : key;
  element?.focus?.();
  dispatchSyntheticEvent(element, "keydown", "KeyboardEvent", { key, code });
  dispatchSyntheticEvent(element, "keyup", "KeyboardEvent", { key, code });
}

function dispatchSyntheticEvent(element, type, constructorName, init = {}) {
  const win = element?.ownerDocument?.defaultView ?? globalThis;
  const EventConstructor = win?.[constructorName] ?? globalThis[constructorName] ?? win?.Event ?? globalThis.Event;
  if (typeof EventConstructor !== "function") {
    return false;
  }
  const eventInit = {
    bubbles: true,
    cancelable: true,
    composed: true,
    view: win,
    ...init
  };
  let event = null;
  try {
    event = new EventConstructor(type, eventInit);
  } catch {
    event = new Event(type, eventInit);
  }
  return element.dispatchEvent?.(event) ?? false;
}

export async function clickSend(root, options = {}) {
  const requestedTimeoutMs = Number(options.timeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS);
  const minTimeoutMs = Number(options.minTimeoutMs ?? DEFAULT_SEND_MIN_TIMEOUT_MS);
  const timeoutMs = Math.max(requestedTimeoutMs, minTimeoutMs);
  const intervalMs = Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS);
  const startedAt = Date.now();
  let lastCandidate = null;

  while (Date.now() - startedAt < timeoutMs) {
    const button = findSendButtonControl(root, { requireEnabled: true });
    if (button) {
      button.click();
      return true;
    }
    lastCandidate = findSendButtonControl(root, { requireEnabled: false }) ?? lastCandidate;
    await sleep(intervalMs);
  }

  if (lastCandidate) {
    throw new Error(`ChatGPT send button remained disabled (${describeElement(lastCandidate)}; ${sendReadinessDiagnostics(root)})`);
  }
  throw new Error(`ChatGPT send button not found (${sendReadinessDiagnostics(root)})`);
}

export function sendAcceptanceBaseline(root = document) {
  const composer = findComposer(root);
  return {
    user_count: findUserTurns(root).length,
    assistant_count: findAssistantTurns(root).length,
    is_generating: isResponseGenerating(root),
    composer_text_chars: editableText(composer).length
  };
}

export async function waitForSendAccepted(root, baseline = {}, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS);
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const current = sendAcceptanceBaseline(root);
    if (current.user_count > Number(baseline.user_count ?? 0)) {
      return { send_acceptance_signal: "user_turn" };
    }
    if (current.assistant_count > Number(baseline.assistant_count ?? 0)) {
      return { send_acceptance_signal: "assistant_turn" };
    }
    if (!baseline.is_generating && current.is_generating) {
      return { send_acceptance_signal: "stop_control" };
    }
    await sleep(intervalMs);
  }
  throw new Error(`ChatGPT did not accept the prompt after send click (${sendReadinessDiagnostics(root)})`);
}

export function extractResponse(root = document) {
  const userTurns = findUserTurns(root);
  const assistantTurns = findAssistantTurns(root);
  const copyButtons = Array.from(root.querySelectorAll('button[aria-label*="Copy"], button[data-testid*="copy"]'));
  const assistantCopyButtons = copyButtons.filter((button) => isCopyControl(button) && assistantTurnForNode(button));
  const copyButtonCount = assistantCopyButtons.length;
  const latestTextEntry = latestTextBearingAssistantTurn(assistantTurns);
  const latestAssistant = latestTextEntry?.turn ?? assistantTurns.at(-1);
  const turnIndex = latestTextEntry?.index ?? (latestAssistant ? assistantTurns.length - 1 : -1);
  const latestUser = userTurns.at(-1);
  const latestTextConversation = latestTextEntry?.node ? responseConversationScope(latestTextEntry.node, latestUser) : null;
  const latestTextHasCopyButton = latestTextEntry?.node
    ? Boolean(
        latestTextConversation
          && copyButtons.some((button) => isScopedResponseCopyButton(root, button, latestUser, latestTextConversation, {
            responseNode: latestTextEntry.node,
            responseTurn: latestAssistant
          }))
      )
    : false;
  const latestTurnHasCopyButton = latestTextHasCopyButton
    || assistantCopyButtons.some((button) => sameAssistantTurn(assistantTurnForNode(button), latestAssistant));
  const diagnostics = extractionDiagnostics(root, assistantTurns, copyButtons);
  const scopedText = latestTextEntry?.text ?? assistantMessageText(latestAssistant);
  if (scopedText) {
    return {
      method: latestTurnHasCopyButton ? "copy_scope_dom_fallback" : "assistant_dom_fallback",
      text: scopedText,
      is_generating: isResponseGenerating(root),
      assistant_count: assistantTurns.length,
      user_count: userTurns.length,
      preceding_user_count: precedingTurnCount(root, latestAssistant, userTurns),
      copy_button_count: copyButtonCount,
      has_copy_button: latestTurnHasCopyButton,
      turn_index: turnIndex,
      diagnostics
    };
  }

  const standalone = latestStandaloneAssistantMarkdown(root, userTurns, copyButtons);
  if (standalone) {
    const assistantCount = Math.max(assistantTurns.length, 1);
    return {
      method: standalone.hasCopyButton ? "copy_scope_dom_fallback" : "assistant_dom_fallback",
      text: standalone.text,
      is_generating: isResponseGenerating(root),
      assistant_count: assistantCount,
      user_count: userTurns.length,
      preceding_user_count: precedingTurnCount(root, standalone.node, userTurns),
      copy_button_count: Math.max(copyButtonCount, standalone.hasCopyButton ? 1 : 0),
      has_copy_button: standalone.hasCopyButton,
      turn_index: assistantCount - 1,
      diagnostics
    };
  }

  return {
    method: "page_text_fallback",
    text: normalizeText(getPageText(root)),
    is_generating: isResponseGenerating(root),
    assistant_count: assistantTurns.length,
    user_count: userTurns.length,
    preceding_user_count: -1,
    copy_button_count: copyButtonCount,
    has_copy_button: copyButtonCount > 0,
    turn_index: -1,
    diagnostics
  };
}

function latestTextBearingAssistantTurn(assistantTurns) {
  for (let index = assistantTurns.length - 1; index >= 0; index -= 1) {
    const turn = assistantTurns[index];
    const entry = assistantMessageTextEntry(turn);
    if (entry.text) {
      return { turn, index, ...entry };
    }
  }
  return null;
}

function latestStandaloneAssistantMarkdown(root, userTurns, copyButtons) {
  const latestUser = userTurns.at(-1);
  if (!latestUser) {
    return null;
  }
  const conversation = conversationScope(latestUser);
  if (!conversation) {
    return null;
  }
  const ordered = flattenTree(root.documentElement ?? root.body ?? root);
  const latestUserIndex = ordered.indexOf(latestUser);
  if (latestUserIndex < 0) {
    return null;
  }
  for (let index = ordered.length - 1; index > latestUserIndex; index -= 1) {
    const marker = ordered[index];
    if (marker?.getAttribute?.("data-message-author-role") !== "assistant"
      || !containsNode(conversation, marker)
      || isInsideUserTurn(marker)
      || isNonConversationChrome(marker)) {
      continue;
    }
    const segment = standaloneAssistantSegment(root, conversation, ordered, marker, latestUser, copyButtons);
    if (segment) {
      return segment;
    }
  }
  return null;
}

function standaloneAssistantSegment(root, conversation, ordered, marker, latestUser, copyButtons) {
  const markerIndex = ordered.indexOf(marker);
  if (markerIndex < 0) {
    return null;
  }
  const nextBoundaryIndex = nextRoleBoundaryIndex(ordered, markerIndex);
  const markdownNodes = leafNodes(Array.from(conversation.querySelectorAll('[class*="markdown"]')))
    .filter((node) => {
      const nodeIndex = ordered.indexOf(node);
      return nodeIndex > markerIndex
        && (nextBoundaryIndex < 0 || nodeIndex < nextBoundaryIndex)
        && nodePrecedes(root, latestUser, node)
        && isVisible(node, { allowDisabled: true })
        && !isInsideUserTurn(node)
        && !isNonConversationChrome(node);
    });
  const textEntries = markdownNodes
    .map((node) => ({ node, text: cleanAssistantText(node, { preserveContentStatusText: true }) }))
    .filter((entry) => entry.text);
  if (textEntries.length === 0) {
    return null;
  }
  const hasCopyButton = copyButtons.some((button) => isScopedResponseCopyButton(root, button, latestUser, conversation, {
    ordered,
    startIndex: markerIndex,
    nextBoundaryIndex
  }));
  return {
    node: textEntries.at(-1).node,
    text: normalizeText(textEntries.map((entry) => entry.text).join("\n\n")),
    hasCopyButton
  };
}

function nextRoleBoundaryIndex(ordered, startIndex) {
  for (let index = startIndex + 1; index < ordered.length; index += 1) {
    const role = ordered[index]?.getAttribute?.("data-message-author-role");
    if (role === "user" || role === "assistant") {
      return index;
    }
  }
  return -1;
}

function isScopedResponseCopyButton(root, button, latestUser, conversation, scope = {}) {
  if (!isCopyControl(button) || !isVisible(button, { allowDisabled: true, allowTransparent: true, allowPointerEventsNone: true, allowNoLayout: true })) {
    return false;
  }
  if (!containsNode(conversation, button) || isNonConversationChrome(button) || isInsideUserTurn(button)) {
    return false;
  }
  if (latestUser && !nodePrecedes(root, latestUser, button)) {
    return false;
  }
  if (scope.responseNode && sameResponseFrame(button, scope.responseNode)) {
    return true;
  }
  if (scope.responseTurn && sameAssistantTurn(assistantTurnForNode(button), scope.responseTurn)) {
    return true;
  }
  const ordered = scope.ordered ?? flattenTree(root.documentElement ?? root.body ?? root);
  const buttonIndex = ordered.indexOf(button);
  if (buttonIndex < 0) {
    return false;
  }
  if (Number.isInteger(scope.startIndex)) {
    return buttonIndex > scope.startIndex
      && (scope.nextBoundaryIndex < 0 || buttonIndex < scope.nextBoundaryIndex);
  }
  if (!scope.responseNode) {
    return false;
  }
  const responseIndex = ordered.indexOf(scope.responseNode);
  if (responseIndex < 0) {
    return false;
  }
  if (buttonIndex > responseIndex) {
    return !hasResponseBoundaryBetween(ordered, responseIndex, buttonIndex);
  }
  return !hasResponseBoundaryBetween(ordered, buttonIndex, responseIndex);
}

function hasResponseBoundaryBetween(ordered, startIndex, endIndex) {
  for (let index = startIndex + 1; index < endIndex; index += 1) {
    const node = ordered[index];
    const role = node?.getAttribute?.("data-message-author-role");
    if (role === "user" || role === "assistant") {
      return true;
    }
    if (isMarkdownNode(node) && cleanAssistantText(node, { preserveContentStatusText: true })) {
      return true;
    }
  }
  return false;
}

function sameResponseFrame(left, right) {
  const leftFrame = responseFrame(left);
  return Boolean(leftFrame && leftFrame === responseFrame(right));
}

function responseFrame(node) {
  return node?.closest?.('[data-testid*="conversation-turn"], article') ?? null;
}

function isInsideUserTurn(node) {
  return Boolean(node.closest?.('[data-message-author-role="user"], [class*="user-turn"]'));
}

function responseConversationScope(node, latestUser) {
  const userScope = latestUser ? conversationScope(latestUser) : null;
  if (userScope && containsNode(userScope, node)) {
    return userScope;
  }
  return conversationScope(node);
}

function conversationScope(node) {
  for (let current = node; current; current = current.parentElement) {
    // A turn can be labelled like a conversation; walk past it to the transcript root.
    if (isTurnLikeScope(current)) {
      continue;
    }
    if (isConversationContainer(current)) {
      return current;
    }
  }
  return null;
}

function isConversationContainer(node) {
  const marker = nodeMarker(node, ["tag", "role", "data-testid", "class"]);
  return /\bmain\b/i.test(marker)
    || /\bconversation\b/i.test(marker);
}

function isTurnLikeScope(node) {
  const marker = nodeMarker(node, ["data-message-author-role", "data-testid", "class"]);
  return /\b(user|assistant|conversation-turn|turn-messages|user-turn|agent-turn)\b/i.test(marker);
}

const CHROME_KEYWORD = /^(aside|nav|header|footer|complementary|navigation|dialog|sidebar|side-panel|popover|modal)$/i;

function isNonConversationChrome(node) {
  for (let current = node; current; current = current.parentElement) {
    // tag / role / data-testid / aria-label are semantic identifiers — match chrome keywords
    // as whole words there. The class attribute is NOT semantic: ChatGPT uses Tailwind utility
    // tokens that embed arbitrary CSS expressions (e.g. scroll-mt-[calc(var(--header-height)+...)]
    // on the conversation-turn <section>), so a substring \bheader\b inside var(--header-height)
    // would mis-flag a real answer turn as chrome and drop it to page_text_fallback. Match class
    // chrome keywords only as whole space-separated tokens so genuine chrome classes still match
    // while CSS-expression substrings do not.
    const semanticMarker = nodeMarker(current, ["tag", "role", "data-testid", "aria-label"]);
    if (/\b(aside|nav|header|footer|complementary|navigation|dialog|sidebar|side-panel|popover|modal)\b/i.test(semanticMarker)) {
      return true;
    }
    if (classTokensSignalChrome(current)) {
      return true;
    }
  }
  return false;
}

function classTokensSignalChrome(node) {
  const className = node?.getAttribute?.("class");
  if (!className) {
    return false;
  }
  // Match a chrome keyword only as a WHOLE space-separated class token (e.g. "popover", "modal",
  // "sidebar", "side-panel"). A Tailwind utility such as scroll-mt-[calc(var(--header-height)+...)]
  // is a single token that is not equal to any chrome keyword, so it no longer false-positives.
  return String(className)
    .split(/\s+/)
    .some((token) => CHROME_KEYWORD.test(token));
}

function nodeMarker(node, fields) {
  return fields
    .map((field) => field === "tag" ? node?.tagName?.toLowerCase?.() : node?.getAttribute?.(field))
    .filter(Boolean)
    .join(" ");
}

function assistantMessageText(turn) {
  return assistantMessageTextEntry(turn).text;
}

function assistantMessageTextEntry(turn) {
  if (!turn) {
    return { node: null, text: "" };
  }
  const contentNodes = [];
  const addContentNode = (node) => {
    if (node && !contentNodes.includes(node)) {
      contentNodes.push(node);
    }
  };
  if (isAssistantContentNode(turn, turn)) {
    addContentNode(turn);
  }
  for (const selector of [
    '[data-testid*="assistant-message"]',
    '[data-testid*="assistant-response"]',
    '[data-message-author-role="assistant"] [class*="markdown"]',
    '[class*="markdown"]'
  ]) {
    for (const node of Array.from(turn.querySelectorAll?.(selector) ?? [])) {
      if (!looksLikeUserTurn(node) && isAssistantContentNode(node, turn)) {
        addContentNode(node);
      }
    }
  }
  const leafContentNodes = leafNodes(contentNodes);
  const textEntries = [];
  for (const node of leafContentNodes) {
    const text = cleanAssistantText(node, { preserveContentStatusText: node !== turn });
    if (text) {
      textEntries.push({ node, text });
    }
  }
  if (textEntries.length > 0) {
    return {
      node: textEntries.at(-1).node,
      text: normalizeText(textEntries.map((entry) => entry.text).join("\n\n"))
    };
  }
  const fallback = cleanAssistantText(turn);
  return { node: turn, text: isModelStatusText(fallback) ? "" : fallback };
}

function isAssistantContentNode(node, turn = null) {
  if (!node || looksLikeUserTurn(node) || isInsideUserTurn(node) || isNonConversationChrome(node)) {
    return false;
  }
  const role = node.getAttribute?.("data-message-author-role");
  if (role === "assistant") {
    return true;
  }
  if (role === "user") {
    return false;
  }
  const testId = String(node.getAttribute?.("data-testid") ?? "");
  if (/assistant-(message|response)/i.test(testId)) {
    return true;
  }
  if (isMarkdownNode(node)) {
    return isAssistantMarkdownInTurn(node, turn ?? assistantTurnForNode(node));
  }
  return false;
}

function leafNodes(nodes) {
  return nodes.filter((node) => !nodes.some((other) => other !== node && containsNode(node, other)));
}

function cleanAssistantText(node, options = {}) {
  // Body text MUST come from textContent, not innerText: a virtualized/clipped long answer node
  // returns only its rendered head via innerText (observed live as a single "I"). textContent
  // returns the full DOM text regardless of layout. Per-line control/status stripping below then
  // removes any code-block "Copy code", sr-only, thought/status, and control lines that
  // textContent may surface. Fall back to innerText only if textContent is empty (defensive).
  const source = bodyTextOf(node) || textOf(node);
  const lines = source
    .split(/\n+/)
    .map((line) => normalizeText(line))
    .filter((line) => line && !isAssistantControlLine(line, options));
  return normalizeText(lines.join("\n"));
}

function isAssistantControlLine(line, options = {}) {
  const value = normalizeText(line);
  return /^(copy|copied|read aloud|share|regenerate|retry|edit|like|dislike)$/i.test(value)
    // Code-block affordances: textContent (unlike innerText) surfaces the code-block toolbar
    // button labels, which ChatGPT renders as "Copy code"/"Copy"/"Edit"/"Copy code button text".
    // Strip them as standalone lines so a fenced code block in the answer doesn't leak its
    // toolbar text. Anchored to a standalone line so it never eats real answer prose.
    || /^copy code$/i.test(value)
    || /^(thought|reasoned)\s+for\s+\S.*$/i.test(value)
    || /^show\s+(more|reasoning)$/i.test(value)
    || (!options.preserveContentStatusText && (isThoughtStatusLine(line) || isModelStatusText(line)));
}

function isModelStatusText(text) {
  const value = normalizeText(text);
  return /^(pro thinking|extended thinking|thinking|pro|extended pro)$/i.test(value)
    || /^gpt[\s.-]*\d+(?:[\s.-]*\d+)*(?:\s+(?:pro|thinking))?$/i.test(value);
}

function isMarkdownNode(node) {
  return /\bmarkdown\b/i.test(String(node?.getAttribute?.("class") ?? ""));
}

function isThoughtStatusLine(line) {
  const value = normalizeText(line);
  return /^(thought|reasoned)\s+for\s+\S.*$/i.test(value)
    || /^(analyzing|thinking|working|searching)[.…]*$/i.test(value)
    || /^show\s+(more|reasoning)$/i.test(value);
}

export function isResponseGenerating(root = document) {
  return Boolean(firstVisible(root, [
    'button[data-testid*="stop"]',
    'button[aria-label*="Stop generating" i]',
    'button[aria-label*="Stop streaming" i]'
  ]));
}

// Best-effort: click ChatGPT's visible stop-streaming/stop-generating control if
// one is rendered. Mirrors isResponseGenerating's selector list so we click the
// same affordance we use to detect ongoing generation. Returns true if a stop
// control was found and clicked, false if generation was already idle.
// Never throws — cancel is best-effort and a missing stop button is normal when
// the response has already settled or the page navigated away.
export function clickStopGenerating(root = document) {
  const button = firstVisible(root, [
    'button[data-testid*="stop"]',
    'button[aria-label*="Stop generating" i]',
    'button[aria-label*="Stop streaming" i]'
  ]);
  if (!button) {
    return false;
  }
  try {
    button.click();
    return true;
  } catch {
    return false;
  }
}

// Click stop, then WAIT for ChatGPT to actually go idle before reporting back.
// A bare clickStopGenerating only *initiates* ChatGPT's client-side abort to
// OpenAI; the service worker tearing the tab down microseconds later races that
// request and frequently never stops server-side generation. Polling
// isResponseGenerating until it clears both proves the abort registered in the
// UI and gives the abort request time to flush before the tab is removed.
//
// Bounded loop (default ≤5s, 250ms interval) with ONE extra re-click if the
// stop control is still present after the first interval (covers a click that
// landed on a stale/transitional button). Returns { stopped, confirmed_idle,
// waited_ms }. Never throws — cancel is best-effort and a torn-down or
// navigated page must not turn into a hard error.
export async function confirmGenerationStopped(root = document, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? 5000);
  const intervalMs = Number(options.intervalMs ?? 250);
  const startedAt = Date.now();
  let stopped = false;
  let reclicked = false;
  try {
    // Already idle (no stop control) → confirmed_idle without waiting.
    if (!isResponseGenerating(root)) {
      return { stopped: false, confirmed_idle: true, waited_ms: 0 };
    }
    stopped = clickStopGenerating(root);
    while (Date.now() - startedAt < timeoutMs) {
      await sleep(intervalMs);
      if (!isResponseGenerating(root)) {
        return { stopped, confirmed_idle: true, waited_ms: Date.now() - startedAt };
      }
      // Still generating after the first interval: re-click once in case the
      // first click hit a transitional control, then keep polling.
      if (!reclicked) {
        reclicked = true;
        if (clickStopGenerating(root)) {
          stopped = true;
        }
      }
    }
    // Timed out still generating: report not-idle so the caller can warn the
    // user the run may still be live server-side.
    return { stopped, confirmed_idle: false, waited_ms: Date.now() - startedAt };
  } catch {
    // Page torn down / navigated mid-poll. Treat as best-effort: we clicked
    // (maybe), we cannot confirm idle.
    return { stopped, confirmed_idle: false, waited_ms: Date.now() - startedAt };
  }
}

export function normalizeText(value) {
  return String(value ?? "")
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function firstVisible(root, selectors) {
  return firstMatching(root, selectors, { allowHidden: false });
}

function firstVisibleModelControl(root, selectors, options = {}) {
  for (const selector of selectors) {
    const nodes = Array.from(root.querySelectorAll(selector));
    const visible = nodes.find((node) =>
      isVisible(node, options) && !isTranscriptModelControl(node, options)
    );
    if (visible) {
      return visible;
    }
  }
  return null;
}

function isTranscriptModelControl(node, options = {}) {
  if (options.allowStandaloneFallback !== false) {
    return false;
  }
  return Boolean(node?.closest?.([
    "[data-message-author-role]",
    "article",
    '[data-testid*="conversation-turn"]',
    '[class*="turn-messages"]',
    '[class*="agent-turn"]',
    '[class*="user-turn"]'
  ].join(",")));
}

function firstMatching(root, selectors, options = {}) {
  for (const selector of selectors) {
    const nodes = Array.from(root.querySelectorAll(selector));
    const visible = nodes.find((node) => options.allowHidden
      ? isEnabled(node)
      : isVisible(node, options));
    if (visible) {
      return visible;
    }
  }
  return null;
}

function firstVisibleInComposerScopes(root, selectors) {
  return firstInComposerScopes(root, selectors, { allowHidden: false });
}

function firstInComposerScopes(root, selectors, options = {}) {
  for (const scope of composerScopes(root, { includeRoot: Boolean(options.includeRoot) })) {
    const visible = firstMatching(scope, selectors, options);
    if (visible) {
      return visible;
    }
  }
  return null;
}

function composerScopes(root, options = {}) {
  const composer = findComposer(root);
  const scopes = [];
  const add = (scope) => {
    if (scope && !scopes.includes(scope)) {
      scopes.push(scope);
    }
  };
  add(composer?.closest("form"));
  add(composer?.closest('[data-testid*="composer"], [class*="composer"], main, [role="main"]'));
  add(composer?.parentElement);
  if (options.includeRoot) {
    add(root);
  }
  return scopes;
}

function findSendButtonControl(root, { requireEnabled } = {}) {
  const selectors = [
    'button[data-testid="send-button"]',
    'button[data-testid="fruitjuice-send-button"]',
    'button[aria-label*="Send" i]',
    'button[title*="Send" i]',
    'form button[type="submit"]:last-of-type',
    'button[type="submit"]'
  ];
  for (const scope of composerScopes(root, { includeRoot: false })) {
    for (const selector of selectors) {
      const candidate = Array.from(scope.querySelectorAll(selector))
        .find((node) => isSendButtonCandidate(node, { requireEnabled }));
      if (candidate) {
        return candidate;
      }
    }

    const fallback = Array.from(scope.querySelectorAll("button"))
      .find((node) => isSendButtonCandidate(node, { requireEnabled }));
    if (fallback) {
      return fallback;
    }
  }
  return null;
}

function isSendButtonCandidate(node, { requireEnabled } = {}) {
  if (!isVisible(node, { allowDisabled: true })) {
    return false;
  }
  if (requireEnabled && !isEnabled(node)) {
    return false;
  }
  const text = [
    node.getAttribute?.("data-testid"),
    node.getAttribute?.("aria-label"),
    node.getAttribute?.("title"),
    node.getAttribute?.("type"),
    textOf(node)
  ].filter(Boolean).join(" ").toLowerCase();
  if (!text) {
    return false;
  }
  if (/\b(stop|cancel|voice|microphone|dictate|attach|upload|file|model|menu)\b/.test(text)) {
    return false;
  }
  return /\bsend\b|submit/.test(text);
}

async function waitForElement(root, finder, description, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS);
  const startedAt = Date.now();
  let element = finder(root);
  while (!element && Date.now() - startedAt < timeoutMs) {
    await sleep(intervalMs);
    element = finder(root);
  }
  if (!element) {
    throw new Error(`${description} not found`);
  }
  return element;
}

async function waitForCondition(predicate, description, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS);
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    if (predicate()) {
      return true;
    }
    await sleep(intervalMs);
  }
  throw new Error(description);
}

function findModelOption(root, labels) {
  const options = visibleModelOptions(root);
  for (const slug of labels.slugs) {
    const exact = options.find((node) => optionSlugs(node).includes(slug));
    if (exact) {
      return exact;
    }
  }
  for (const label of labels.labels) {
    const textMatch = options.find((node) => modelOptionMatchesLabel(node, label));
    if (textMatch) {
      return textMatch;
    }
  }
  return null;
}

function visibleModelOptions(root) {
  return Array.from(root.querySelectorAll('[role="menuitem"], [role="menuitemradio"], [role="option"], [data-testid^="model-switcher-"]:not([data-testid="model-switcher-selected-model"])'))
    .filter((node) => isVisible(node) && !/model-switcher-(dropdown-button|selected-model)$/i.test(String(node.getAttribute?.("data-testid") ?? "")));
}

function visibleModelOptionLabels(root) {
  return visibleModelOptions(root)
    .map((node) => optionText(node))
    .filter(Boolean);
}

async function waitForRequestedModelOption(root, requested, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? 7000);
  const intervalMs = Number(options.intervalMs ?? 100);
  const stableForMs = Number(options.stableForMs ?? 600);
  const startedAt = Date.now();
  let lastSignature = "";
  let stableSince = 0;
  let availableOptions = [];

  while (Date.now() - startedAt < timeoutMs) {
    const option = findModelOption(root, requested);
    availableOptions = visibleModelOptionLabels(root);
    if (option) {
      return { option, availableOptions };
    }

    const signature = availableOptions.join("\n");
    const now = Date.now();
    if (signature !== lastSignature) {
      lastSignature = signature;
      stableSince = signature ? now : 0;
    }

    if (stableForMs > 0 && signature && stableSince > 0 && now - stableSince >= stableForMs) {
      return { option: null, availableOptions };
    }
    await sleep(intervalMs);
  }

  return { option: null, availableOptions };
}

async function waitForRequestedModelSelected(root, requested, _option, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? 4000);
  const intervalMs = Number(options.intervalMs ?? 100);
  const startedAt = Date.now();
  let modelUsed = currentModelLabel(root, options);

  while (Date.now() - startedAt < timeoutMs) {
    modelUsed = currentModelLabel(root, options);
    if (modelTextMatchesRequest(modelUsed, requested)) {
      return { selected: true, model_used: modelUsed };
    }
    await sleep(intervalMs);
  }

  return { selected: false, model_used: modelUsed };
}

function optionSlugs(node) {
  return [
    node.getAttribute?.("data-testid")?.replace(/^model-switcher-/, ""),
    node.getAttribute?.("aria-label"),
    node.getAttribute?.("title"),
    textOf(node)
  ]
    .filter(Boolean)
    .map(canonicalModelSlug)
    .filter(Boolean);
}

function optionText(node) {
  return [
    textOf(node),
    node.getAttribute?.("aria-label"),
    node.getAttribute?.("title"),
    node.getAttribute?.("data-testid")
  ].filter(Boolean).join(" ");
}

function modelTextMatchesRequest(text, requested) {
  if (!text) {
    return false;
  }
  const slug = canonicalModelSlug(text);
  return requested.slugs.includes(slug) || requested.labels.some((label) => modelLabelMatchesText(label, text));
}

function modelOptionMatchesLabel(node, label) {
  const labelSlug = canonicalModelSlug(label);
  return optionSlugs(node).includes(labelSlug) || modelLabelMatchesText(label, optionText(node));
}

function modelLabelMatchesText(label, text) {
  const labelSlug = canonicalModelSlug(label);
  const folded = normalizeText(text).toLowerCase();
  if (!labelSlug || !folded) {
    return false;
  }
  if (labelSlug === "pro" || labelSlug === "thinking") {
    return new RegExp(`\\b${labelSlug}\\b`).test(folded);
  }
  if (labelSlug === "gpt-5") {
    return /\bgpt[\s.-]*5\b(?![\s.-]*\d)/i.test(folded);
  }
  return canonicalModelSlug(text) === labelSlug;
}

async function waitForUploadComplete(root, file, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS);
  const baselineAttachments = options.baselineAttachments ?? new Set();
  const startedAt = Date.now();
  let lastState = "";
  while (Date.now() - startedAt < timeoutMs) {
    const error = uploadErrorText(root);
    if (error) {
      throw new Error(`ChatGPT file upload failed: ${error}`);
    }
    const attached = hasAttachmentNamed(root, file.name, baselineAttachments);
    const pending = hasUploadPending(root);
    lastState = `attached=${attached}, pending=${pending}, diagnostics=${sendReadinessDiagnostics(root)}`;
    if (attached && !pending) {
      return true;
    }
    await sleep(intervalMs);
  }
  throw new Error(`ChatGPT file upload did not complete for ${file.name} (${lastState})`);
}

async function openAttachmentUi(root, options = {}) {
  const button = findAttachmentButton(root);
  if (button) {
    button.click();
    await sleep(Number(options.attachmentMenuDelayMs ?? 250));
    const uploadItem = findUploadMenuItem(root);
    if (uploadItem) {
      uploadItem.click();
      await sleep(Number(options.attachmentMenuDelayMs ?? 250));
    }
  }
}

function findAttachmentButton(root) {
  for (const scope of composerScopes(root, { includeRoot: false })) {
    const candidate = firstVisible(scope, [
      'button[data-testid*="attach"]',
      'button[aria-label*="Attach" i]',
      'button[aria-label*="Upload" i]',
      'button[title*="Attach" i]',
      'button[title*="Upload" i]'
    ]);
    if (candidate) {
      return candidate;
    }
    const fallback = Array.from(scope.querySelectorAll("button"))
      .find((node) => isVisible(node) && /\b(attach|upload|file)\b/i.test(textOf(node)));
    if (fallback) {
      return fallback;
    }
  }
  return null;
}

function findUploadMenuItem(root) {
  const candidates = Array.from(root.querySelectorAll('[role="menuitem"], [role="option"], button'));
  return candidates.find((node) => isVisible(node)
    && /\b(upload|attach|file|computer)\b/i.test([
      textOf(node),
      node.getAttribute?.("aria-label"),
      node.getAttribute?.("title")
    ].filter(Boolean).join(" ")));
}

function findNewChatControl(root) {
  return firstVisible(root, [
    'a[href="/"]',
    'button[data-testid="create-new-chat-button"]',
    'button[aria-label*="New chat" i]',
    'a[aria-label*="New chat" i]'
  ]);
}

function findAttachmentTiles(root, options = {}) {
  const scopes = options.composerOnly ? composerScopes(root, { includeRoot: false }) : [root];
  return uniqueElements(scopes.flatMap((scope) => Array.from(scope.querySelectorAll('[class*="file-tile"], [data-testid*="attachment"]'))))
    .filter((node) => isVisible(node, { allowDisabled: true }));
}

function hasAttachmentNamed(root, filename, baselineAttachments = new Set()) {
  const needle = normalizeText(filename).toLowerCase();
  if (!needle) {
    return false;
  }
  const candidates = findAttachmentCandidates(root);
  return candidates.some((node) => {
    if (baselineAttachments.has(attachmentNodeKey(node))) {
      return false;
    }
    const text = [
      textOf(node),
      node.getAttribute?.("aria-label"),
      node.getAttribute?.("title")
    ].filter(Boolean).join(" ");
    return normalizeText(text).toLowerCase().includes(needle);
  });
}

// Broad candidate selector shared by upload baseline capture and the
// post-upload `hasAttachmentNamed` check. Keeping these in sync ensures any
// pre-existing composer-scoped node whose text contains the filename is
// recorded in the baseline and excluded from the per-tick match — otherwise a
// stale span/div bearing the bundle filename would falsely satisfy
// hasAttachmentNamed before the real upload tile appears.
function findAttachmentCandidates(root) {
  return uniqueElements(composerScopes(root, { includeRoot: false }).flatMap((scope) => Array.from(scope.querySelectorAll([
    '[class*="file-tile"]',
    '[data-testid*="attachment"]',
    '[aria-label]',
    '[title]',
    'span',
    'div'
  ].join(",")))));
}

function attachmentNodeKeys(nodes) {
  return new Set(nodes.map(attachmentNodeKey));
}

function attachmentNodeKey(node) {
  return [
    node.getAttribute?.("data-testid"),
    node.getAttribute?.("aria-label"),
    node.getAttribute?.("title"),
    textOf(node)
  ].filter(Boolean).join("|");
}

function hasUploadPending(root) {
  const pending = firstVisible(root, [
    '[role="progressbar"]',
    '[aria-busy="true"]',
    '[data-testid*="upload"][data-state*="loading"]',
    '[data-testid*="attachment"][data-state*="loading"]'
  ]);
  if (pending) {
    return true;
  }
  const candidates = Array.from(root.querySelectorAll("[aria-label], [role], [data-testid], button, span, div"));
  return candidates.some((node) => isVisible(node) && /\b(uploading|attaching|processing|scanning)\b/i.test(textOf(node)));
}

function uploadErrorText(root) {
  const candidates = Array.from(root.querySelectorAll('[role="alert"], [data-testid*="error"], [aria-live="assertive"]'));
  const error = candidates.find((node) => isVisible(node) && /\b(upload|attach|file|failed|error)\b/i.test(textOf(node)));
  return error ? textOf(error) : "";
}

function insertContenteditableText(root, composer, prompt) {
  const selection = root.defaultView?.getSelection?.();
  const range = root.createRange?.();
  if (selection && range) {
    range.selectNodeContents(composer);
    range.deleteContents();
    selection.removeAllRanges();
    selection.addRange(range);
  } else {
    composer.textContent = "";
  }
  if (!root.execCommand?.("insertText", false, prompt)) {
    composer.textContent = prompt;
  }
  dispatchTextInput(composer, "input", prompt);
}

function setInputValue(element, value) {
  const win = element.ownerDocument?.defaultView ?? globalThis;
  const prototypeName = element.tagName === "TEXTAREA"
    ? "HTMLTextAreaElement"
    : element.tagName === "INPUT"
      ? "HTMLInputElement"
      : null;
  const prototype = prototypeName ? win?.[prototypeName]?.prototype : null;
  const descriptor = prototype ? Object.getOwnPropertyDescriptor(prototype, "value") : null;
  if (descriptor?.set) {
    descriptor.set.call(element, value);
  } else {
    element.value = value;
  }
}

function dispatchTextInput(element, type, text) {
  let event;
  try {
    event = new InputEvent(type, {
      bubbles: true,
      inputType: "insertText",
      data: text
    });
  } catch {
    event = new Event(type, { bubbles: true });
  }
  element.dispatchEvent(event);
}

function composerContainsPrompt(composer, prompt) {
  const expected = normalizeText(prompt);
  if (!expected) {
    return true;
  }
  const actual = editableText(composer);
  return actual === expected;
}

function findUserTurns(root) {
  const explicitUserTurns = Array.from(root.querySelectorAll('[data-message-author-role="user"]'))
    .map((node) => node.closest?.('article, [data-testid*="conversation-turn"], [class*="user-turn"], [class*="turn-messages"]') ?? node);
  return uniqueElements(explicitUserTurns)
    .filter((node) => isVisible(node, { allowDisabled: true, allowNoLayout: true }));
}

function findAssistantTurns(root) {
  const explicitAssistantTurns = Array.from(root.querySelectorAll('[data-message-author-role="assistant"]'))
    .map((node) => assistantTurnForNode(node) ?? node);
  const markdownAssistantTurns = Array.from(root.querySelectorAll([
    '[data-testid*="assistant-message"]',
    '[data-testid*="assistant-response"]',
    '[data-message-author-role="assistant"] [class*="markdown"]',
    '[data-testid*="conversation-turn"] [class*="markdown"]',
    '[class*="agent-turn"] [class*="markdown"]'
  ].join(",")))
    .map((node) => assistantTurnForNode(node) ?? (isAssistantMarkerNode(node) ? node : null));
  const copyScopedTurns = Array.from(root.querySelectorAll('button[aria-label*="Copy"], button[data-testid*="copy"]'))
    .map((node) => assistantTurnForNode(node));
  return uniqueElements([...explicitAssistantTurns, ...markdownAssistantTurns, ...copyScopedTurns])
    .filter((node) => isVisible(node, { allowDisabled: true, allowNoLayout: true }));
}

function assistantTurnForNode(node) {
  if (!node) {
    return null;
  }
  const explicit = node.closest?.('[data-message-author-role="assistant"]');
  const turn = node.closest?.('article, [data-testid*="conversation-turn"], [class*="agent-turn"], [class*="turn-messages"]');
  if (explicit && turn && !looksLikeUserTurn(turn)) {
    if (!hasUserRoleDescendant(turn)) {
      return turn;
    }
  }
  if (explicit) {
    return explicit;
  }
  if (!turn) {
    return isAssistantMarkerNode(node) ? node : null;
  }
  const turnRole = turn.getAttribute?.("data-message-author-role");
  if (turnRole === "user") {
    return null;
  }
  if (turnRole === "assistant") {
    return turn;
  }
  if (hasUserRoleDescendant(turn)) {
    return null;
  }
  const assistantDescendants = Array.from(turn.querySelectorAll?.('[data-message-author-role="assistant"]') ?? []);
  if (assistantDescendants.length > 0) {
    return turn;
  }
  if (looksLikeUserTurn(turn)) {
    return null;
  }
  return isCopyControl(node) || isAssistantMarkerNode(node) || isAssistantMarkdownInTurn(node, turn) ? turn : null;
}

function hasUserRoleDescendant(node) {
  if (!node) {
    return false;
  }
  const queried = Array.from(node.querySelectorAll?.('[data-message-author-role="user"]') ?? []);
  if (queried.length > 0) {
    return true;
  }
  for (const child of Array.from(node.children ?? [])) {
    if (child.getAttribute?.("data-message-author-role") === "user" || hasUserRoleDescendant(child)) {
      return true;
    }
  }
  return false;
}

function hasAssistantRoleDescendant(node) {
  if (!node) {
    return false;
  }
  const queried = Array.from(node.querySelectorAll?.('[data-message-author-role="assistant"]') ?? []);
  if (queried.length > 0) {
    return true;
  }
  for (const child of Array.from(node.children ?? [])) {
    if (child.getAttribute?.("data-message-author-role") === "assistant" || hasAssistantRoleDescendant(child)) {
      return true;
    }
  }
  return false;
}

function hasConversationResidue(root) {
  const residue = conversationResidue(root);
  return residue.user_count > 0 || residue.assistant_count > 0 || residue.copy_button_count > 0;
}

function conversationUnavailableState(root, win) {
  const text = normalizeText(`${String(win.document?.title ?? "")}\n${conversationUnavailableSurfaceText(root)}`).toLowerCase();
  if (!text) {
    return null;
  }
  const unavailablePatterns = [
    /\bconversation not found\b/,
    /\bchat not found\b/,
    /\bconversation (?:is )?unavailable\b/,
    /\byou (?:do not|don't) have access to (?:this )?(?:conversation|chat)\b/,
    /\b(?:cannot|can't|could not) access (?:this )?(?:conversation|chat)\b/,
    /\bthis (?:conversation|chat) (?:has been )?archived\b/,
    /\barchived (?:conversation|chat)\b/
  ];
  const matched = unavailablePatterns.find((pattern) => pattern.test(text));
  return matched ? { reason: matched.source } : null;
}

function conversationUnavailableSurfaceText(root) {
  // Prior transcript text can quote "conversation not found" / "no access" phrases.
  // When transcript residue exists, keep page-level banners and other non-turn UI
  // text, but drop message containers so quoted assistant/user content cannot
  // mask a valid resumed conversation or create a false unavailable state.
  if (!hasConversationResidue(root)) {
    return getPageText(root);
  }
  const start = root.body ?? root.documentElement ?? root;
  const chunks = [];
  collectConversationUnavailableSurfaceText(start, chunks);
  return chunks.join("\n");
}

function collectConversationUnavailableSurfaceText(node, chunks) {
  if (!node || isConversationTurnSurface(node)) {
    return;
  }
  const children = Array.from(node.children ?? []);
  if (children.length === 0) {
    if (isVisible(node, { allowDisabled: true })) {
      const text = textOf(node);
      if (text) {
        chunks.push(text);
      }
    }
    return;
  }
  for (const child of children) {
    collectConversationUnavailableSurfaceText(child, chunks);
  }
}

function isConversationTurnSurface(node) {
  return Boolean(node?.closest?.([
    "[data-message-author-role]",
    "article",
    '[data-testid*="conversation-turn"]',
    '[class*="turn-messages"]',
    '[class*="agent-turn"]',
    '[class*="user-turn"]'
  ].join(",")));
}

function conversationResidue(root) {
  const copyButtons = Array.from(root.querySelectorAll('button[aria-label*="Copy"], button[data-testid*="copy"]'))
    .filter((node) => isCopyControl(node));
  return {
    user_count: findUserTurns(root).length,
    assistant_count: findAssistantTurns(root).length,
    copy_button_count: copyButtons.length
  };
}

function precedingTurnCount(root, turn, candidates) {
  if (!turn) {
    return -1;
  }
  return candidates.filter((candidate) => nodePrecedes(root, candidate, turn)).length;
}

function nodePrecedes(root, left, right) {
  if (!left || !right || left === right) {
    return false;
  }
  if (typeof left.compareDocumentPosition === "function") {
    const following = root.defaultView?.Node?.DOCUMENT_POSITION_FOLLOWING ?? 4;
    return Boolean(left.compareDocumentPosition(right) & following);
  }
  const ordered = flattenTree(root.documentElement ?? root.body ?? root);
  const leftIndex = ordered.indexOf(left);
  const rightIndex = ordered.indexOf(right);
  return leftIndex >= 0 && rightIndex >= 0 && leftIndex < rightIndex;
}

function flattenTree(node) {
  if (!node) {
    return [];
  }
  return [node, ...Array.from(node.children ?? []).flatMap(flattenTree)];
}

function sameAssistantTurn(left, right) {
  return Boolean(left && right && (
    left === right
    || containsNode(left, right)
    || containsNode(right, left)
  ));
}

function containsNode(parent, child) {
  if (!parent || !child) {
    return false;
  }
  if (typeof parent.contains === "function") {
    return parent.contains(child);
  }
  for (const node of Array.from(parent.children ?? [])) {
    if (node === child || containsNode(node, child)) {
      return true;
    }
  }
  return false;
}

function isCopyControl(node) {
  if (isCodeCopyControl(node)) {
    return false;
  }
  return /\bcopy\b/i.test([
    node?.getAttribute?.("aria-label"),
    node?.getAttribute?.("data-testid"),
    node?.getAttribute?.("title"),
    textOf(node)
  ].filter(Boolean).join(" "));
}

function isCodeCopyControl(node) {
  return Boolean(node?.closest?.('pre, code, [class*="code"], [data-testid*="code"]'));
}

function isAssistantMarkerNode(node) {
  const role = node?.getAttribute?.("data-message-author-role");
  if (role === "assistant") {
    return true;
  }
  const testId = String(node?.getAttribute?.("data-testid") ?? "");
  return /assistant-(message|response)/i.test(testId);
}

function isAssistantMarkdownInTurn(node, turn) {
  const marker = [
    node?.getAttribute?.("class"),
    turn?.getAttribute?.("class"),
    turn?.getAttribute?.("data-testid")
  ].filter(Boolean).join(" ");
  return /\bmarkdown\b/i.test(marker)
    && (
      /\bagent-turn\b/i.test(marker)
      || /\bassistant\b/i.test(marker)
      || /\bconversation-turn\b/i.test(marker)
      || turn?.getAttribute?.("data-message-author-role") === "assistant"
      || hasAssistantRoleDescendant(turn)
    );
}

function looksLikeUserTurn(turn) {
  const marker = [
    turn?.getAttribute?.("data-message-author-role"),
    turn?.getAttribute?.("class"),
    turn?.getAttribute?.("data-testid")
  ].filter(Boolean).join(" ");
  return /\buser\b/i.test(marker);
}

function extractionDiagnostics(root, assistantTurns, copyButtons) {
  const pageText = normalizeText(getPageText(root));
  // page_text_chars is innerText-derived (getPageText = body.innerText) and therefore
  // UNDER-reports when ChatGPT virtualizes/clips long turns. page_text_content_chars is the
  // textContent length, which is layout-independent. A large gap between the two on a completed
  // turn is the discriminator for the "extracted only a single char" failure mode: if
  // textContent >> innerText, the answer is present but innerText-truncated (extraction bug,
  // recovered by the textContent body reader); if both are tiny, the model genuinely produced
  // little text.
  const pageTextContentChars = normalizeText(root.body?.textContent ?? root.documentElement?.textContent ?? "").length;
  return {
    page_text_chars: pageText.length,
    page_text_content_chars: pageTextContentChars,
    body_text_tail: pageText.slice(-500),
    counts: {
      articles: root.querySelectorAll("article").length,
      assistant_roles: root.querySelectorAll('[data-message-author-role="assistant"]').length,
      user_roles: root.querySelectorAll('[data-message-author-role="user"]').length,
      markdown: root.querySelectorAll('[class*="markdown"]').length,
      conversation_turns: root.querySelectorAll('[data-testid*="conversation-turn"]').length,
      agent_turns: root.querySelectorAll('[class*="agent-turn"]').length,
      stop_controls: root.querySelectorAll('button[data-testid*="stop"], button[aria-label*="Stop generating" i], button[aria-label*="Stop streaming" i]').length,
      copy_buttons: copyButtons.length,
      assistant_turns: assistantTurns.length
    },
    assistant_turn_snippets: assistantTurns.slice(-3).map(elementSummary),
    article_snippets: Array.from(root.querySelectorAll("article")).slice(-5).map(elementSummary),
    markdown_snippets: Array.from(root.querySelectorAll('[class*="markdown"]')).slice(-5).map(elementSummary),
    stop_control_snippets: Array.from(root.querySelectorAll('button[data-testid*="stop"], button[aria-label*="Stop generating" i], button[aria-label*="Stop streaming" i]')).slice(0, 5).map(elementSummary)
  };
}

function elementSummary(node) {
  return {
    tag: node?.tagName?.toLowerCase?.() ?? "element",
    role: String(node?.getAttribute?.("data-message-author-role") ?? ""),
    testid: String(node?.getAttribute?.("data-testid") ?? "").slice(0, 120),
    class: String(node?.getAttribute?.("class") ?? "").slice(0, 160),
    aria: String(node?.getAttribute?.("aria-label") ?? "").slice(0, 160),
    text_chars: textOf(node).length,
    // text_content_chars exposes the layout-independent textContent length next to the
    // innerText-derived text_chars. On the truncation failure mode this node will show
    // text_chars=1 ("I") while text_content_chars holds the full answer length — a single
    // native inspect then settles whether "I" is an extraction artifact or genuine output.
    text_content_chars: bodyTextOf(node).length,
    text: textOf(node).slice(0, 240)
  };
}

function uniqueElements(nodes) {
  return nodes.filter((node, index) => node && nodes.indexOf(node) === index);
}

function currentModelLabel(root, options = {}) {
  const modelButton = findModelButton(root, options);
  const selected = options.allowStandaloneFallback === false
    ? (modelButton ? firstVisible(modelButton, ['[data-testid="model-switcher-selected-model"]']) : null)
    : firstVisible(root, ['[data-testid="model-switcher-selected-model"]']);
  const selectedText = normalizeText(selected?.innerText ?? selected?.textContent ?? "");
  if (selectedText) {
    return selectedText;
  }
  if (visibleModelOptions(root).length > 0) {
    return "";
  }
  return modelControlLabel(modelButton);
}

export function modelSelectionDiagnostics(root = document) {
  const requested = proExtendedModelRequest();
  const modelButton = findModelButton(root);
  const modelUsed = currentModelLabel(root);
  return {
    requested_model: requested.raw,
    current_model_label: modelUsed,
    current_matches_requested: modelTextMatchesRequest(modelUsed, requested),
    model_button: modelButton ? elementSummary(modelButton) : null,
    visible_options: visibleModelOptionLabels(root).slice(0, 20),
    composer: elementSummary(findComposer(root)),
    model_control_scopes: modelControlScopes(root).slice(0, 5).map(elementSummary)
  };
}

function proExtendedModelRequest() {
  return {
    raw: "extended-pro",
    labels: ["Extended Pro"],
    slugs: ["extended-pro"]
  };
}

function canonicalModelSlug(value) {
  const folded = normalizeText(value).toLowerCase();
  if (folded === "pro") return "pro";
  if (/\bextended\b/.test(folded) && /\bpro\b/.test(folded)) return "extended-pro";
  if (folded.includes("thinking")) return "thinking";
  const match = folded.match(/^gpt[\s-]*(\d+)(?:[.\s-]*(\d+))?(?:[.\s-]*(\d+))?(?:[\s.-]*(pro))?$/);
  if (!match) return folded.replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  const version = [match[1], match[2], match[3]].filter(Boolean).join("-");
  return `gpt-${version}${match[4] ? "-pro" : ""}`;
}

function includesFolded(value, needle) {
  return normalizeText(value).toLowerCase().includes(normalizeText(needle).toLowerCase());
}

function textOf(node) {
  const inner = normalizeText(node?.innerText ?? "");
  return inner || normalizeText(node?.textContent ?? "");
}

// Body-text reader for assistant ANSWER content nodes (markdown leaves), NOT control/label
// nodes. Uses textContent, not innerText, on purpose: ChatGPT virtualizes/clips long assistant
// turns, so innerText (layout-dependent) returns only the rendered head — observed live as a
// single "I" for a completed 955 KB Pro review while the full answer sat in textContent. Because
// callers pass markdown LEAF nodes (the [class*="markdown"] answer content, never the turn
// container) and the result is still run through cleanAssistantText's per-line control/status
// stripping, this recovers the full answer without pulling in reasoning/thinking-block text or
// code-block "Copy code"/sr-only control lines (those are separate nodes and/or stripped lines).
function bodyTextOf(node) {
  return normalizeText(node?.textContent ?? "");
}

function editableText(node) {
  if (!node) {
    return "";
  }
  if ("value" in node) {
    return normalizeText(node.value);
  }
  return textOf(node);
}

function sendReadinessDiagnostics(root) {
  const composer = findComposer(root);
  const attachmentTiles = Array.from(root.querySelectorAll('[class*="file-tile"], [data-testid*="attachment"]'))
    .filter((node) => isVisible(node, { allowDisabled: true }))
    .slice(0, 3)
    .map((node) => ({
      text: textOf(node).slice(0, 120),
      ariaLabel: String(node.getAttribute?.("aria-label") ?? "").slice(0, 120),
      testId: String(node.getAttribute?.("data-testid") ?? "").slice(0, 80),
      busy: node.getAttribute?.("aria-busy") === "true"
        || /uploading|processing|attaching|scanning/i.test(textOf(node))
    }));
  const alerts = Array.from(root.querySelectorAll('[role="alert"], [aria-live], [data-testid*="error"]'))
    .filter((node) => isVisible(node, { allowDisabled: true }))
    .map((node) => textOf(node).slice(0, 160))
    .filter(Boolean)
    .slice(0, 3);
  return JSON.stringify({
    composer: composer ? describeElement(composer) : null,
    composer_text_chars: textOf(composer).length || String(composer?.value ?? "").length,
    attachment_tiles: attachmentTiles,
    alerts
  }).slice(0, 800);
}

function describeElement(node) {
  const attrs = ["data-testid", "aria-label", "title", "type", "aria-disabled", "disabled"]
    .map((name) => {
      const value = node.getAttribute?.(name);
      return value == null ? null : `${name}=${JSON.stringify(value)}`;
    })
    .filter(Boolean)
    .join(" ");
  return `${node.tagName?.toLowerCase?.() ?? "element"}${attrs ? ` ${attrs}` : ""}`;
}

function conversationIdFromPathname(pathname) {
  const match = String(pathname ?? "").match(/^\/c\/([^/?#]+)$/);
  if (!match) {
    return null;
  }
  try {
    return decodeURIComponent(match[1]);
  } catch {
    return null;
  }
}

function chatgptCommandError(code, message, detail = {}) {
  const error = new Error(message);
  error.code = code;
  if (detail.phase) {
    error.phase = detail.phase;
  }
  if (typeof detail.side_effect_started === "boolean") {
    error.side_effect_started = detail.side_effect_started;
  }
  for (const [key, value] of Object.entries(detail)) {
    if (!(key in error) && value !== undefined) {
      error[key] = value;
    }
  }
  return error;
}

function currentLocationForError(win) {
  return String(win.location?.href ?? win.location?.pathname ?? "(unknown)") || "(unknown)";
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function isVisible(element, options = {}) {
  if (!element) {
    return false;
  }
  if (!options.allowDisabled && !isEnabled(element)) {
    return false;
  }
  const usesHiddenAffordanceException = options.allowTransparent || options.allowPointerEventsNone || options.allowNoLayout;
  if (!usesHiddenAffordanceException && typeof element.checkVisibility === "function") {
    try {
      if (!element.checkVisibility({
        checkOpacity: true,
        checkVisibilityCSS: true,
        contentVisibilityAuto: true
      })) {
        return false;
      }
    } catch {
      if (!element.checkVisibility()) {
        return false;
      }
    }
  }
  let current = element;
  while (current) {
    if (current.hidden
      || current.getAttribute?.("hidden") != null
      || current.getAttribute?.("aria-hidden") === "true"
      || current.getAttribute?.("inert") != null) {
      return false;
    }
    const style = current.ownerDocument?.defaultView?.getComputedStyle?.(current);
    if (style && (
      style.visibility === "hidden"
      || style.display === "none"
      || (!options.allowTransparent && style.opacity === "0")
      || (!options.allowPointerEventsNone && style.pointerEvents === "none")
    )) {
      return false;
    }
    current = current.parentElement;
  }
  if (!options.allowNoLayout && typeof element.getClientRects === "function" && element.getClientRects().length === 0) {
    return false;
  }
  return true;
}

function isEnabled(element) {
  return !element.disabled
    && element.getAttribute?.("disabled") == null
    && element.getAttribute?.("aria-disabled") !== "true";
}

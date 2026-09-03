export const YOETZ_WINDOW_PREFIX = "yoetz-chatgpt-native:";
export const OWNERSHIP_ATTR = "data-yoetz-chatgpt-native-job";

import {
  readPicker,
  normalizeText,
  textOf,
  foldedModelText,
  foldedFamilyLabel,
  isFamilyOptionLabel,
  optionLabel,
  effortOptionDisabled,
  itemIsChecked,
  familyIsSol,
  modelPickerTriggerIsOpen,
  pickerSurfaceIsOpen,
  structurallyReadablePickerItem,
  menuRadioItems,
  familyMenuRadios,
  disabledProEffortOption,
  isSelectModelViewToggle,
  expandedSelectModelView,
  activeFamilyView as activeFamilyViewPure,
  visibleMenus,
  findMainModelMenu,
  isEffortMenuLabels,
  isMainModelMenu,
  findFamilySubmenu,
  looksLikeLegacyAdvancedPicker,
  looksLikePersonalPicker,
  findPersonalPickerSurface,
  surfaceHasParsableEffortSlider,
  hasSelectModelViewToggle,
  findAdvancedPickerSurface,
  hybridFamilyView,
  findSliderPickerSurface,
  sliderLooksLikePowerControl,
  sliderIsEffortControl,
  sliderEffortSnapshot,
  effortLabelNearSlider,
  structuralFamilyEvidence,
  structurallyOpenControlledSurfaceForTrigger,
  readMenuPickerState,
  readSliderPickerState,
  readPersonalPickerState,
  readStructurallyTrustedPickerState,
  classifyPickerSurface,
  surfaceHasEffortRows,
  pickerStateIsReady,
  findPickerState as findPickerStatePure,
  isSupportedPickerShape,
  effortIsChatProTier,
  advancedViewRows,
  sliderEffortDiagnostics,
  personalEffortDiagnostics,
  effortControlDiagnostics,
  effortDiagnostics
} from "./chatgpt-picker-reader.js";

// Re-export the helpers that moved to the reader so existing callers
// (tests, sites/chatgpt.js `export *`) keep resolving them from here.
export {
  normalizeText,
  readPicker
};
const DEFAULT_WAIT_TIMEOUT_MS = 15000;
const DEFAULT_WAIT_INTERVAL_MS = 250;
const DEFAULT_SEND_MIN_TIMEOUT_MS = 120000;
const MIN_IMPLICIT_SURFACE_ABSENCE_MS = 1500;
const CHATGPT_SOL_CHAT_PRO_MODEL = "gpt-5-6-sol-chat-pro";
const CHATGPT_SOL_FAMILY_LABEL = "GPT-5.6 Sol";

// findPickerState / activeFamilyView wrappers: the driver calls these with the
// pre-Wave-1 signature (root only). They locate the layout-dependent pill
// and leftover composer triggers (which stay in this module) and pass them
// into the pure reader so the reader never locates anything itself.
function findPickerState(root) {
  return findPickerStatePure(root, {
    pill: findModelButton(root),
    leftoverTriggers: openComposerPickerLeftovers(root).map((entry) => entry.trigger)
  });
}

function activeFamilyView(root, mainMenu, trigger) {
  return activeFamilyViewPure(root, mainMenu, trigger, findModelButton(root),
    openComposerPickerLeftovers(root).map((entry) => entry.trigger));
}
const CHAT_SURFACE_GROUP_SELECTOR = '[role="radiogroup"][aria-label="Select chat surface"]';
const CHAT_SURFACE_CHAT_SELECTOR = '[role="radio"][data-tpp-toggle-value="chatgpt"]';
const CHAT_SURFACE_WORK_SELECTOR = '[role="radio"][data-tpp-toggle-value="work"]';
const MANUAL_HANDOFF_SHELL_SELECTORS = Object.freeze([
  "nav",
  "aside",
  "header",
  "footer",
  '[role="navigation"]',
  '[role="complementary"]',
  '[data-testid*="sidebar"]',
  '[aria-label*="sidebar"]',
  '[class~="sidebar"]',
  '[class~="side-panel"]'
]);

export function ownedWindowName(job) {
  const base = `${YOETZ_WINDOW_PREFIX}${job.run_id}:${job.job_id}`;
  const workspaceId = String(job?.workspace_id ?? "");
  const ownershipNonce = String(job?.ownership_nonce ?? "");
  return workspaceId || ownershipNonce
    ? `${base}|${encodeURIComponent(workspaceId)}|${encodeURIComponent(ownershipNonce)}`
    : base;
}

export function parseOwnedWindowName(value) {
  if (typeof value !== "string" || !value.startsWith(YOETZ_WINDOW_PREFIX)) {
    return null;
  }
  const [identity, workspaceId, ownershipNonce] = value
    .slice(YOETZ_WINDOW_PREFIX.length)
    .split("|");
  const separator = identity.lastIndexOf(":");
  if (separator <= 0 || separator === identity.length - 1) {
    return null;
  }
  const parsed = {
    run_id: identity.slice(0, separator),
    job_id: identity.slice(separator + 1)
  };
  if (workspaceId) {
    parsed.workspace_id = decodeMarkerField(workspaceId);
  }
  if (ownershipNonce) {
    parsed.ownership_nonce = decodeMarkerField(ownershipNonce);
  }
  return parsed.workspace_id === null || parsed.ownership_nonce === null ? null : parsed;
}

function decodeMarkerField(value) {
  try {
    return decodeURIComponent(value);
  } catch {
    return null;
  }
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
  const pathname = manualHandoffPathname(url);
  const conversationRoute = /^\/c\/[^/]+$/.test(pathname);
  const normalizedTitle = conversationRoute ? "" : normalizeText(title).toLowerCase();
  const normalizedText = normalizeText(text).toLowerCase();
  const challengeRoute = /^\/cdn-cgi\/challenge-platform(?:\/|$)/.test(pathname);
  const challengeTitle = /^(?:just a moment(?:\.\.\.)?|checking your browser(?:\.\.\.)?)(?:\s*(?:\||[-—])\s*(?:chatgpt|openai))?$/.test(normalizedTitle);
  if (challengeRoute
      || challengeTitle
      || /captcha|cloudflare|checking your browser|attention required|security check|just a moment|verify you are human|cf-chl/.test(normalizedText)) {
    return {
      state: "challenge_required",
      message: "ChatGPT requires manual challenge completion"
    };
  }
  const loginRoute = /^\/(?:auth\/(?:login|oauth)|login|oauth)(?:\/|$)/.test(pathname);
  const loginTitle = /^(?:log in|login|sign in)(?:\s*(?:\||[-—])\s*(?:chatgpt|openai))?$/.test(normalizedTitle);
  if (loginRoute
      || loginTitle
      || /\blog in\b|\blogin\b|\bsign in\b|\bsign up\b|continue with google/.test(normalizedText)) {
    return {
      state: "login_required",
      message: "ChatGPT login required in this Chrome profile"
    };
  }
  const rateLimitTitle = /^(?:rate limited|too many requests|try again later)(?:\s*(?:\||[-—])\s*(?:chatgpt|openai))?$/.test(normalizedTitle);
  if (rateLimitTitle || /\brate limits?\b|\btoo many requests\b|\btry again later\b/.test(normalizedText)) {
    return {
      state: "rate_limited",
      message: "ChatGPT is rate limited"
    };
  }
  return null;
}

export function classifyWaitManualHandoff({ url = "", title = "", text = "" } = {}) {
  return classifyManualHandoff({ url, title, text });
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

export function findAuthenticatedComposer(root = document) {
  return firstVisible(root, [
    "#prompt-textarea",
    'textarea[data-testid*="composer"]',
    'div[contenteditable="true"][data-testid*="composer"]'
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

export function findModelButton(root = document) {
  const scopes = modelControlScopes(root);
  for (const scope of scopes) {
    const buttons = Array.from(scope.querySelectorAll('button[aria-haspopup="menu"]'))
      .filter((node) => isVisible(node, { allowDisabled: true }) && !isTranscriptModelControl(node));
    const composerPills = buttons.filter((node) => classTokens(node).includes("__composer-pill"));
    const grammarPill = composerPills.find((node) => modelPillSummaryMatches(modelControlLabel(node)))
      ?? buttons.find((node) => modelPillSummaryMatches(modelControlLabel(node)));
    if (grammarPill) {
      return grammarPill;
    }
    const familyTokenPill = composerPills.find((node) => pillHasModelFamilyToken(modelControlLabel(node)));
    if (familyTokenPill) {
      return familyTokenPill;
    }
    if (composerPills[0]) {
      return composerPills[0];
    }
  }
  return null;
}

export function getPageText(root = document) {
  return String(root.body?.innerText ?? root.documentElement?.innerText ?? "");
}

export function manualHandoffContext(root = document) {
  if (findAuthenticatedComposer(root)) {
    return {
      authenticated: true,
      title: "",
      text: ""
    };
  }

  const hasTranscript = hasConversationResidue(root);
  const surfaces = manualHandoffSurfaces(root, { hasTranscript });
  const chunks = [];
  for (const surface of surfaces) {
    collectManualHandoffSurfaceText(surface, chunks);
  }
  const hasShell = hasManualHandoffShell(root);
  if (surfaces.length > 0 || hasShell || hasTranscript) {
    return {
      authenticated: false,
      title: "",
      text: normalizeText(chunks.join("\n"))
    };
  }
  return {
    authenticated: false,
    title: String(root.title ?? ""),
    text: getPageText(root)
  };
}

export function markOwnership(root, job) {
  const target = root.documentElement ?? root.body;
  if (!target) {
    return false;
  }
  target.setAttribute(OWNERSHIP_ATTR, job.job_id);
  target.setAttribute("data-yoetz-run-id", job.run_id);
  if (job.workspace_id) {
    target.setAttribute("data-yoetz-workspace-id", job.workspace_id);
  }
  if (job.ownership_nonce) {
    target.setAttribute("data-yoetz-ownership-nonce", job.ownership_nonce);
  }
  return true;
}

export function assertOwnedPage(win, job) {
  const parsed = parseOwnedWindowName(win.name);
  return ownershipMatchesJob(parsed, job);
}

function ownershipMatchesJob(parsed, job) {
  return parsed?.job_id === job?.job_id
    && parsed?.run_id === job?.run_id
    && (job?.workspace_id == null || parsed?.workspace_id === job.workspace_id)
    && (job?.ownership_nonce == null || parsed?.ownership_nonce === job.ownership_nonce);
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
  return waitForUploadComplete(root, file, { ...options, baselineAttachments });
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

// Hidden tabs hydrate late (tens of seconds), and interacting with the page
// before hydration completes can wedge it permanently: the composer pill
// opens an empty menu whose items and handlers never attach. In a genuinely
// hidden tab, hold off until the composer pill node has been stable for a
// few seconds. The MAIN-world visibility shim does not reach this isolated
// world, so document.hidden here reflects the tab's real visibility.
async function waitForHiddenTabHydration(root, options = {}) {
  const doc = typeof root?.hidden === "boolean" ? root : root?.ownerDocument;
  if (doc?.hidden !== true) return "visible";
  const totalMs = Number(options.hydrationTimeoutMs ?? 30000);
  const stabilityMs = Number(options.hydrationStabilityMs ?? 3000);
  const deadline = Date.now() + totalMs;
  const attr = (name) => doc.documentElement?.getAttribute?.(name) === "1";
  const shimPresent = attr("data-yoetz-shim");
  // With the MAIN-world shim present it stamps data-yoetz-hydrated once the
  // composer pill carries a React fiber; the server-rendered skeleton is
  // stable but handler-less, so node stability must not short-circuit the
  // flag wait. A short stability window is reserved at the END of the same
  // total budget as a secondary gate for a shim whose pill probe never
  // matched; the whole gate never exceeds hydrationTimeoutMs.
  const reservedMs = shimPresent ? Math.min(Math.max(stabilityMs * 2, stabilityMs + 1000), totalMs / 2) : 0;
  if (shimPresent) {
    const flagDeadline = deadline - reservedMs;
    while (Date.now() < flagDeadline) {
      if (attr("data-yoetz-hydrated")) return "flag";
      await sleep(500);
    }
  }
  let last = findModelButton(root);
  let stableSince = Date.now();
  while (Date.now() < deadline) {
    await sleep(500);
    if (shimPresent && attr("data-yoetz-hydrated")) return "flag";
    const current = findModelButton(root);
    if (!current || current !== last) {
      last = current;
      stableSince = Date.now();
      continue;
    }
    if (Date.now() - stableSince >= stabilityMs) {
      return shimPresent ? "flag_timeout_node_stability" : "node_stability";
    }
  }
  return "timeout";
}

export async function configureModelState(root, job = {}) {
  const hydrationSignal = await waitForHiddenTabHydration(root, modelSelectionOptionsForJob(job));
  const surface = await ensureChatSurface(root, modelSelectionOptionsForJob(job));
  if (!surface.ok) {
    return {
      hydration_signal: hydrationSignal,
      status: "unavailable",
      model_used: null,
      requested_model: modelSelectionStrategyForJob(job) === "current"
        ? "current"
        : CHATGPT_SOL_CHAT_PRO_MODEL,
      available_options: [],
      available_families: [],
      family_status: "unverified",
      effort_status: "unverified",
      picker_family_status: "unverified",
      picker_effort_status: "unverified",
      closed_pill_family_status: "skipped",
      closed_pill_effort_status: "skipped",
      closed_pill_text: null,
      failure_reason: surface.failure_reason,
      warning: surface.warning,
      warnings: [],
      ...surfaceResultFields(surface)
    };
  }

  if (modelSelectionStrategyForJob(job) === "current") {
    const modelButton = await waitForModelButton(root, { timeoutMs: 1500, intervalMs: 250 });
    const pillText = modelControlLabel(modelButton);
    if (!modelButton || !pillText) {
      return {
        hydration_signal: hydrationSignal,
        status: "unavailable",
        model_used: null,
        requested_model: "current",
        family_status: "skipped",
        effort_status: "skipped",
        failure_reason: "current_model_pill_not_found",
        warning: "ChatGPT current model pill was not visible",
        ...surfaceResultFields(surface)
      };
    }
    return {
      hydration_signal: hydrationSignal,
      status: "current",
      model_used: pillText ?? "",
      requested_model: "current",
      available_options: [],
      available_families: [],
      family_status: "skipped",
      effort_status: "skipped",
      picker_family_status: "skipped",
      picker_effort_status: "skipped",
      closed_pill_family_status: "skipped",
      closed_pill_effort_status: "skipped",
      closed_pill_text: pillText ?? "",
      failure_reason: null,
      picker_shape: null,
      surface_trust: null,
      surface_descendants: [],
      effort_ceiling_label: null,
      advanced_rows: [],
      checkbox_probe: null,
      family_menu_probe: null,
      effort_control: null,
      effort_move_method: null,
      pill_text: pillText ?? "",
      family_label: null,
      family_label_candidates: [],
      family_label_source: null,
      picker_close_method: null,
      picker_close_verification: null,
      effort_options: [],
      warning: "model pinning bypassed — answer may come from any model",
      warnings: [],
      ...surfaceResultFields(surface)
    };
  }

  const selection = await selectSolChatProModel(root, modelSelectionOptionsForJob(job));
  const warnings = selection.warning ? [selection.warning] : [];
  return {
    hydration_signal: hydrationSignal,
    status: selection.status,
    model_used: selection.model_used,
    requested_model: CHATGPT_SOL_CHAT_PRO_MODEL,
    available_options: selection.available_options ?? [],
    available_families: selection.available_families ?? [],
    family_status: selection.family_status ?? "unverified",
    effort_status: selection.effort_status ?? "unverified",
    picker_family_status: selection.picker_family_status ?? "unverified",
    picker_effort_status: selection.picker_effort_status ?? "unverified",
    closed_pill_family_status: selection.closed_pill_family_status ?? "skipped",
    closed_pill_effort_status: selection.closed_pill_effort_status ?? "skipped",
    closed_pill_text: selection.closed_pill_text ?? selection.pill_text ?? null,
    post_close_family_status: selection.post_close_family_status ?? "skipped",
    post_close_effort_status: selection.post_close_effort_status ?? "skipped",
    post_close_picker_shape: selection.post_close_picker_shape ?? null,
    post_close_picker_close_verification: selection.post_close_picker_close_verification ?? null,
    post_close_closed_pill_family_status: selection.post_close_closed_pill_family_status ?? null,
    post_close_closed_pill_effort_status: selection.post_close_closed_pill_effort_status ?? null,
    post_close_closed_pill_text: selection.post_close_closed_pill_text ?? null,
    post_close_failure_reason: selection.post_close_failure_reason ?? null,
    post_close_disabled_reason: selection.post_close_disabled_reason ?? null,
    failure_reason: selection.failure_reason ?? null,
    picker_shape: selection.picker_shape ?? null,
    surface_trust: selection.surface_trust ?? null,
    surface_descendants: selection.surface_descendants ?? [],
    effort_ceiling_label: selection.effort_ceiling_label ?? null,
    advanced_rows: selection.advanced_rows ?? [],
    checkbox_probe: selection.checkbox_probe ?? null,
    family_menu_probe: selection.family_menu_probe ?? null,
    effort_control: selection.effort_control ?? null,
    effort_move_method: selection.effort_move_method ?? null,
    pill_text: selection.pill_text ?? null,
    family_label: selection.family_label ?? null,
    family_label_candidates: selection.family_label_candidates ?? [],
    family_label_source: selection.family_label_source ?? null,
    picker_close_method: selection.picker_close_method ?? null,
    picker_close_verification: selection.picker_close_verification ?? null,
    effort_options: selection.effort_options ?? [],
    warning: warnings[0] ?? null,
    warnings,
    ...surfaceResultFields(surface)
  };
}

export async function ensureChatSurface(root, options = {}) {
  const timing = surfaceTiming(options);
  const startedAt = Date.now();
  let attempts = 0;
  let observedValues = [];
  let state = surfaceSelectionState(null);
  let clicked = false;
  let stableProof = null;
  let stableObservations = 0;
  let surfaceEvidenceSeen = options.surfaceEvidenceSeen === true;
  while (Date.now() - startedAt < timing.timeoutMs) {
    attempts += 1;
    const controls = findChatSurfaceControls(root);
    observedValues = observedSurfaceToggleValues(root);
    const visibleSurfaceToggleCount = visibleSurfaceToggleNodes(root).length;
    surfaceEvidenceSeen = surfaceEvidenceSeen || surfaceEvidencePresent(root);
    state = surfaceSelectionState(controls?.chat);
    const implicitProofReady = Date.now() - startedAt >= MIN_IMPLICIT_SURFACE_ABSENCE_MS;
    const proof = controls && surfaceSelectionIsChat(controls)
      && visibleSurfaceToggleCount === 2
      ? "controls"
      : !controls && !surfaceEvidenceSeen && implicitProofReady && implicitChatSurfaceProof(root, observedValues)
        ? "composer_aria"
        : null;
    if (proof === stableProof) {
      stableObservations += 1;
    } else {
      stableProof = proof;
      stableObservations = proof ? 1 : 0;
    }
    if (proof && stableObservations >= 2) {
      return {
        ok: true,
        elapsed_ms: Math.max(0, Date.now() - startedAt),
        attempts,
        timeout_ms: timing.timeoutMs,
        interval_ms: timing.intervalMs,
        verification_attempts: Math.max(0, attempts - 1),
        state,
        observed_values: observedValues,
        surface_evidence_seen: surfaceEvidenceSeen
      };
    }
    if (options.selectChat === false && surfaceEvidenceSeen && !proof) {
      return {
        ok: false,
        failure_reason: "chat_surface_selection_mismatch",
        warning: "ChatGPT Chat surface was not selected before send",
        elapsed_ms: Math.max(0, Date.now() - startedAt),
        attempts,
        timeout_ms: timing.timeoutMs,
        interval_ms: timing.intervalMs,
        verification_attempts: Math.max(0, attempts - 1),
        state,
        observed_values: observedValues,
        surface_evidence_seen: surfaceEvidenceSeen
      };
    }
    if (controls && !surfaceSelectionIsChat(controls) && !clicked) {
      realClick(controls.chat);
      clicked = true;
      stableProof = null;
      stableObservations = 0;
    }
    const remainingMs = timing.timeoutMs - (Date.now() - startedAt);
    if (remainingMs <= 0) break;
    await sleep(Math.min(timing.intervalMs, remainingMs));
  }
  return {
    ok: false,
    failure_reason: clicked ? "chat_surface_selection_mismatch" : "chat_surface_control_not_found",
    warning: clicked
      ? "ChatGPT Chat surface could not be verified after selection"
      : 'ChatGPT Chat surface toggle not found or could not be read',
    elapsed_ms: Math.max(0, Date.now() - startedAt),
    attempts,
    timeout_ms: timing.timeoutMs,
    interval_ms: timing.intervalMs,
    verification_attempts: Math.max(0, attempts - 1),
    state,
    observed_values: observedValues,
    surface_evidence_seen: surfaceEvidenceSeen
  };
}

export async function resetModelSelectionState(root) {
  const leftovers = openComposerPickerLeftovers(root);
  if (!findPickerState(root) && leftovers.length === 0) {
    return { reset: true, picker_was_open: false };
  }
  for (const leftover of leftovers) {
    const closed = await closeModelPickerResult(root, leftover.trigger, null, {
      owningTrigger: leftover.trigger
    });
    if (!closed.ok && leftoverSurfaceIsOpen(root, leftover.trigger)) {
      throw chatgptCommandError(
        "model_picker_close_failed",
        "ChatGPT model picker remained open while resetting a restored model selection",
        { phase: "model_selection", side_effect_started: false }
      );
    }
  }
  if (findPickerState(root) || openComposerPickerLeftovers(root).length > 0) {
    const modelButton = findModelButton(root) ?? root.activeElement ?? root.body;
    if (!await closeModelPicker(root, modelButton)) {
      throw chatgptCommandError(
        "model_picker_close_failed",
        "ChatGPT model picker remained open while resetting a restored model selection",
        { phase: "model_selection", side_effect_started: false }
      );
    }
  }
  if (openComposerPickerLeftovers(root).length > 0) {
    throw chatgptCommandError(
      "model_picker_close_failed",
      "ChatGPT model picker remained open while resetting a restored model selection",
      { phase: "model_selection", side_effect_started: false }
    );
  }
  return { reset: true, picker_was_open: true };
}

function modelSelectionOptionsForJob(job = {}) {
  const options = {};
  if (job?.surface_evidence_seen === true) {
    options.surfaceEvidenceSeen = true;
  }
  const timeoutMs = Number(job?.model_selection_timeout_ms);
  if (Number.isFinite(timeoutMs) && timeoutMs > 0) {
    options.timeoutMs = timeoutMs;
  }
  const intervalMs = Number(job?.model_selection_interval_ms);
  if (Number.isFinite(intervalMs) && intervalMs > 0) {
    options.intervalMs = intervalMs;
  }
  const hydrationTimeoutMs = Number(job?.hydration_timeout_ms);
  if (Number.isFinite(hydrationTimeoutMs) && hydrationTimeoutMs > 0) {
    options.hydrationTimeoutMs = hydrationTimeoutMs;
  }
  const hydrationStabilityMs = Number(job?.hydration_stability_ms);
  if (Number.isFinite(hydrationStabilityMs) && hydrationStabilityMs > 0) {
    options.hydrationStabilityMs = hydrationStabilityMs;
  }
  return options;
}

function surfaceTiming(options = {}) {
  return {
    timeoutMs: positiveMs(options.timeoutMs, 30000),
    intervalMs: positiveMs(options.intervalMs, 250)
  };
}

function positiveMs(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : fallback;
}

function findChatSurfaceControls(root) {
  const candidates = [];
  for (const group of visibleSurfaceGroups(root)) {
    const chats = Array.from(group.querySelectorAll(CHAT_SURFACE_CHAT_SELECTOR))
      .filter((node) => isVisibleSurfaceRadio(node));
    const works = Array.from(group.querySelectorAll(CHAT_SURFACE_WORK_SELECTOR))
      .filter((node) => isVisibleSurfaceRadio(node));
    if (chats.length !== 1 || works.length !== 1) {
      return null;
    }
    candidates.push({ group, chat: chats[0], work: works[0] });
  }
  return candidates.length === 1 ? candidates[0] : null;
}

function visibleSurfaceGroups(root) {
  return Array.from(root?.querySelectorAll?.(CHAT_SURFACE_GROUP_SELECTOR) ?? [])
    .filter((group) => isVisible(group, { allowDisabled: true, allowNoLayout: true }));
}

function surfaceEvidencePresent(root) {
  return (root?.querySelectorAll?.(CHAT_SURFACE_GROUP_SELECTOR)?.length ?? 0) > 0
    || (root?.querySelectorAll?.('[role="radio"][data-tpp-toggle-value]')?.length ?? 0) > 0;
}

function visibleSurfaceToggleNodes(root) {
  return Array.from(root?.querySelectorAll?.('[role="radio"][data-tpp-toggle-value]') ?? [])
    .filter((node) => isVisibleSurfaceRadio(node));
}

function observedSurfaceToggleValues(root) {
  return visibleSurfaceToggleNodes(root)
    .map((node) => node?.getAttribute?.("data-tpp-toggle-value"))
    .filter(Boolean)
    .filter((value, index, values) => values.indexOf(value) === index)
    .slice(0, 10);
}

function implicitChatSurfaceProof(root, observedValues) {
  if (observedValues.length > 0
    || surfaceEvidencePresent(root)) {
    return false;
  }
  const composer = findComposer(root);
  return composer?.getAttribute?.("aria-label") === "Chat with ChatGPT";
}

function hasPositiveLayout(element) {
  const rect = element?.getBoundingClientRect?.();
  return Number(rect?.width) > 0 && Number(rect?.height) > 0;
}

function isVisibleSurfaceRadio(element) {
  return isVisible(element, { allowDisabled: true, allowNoLayout: true })
    && hasPositiveLayout(element);
}

function surfaceSelectionState(node) {
  return {
    aria_checked: node?.getAttribute?.("aria-checked") ?? null,
    data_state: node?.getAttribute?.("data-state") ?? null
  };
}

function surfaceSelectionIsChat(controls) {
  const chat = surfaceSelectionState(controls?.chat);
  const work = surfaceSelectionState(controls?.work);
  return chat.aria_checked === "true" && work.aria_checked === "false";
}

function surfaceProofFields(controls, visibleSurfaceToggleCount, composer, proofKind) {
  return {
    surface_proof_kind: proofKind,
    surface_chat_state: controls ? surfaceSelectionState(controls.chat) : null,
    surface_work_state: controls ? surfaceSelectionState(controls.work) : null,
    surface_visible_toggle_count: visibleSurfaceToggleCount,
    surface_composer_aria: proofKind === "implicit_chat_composer_aria"
      ? composer?.getAttribute?.("aria-label") ?? null
      : null
  };
}

export function verifyChatSurface(root = document, options = {}) {
  const controls = findChatSurfaceControls(root);
  const observedValues = observedSurfaceToggleValues(root);
  const visibleSurfaceToggleCount = visibleSurfaceToggleNodes(root).length;
  const surfaceEvidenceSeen = options.surfaceEvidenceSeen === true
    || surfaceEvidencePresent(root);
  const controlsReady = Boolean(
    controls
    && visibleSurfaceToggleCount === 2
    && surfaceSelectionIsChat(controls)
  );
  const composer = findComposer(root);
  const implicitReady = !controls
    && !surfaceEvidenceSeen
    && visibleSurfaceGroups(root).length === 0
    && observedValues.length === 0
    && composer?.getAttribute?.("aria-label") === "Chat with ChatGPT";
  const proofKind = controlsReady
    ? "explicit_chat_work_radios"
    : implicitReady
      ? "implicit_chat_composer_aria"
      : null;
  const proofFields = surfaceProofFields(controls, visibleSurfaceToggleCount, composer, proofKind);
  if (controlsReady || implicitReady) {
    return {
      ok: true,
      state: surfaceSelectionState(controls?.chat),
      observed_values: observedValues,
      surface_evidence_seen: surfaceEvidenceSeen,
      ...proofFields
    };
  }
  return {
    ok: false,
    failure_reason: controls ? "chat_surface_selection_mismatch" : "chat_surface_control_not_found",
    state: surfaceSelectionState(controls?.chat),
    observed_values: observedValues,
    surface_evidence_seen: surfaceEvidenceSeen,
    ...proofFields
  };
}

export function verifyChatgptModelSelectionBeforeSend(root = document, selection = {}) {
  const surface = verifyChatSurface(root, {
    surfaceEvidenceSeen: selection.surface_evidence_seen === true
  });
  const modelButton = findModelButton(root);
  const pillText = modelControlLabel(modelButton);
  const pickerOpen = Boolean(findPickerState(root))
    || openComposerPickerLeftovers(root).some((leftover) => leftoverSurfaceIsOpen(root, leftover.trigger));
  const currentFamilyStatus = pillHasModelFamilyToken(pillText)
    ? pillConfirmsFamilyLabel(pillText, selection.family_label)
      ? "verified"
      : "unverified"
    : "skipped";
  const currentEffortStatus = pillConfirmsEffortLabel(pillText, "Pro") ? "verified" : "unverified";
  const supportedShape = isSupportedPickerShape({ shape: selection.picker_shape });
  const familyCorroborated = selection.closed_pill_family_status === "verified"
    || selection.post_close_family_status === "verified";
  const closeVerification = selection.picker_close_verification;
  const selectedProofFieldsReady = selection.status === "selected"
    && selection.requested_model === CHATGPT_SOL_CHAT_PRO_MODEL
    && selection.model_used === "GPT-5.6 Sol Pro"
    && selection.family_status === "verified"
    && selection.effort_status === "verified"
    && selection.picker_family_status === "verified"
    && selection.picker_effort_status === "verified"
    && supportedShape
    && familyCorroborated
    && selection.closed_pill_effort_status === "verified"
    && selection.post_close_family_status !== "unverified"
    && selection.post_close_effort_status !== "unverified"
    && closeVerification?.picker_surface_closed === true
    && closeVerification?.model_trigger_closed === true
    && closeVerification?.family_trigger_closed === true
    && closeVerification?.closed_pill_pro === true;
  const currentProofFieldsReady = selection.status === "current"
    && selection.requested_model === "current"
    && selection.family_status === "skipped"
    && selection.effort_status === "skipped"
    && Boolean(pillText);
  const currentStrategy = currentProofFieldsReady;
  const proofFieldsReady = selectedProofFieldsReady || currentProofFieldsReady;
  const ok = surface.ok
    && proofFieldsReady
    && !pickerOpen
    && (currentStrategy || currentFamilyStatus !== "unverified")
    && (currentStrategy || currentEffortStatus === "verified");
  return {
    ok,
    failure_reason: ok
      ? null
      : !surface.ok
        ? "chat_surface_selection_mismatch"
        : pickerOpen
          ? "model_picker_open"
          : currentEffortStatus !== "verified"
            ? "effort_composer_pill_unverified"
            : !currentStrategy && currentFamilyStatus === "unverified"
              ? "family_composer_pill_unverified"
              : "model_selection_proof_incomplete",
    surface_evidence_seen: selection.surface_evidence_seen === true || surface.surface_evidence_seen === true,
    surface_state: surface.state ?? selection.surface_state ?? null,
    surface_observed_values: surface.observed_values ?? selection.surface_observed_values ?? [],
    surface_proof_kind: surface.surface_proof_kind ?? selection.surface_proof_kind ?? null,
    surface_chat_state: surface.surface_chat_state ?? selection.surface_chat_state ?? null,
    surface_work_state: surface.surface_work_state ?? selection.surface_work_state ?? null,
    surface_visible_toggle_count: surface.surface_visible_toggle_count ?? selection.surface_visible_toggle_count ?? 0,
    surface_composer_aria: surface.surface_composer_aria ?? selection.surface_composer_aria ?? null,
    picker_shape: selection.picker_shape ?? null,
    current_closed_pill_text: pillText || null,
    current_closed_pill_family_status: currentFamilyStatus,
    current_closed_pill_effort_status: currentEffortStatus,
    picker_open: pickerOpen
  };
}

function surfaceResultFields(surface) {
  return {
    surface_elapsed_ms: surface?.elapsed_ms ?? null,
    surface_attempts: surface?.attempts ?? 0,
    surface_verification_attempts: surface?.verification_attempts ?? 0,
    surface_state: surface?.state ?? surfaceSelectionState(null),
    surface_observed_values: surface?.observed_values ?? [],
    surface_evidence_seen: surface?.surface_evidence_seen === true,
    surface_proof_kind: surface?.surface_proof_kind ?? null,
    surface_chat_state: surface?.surface_chat_state ?? null,
    surface_work_state: surface?.surface_work_state ?? null,
    surface_visible_toggle_count: surface?.surface_visible_toggle_count ?? 0,
    surface_composer_aria: surface?.surface_composer_aria ?? null
  };
}

function modelSelectionStrategyForJob(job = {}) {
  return String(job?.model_strategy ?? "select").trim().toLowerCase() === "current"
    ? "current"
    : "select";
}

function modelControlScopes(root) {
  const composer = findComposer(root);
  const scopes = [];
  const add = (scope) => {
    if (scope && !scopes.includes(scope)) {
      scopes.push(scope);
    }
  };
  add(composer?.closest("form"));
  add(composer?.closest('[data-testid*="composer"], [class*="composer"]'));
  const parent = composer?.parentElement;
  if (isLocalComposerScope(parent)) {
    add(parent);
  }
  for (const scope of [...scopes]) {
    addAdjacentComposerControlScopes(scope, add);
  }
  return scopes;
}

function addAdjacentComposerControlScopes(scope, add) {
  const parent = scope?.parentElement;
  if (!parent) {
    return;
  }
  for (const sibling of Array.from(parent.children ?? [])) {
    if (sibling !== scope && looksLikeComposerControlScope(sibling)) {
      add(sibling);
    }
  }
}

function isLocalComposerScope(node) {
  if (!node) {
    return false;
  }
  const tag = String(node.tagName ?? "").toLowerCase();
  const role = String(node.getAttribute?.("role") ?? "").toLowerCase();
  return !["html", "body", "main"].includes(tag) && role !== "main";
}

function looksLikeComposerControlScope(node) {
  const marker = normalizeText([
    node?.getAttribute?.("data-testid"),
    node?.getAttribute?.("class"),
    node?.getAttribute?.("aria-label"),
    node?.getAttribute?.("role")
  ].filter(Boolean).join(" ")).toLowerCase();
  return /\b(composer|model|switcher|toolbar|controls|pill)\b/.test(marker)
    && !/\b(conversation|transcript|message|turn|assistant|user)\b/.test(marker);
}

function composerMenuTriggers(root) {
  const triggers = [];
  for (const scope of modelControlScopes(root)) {
    for (const node of Array.from(scope.querySelectorAll?.('button[aria-haspopup="menu"]') ?? [])) {
      if (!isTranscriptModelControl(node)) {
        triggers.push(node);
      }
    }
  }
  return uniqueElements(triggers);
}

function openComposerPickerLeftovers(root) {
  const leftovers = [];
  for (const trigger of composerMenuTriggers(root)) {
    if (!modelPickerTriggerIsOpen(trigger)) continue;
    const surface = structurallyOpenControlledSurfaceForTrigger(root, trigger);
    leftovers.push({ trigger, surface });
  }
  return leftovers;
}

function leftoverSurfaceIsOpen(root, trigger) {
  if (!isMountedInRoot(root, trigger)) return false;
  if (modelPickerTriggerIsOpen(trigger)) return true;
  const surface = structurallyOpenControlledSurfaceForTrigger(root, trigger);
  return Boolean(surface);
}

function modelControlLabel(node) {
  return normalizeText([
    textOf(node),
    node?.getAttribute?.("aria-label"),
    node?.getAttribute?.("title")
  ].filter(Boolean).join(" "));
}

async function selectSolChatProModel(root, options = {}) {
  const base = {
    status: "unavailable",
    model_used: null,
    failure_reason: null,
    family_status: "unverified",
    effort_status: "unverified",
    picker_family_status: "unverified",
    picker_effort_status: "unverified",
    closed_pill_family_status: "skipped",
    closed_pill_effort_status: "skipped",
    closed_pill_text: null,
    post_close_family_status: "skipped",
    post_close_effort_status: "skipped",
    post_close_picker_shape: null,
    post_close_picker_close_verification: null,
    picker_shape: null,
    effort_control: null,
    effort_move_method: null,
    available_options: [],
    available_families: [],
    effort_options: []
  };
  const legacyMarkers = visibleLegacyPickerMarkers(root);
  if (legacyMarkers.length > 0) {
    return {
      ...base,
      failure_reason: "legacy_picker_detected",
      warning: "legacy ChatGPT picker detected; this yoetz version requires the GPT-5.6 UI",
      legacy_picker: legacyMarkers.slice(0, 10)
    };
  }

  let modelButton = await waitForModelButton(root, options);
  if (!modelButton) {
    const lateLegacyMarkers = visibleLegacyPickerMarkers(root);
    if (lateLegacyMarkers.length > 0) {
      return {
        ...base,
        failure_reason: "legacy_picker_detected",
        warning: "legacy ChatGPT picker detected; this yoetz version requires the GPT-5.6 UI",
        legacy_picker: lateLegacyMarkers.slice(0, 10)
      };
    }
    return {
      ...base,
      failure_reason: "model_control_not_found",
      warning: "ChatGPT GPT-5.6 composer model pill not found"
    };
  }

  let availableFamilies = [];
  let state = await openAndReadModelPicker(root, modelButton, options);
  if (!state) {
    return {
      ...base,
      failure_reason: "model_picker_open_failed",
      pill_text: modelControlLabel(modelButton),
      warning: "ChatGPT GPT-5.6 model picker did not open"
    };
  }
  if (!isSupportedPickerShape(state)) {
    await closeModelPicker(root, modelButton);
    return selectionFailure(
      base,
      modelButton,
      state,
      availableFamilies,
      "ChatGPT model picker exposed an unsupported shape; refusing unverified model selection",
      "model_picker_shape_unsupported"
    );
  }
  let familyProof = await readCheckedSolFamily(root, state, options);
  availableFamilies = familyProof.available_families;
  if (!familyProof.ok) {
    if (familyProof.checked_items.length !== 1 || !familyProof.sol_option) {
      await closeModelPicker(root, modelButton);
      return selectionFailure(
        base,
        modelButton,
        familyProof.state,
        availableFamilies,
        familyProof.checked_items.length === 1
          ? "GPT-5.6 Sol was not visible in the family submenu"
          : "GPT-5.6 Sol family menu did not expose one checked model",
        familyProof.checked_items.length === 1 ? "model_family_not_found" : "model_family_menu_unverified"
      );
    }
    realClick(familyProof.sol_option);
    await sleep(Number(options.actionSettleMs ?? 250));
    if (!await closeModelPicker(root, modelButton)) {
      return selectionFailure(base, modelButton, familyProof.state, availableFamilies, "ChatGPT model picker did not close after selecting GPT-5.6 Sol", "model_picker_close_failed");
    }
    modelButton = await waitForModelButton(root, options);
    if (!modelButton) {
      return selectionFailure(base, null, null, availableFamilies, "ChatGPT composer model pill did not remount after selecting GPT-5.6 Sol", "model_family_remount_failed");
    }
    state = await openAndReadModelPicker(root, modelButton, options);
    if (!state) {
      return selectionFailure(base, modelButton, null, availableFamilies, "ChatGPT picker did not reopen after selecting GPT-5.6 Sol", "model_picker_reopen_failed");
    }
    if (!isSupportedPickerShape(state)) {
      await closeModelPicker(root, modelButton);
      return selectionFailure(base, modelButton, state, availableFamilies, "ChatGPT model picker exposed an unsupported shape after selecting GPT-5.6 Sol; refusing unverified model selection", "model_picker_shape_unsupported");
    }
    familyProof = await readCheckedSolFamily(root, state, options);
    availableFamilies = familyProof.available_families;
    if (!familyProof.ok) {
      await closeModelPicker(root, modelButton);
      return selectionFailure(base, modelButton, familyProof.state, availableFamilies, "GPT-5.6 Sol family menu selection could not be verified", "model_family_selection_unverified");
    }
  }
  state = familyProof.state;

  if (!effortIsChatProTier(state)) {
    const disabledPro = disabledProEffortOption(state.surface ?? state.menu);
    if (disabledPro) {
      await closeModelPicker(root, modelButton);
      return selectionFailure(
        base,
        modelButton,
        state,
        availableFamilies,
        disabledPro.reason
          ? `ChatGPT Pro effort is disabled: ${disabledPro.reason}`
          : "ChatGPT Pro effort is disabled (account limit reached or rollout lock); refusing unverified selection",
        "effort_options_disabled"
      );
    }
    if (state.shape === "personal") {
      const selected = await selectPersonalChatProEffort(root, state, options);
      state = selected.state ?? state;
      state.effort_move_method = selected.method;
      if (!selected.ok) {
        await closeModelPicker(root, modelButton);
        return selectionFailure(base, modelButton, state, availableFamilies, "GPT-5.6 Sol Pro effort was not visible in the personal picker", "effort_control_not_found");
      }
    } else if (state.shape === "slider") {
      if (!state.effort_slider) {
        await closeModelPicker(root, modelButton);
        return selectionFailure(base, modelButton, state, availableFamilies, "GPT-5.6 Sol effort slider was not found in the Advanced picker", "effort_control_not_found");
      }
      const moved = await moveEffortSliderToProTier(root, state, options);
      state = moved.state ?? state;
      state.effort_move_method = moved.method;
      if (!moved.ok) {
        await closeModelPicker(root, modelButton);
        return selectionFailure(base, modelButton, state, availableFamilies, "GPT-5.6 Sol effort slider did not move to verified Pro", "effort_slider_move_failed");
      }
    } else {
      const proOption = state.effort_items.find((item) => foldedModelText(optionLabel(item)) === "pro");
      if (!proOption) {
        await closeModelPicker(root, modelButton);
        return selectionFailure(base, modelButton, state, availableFamilies, "GPT-5.6 Sol Pro effort was not visible in the effort menu", "effort_control_not_found");
      }
      realClick(proOption);
      await sleep(Number(options.actionSettleMs ?? 250));
      modelButton = await waitForModelButton(root, options);
      if (!modelButton) {
        return selectionFailure(base, null, null, availableFamilies, "ChatGPT composer model pill did not remount after selecting Pro effort", "effort_control_remount_failed");
      }
      state = await openAndReadModelPicker(root, modelButton, options);
      if (!state) {
        return selectionFailure(base, modelButton, null, availableFamilies, "ChatGPT picker did not reopen after selecting Pro effort", "model_picker_reopen_failed");
      }
      if (!isSupportedPickerShape(state)) {
        await closeModelPicker(root, modelButton);
        return selectionFailure(base, modelButton, state, availableFamilies, "ChatGPT model picker exposed an unsupported shape after selecting Pro effort; refusing unverified model selection", "model_picker_shape_unsupported");
      }
    }
  }

  familyProof = await readCheckedSolFamily(root, state, options);
  availableFamilies = familyProof.available_families.length > 0 ? familyProof.available_families : availableFamilies;
  if (!familyProof.ok) {
    await closeModelPicker(root, modelButton);
    return selectionFailure(base, modelButton, familyProof.state, availableFamilies, "GPT-5.6 Sol family menu could not be re-verified after effort selection", "model_family_selection_unverified");
  }
  state = familyProof.state;
  const familyVerified = familyIsSol(state.family_label);
  const effortVerified = effortIsChatProTier(state);
  if (!familyVerified || !effortVerified) {
    await closeModelPicker(root, modelButton);
    return selectionFailure(base, modelButton, state, availableFamilies, "GPT-5.6 Sol at verified Pro effort could not be confirmed in one picker pass", "model_selection_verification_failed");
  }
  const closeResult = await closeModelPickerResult(root, modelButton, state, { requireProPill: true });
  state.picker_close_method = closeResult.method;
  state.picker_close_verification = closeResult.verification;
  if (!closeResult.ok) {
    if (closeResult.verification?.picker_surface_closed
      && closeResult.verification?.model_trigger_closed
      && closeResult.verification?.family_trigger_closed
      && closeResult.verification?.closed_pill_pro === false) {
      return selectionFailure(base, modelButton, state, availableFamilies, "ChatGPT composer model pill did not confirm verified Pro effort", "effort_composer_pill_unverified", { closedPill: true });
    }
    return selectionFailure(base, modelButton, state, availableFamilies, "ChatGPT model picker remained open or closed composer model pill failed verification", "model_picker_close_failed");
  }
  modelButton = await waitForModelButton(root, options);
  const pillText = modelControlLabel(modelButton);
  const verifiedEffortLabel = pickerVerifiedEffortLabel(state);
  const closedPill = closedPillDiagnostics(pillText, state);
  if (closedPill.closed_pill_family_status === "unverified") {
    return selectionFailure(base, modelButton, state, availableFamilies, "ChatGPT composer model pill reported another model family after closing the picker", "family_composer_pill_unverified", { closedPill: true });
  }
  if (closedPill.closed_pill_effort_status !== "verified") {
    return selectionFailure(base, modelButton, state, availableFamilies, "ChatGPT composer model pill did not confirm verified Pro effort", "effort_composer_pill_unverified", { closedPill: true });
  }
  let postClose = {
    post_close_family_status: "skipped",
    post_close_effort_status: "skipped",
    post_close_picker_shape: null,
    post_close_picker_close_verification: null
  };
  if (closedPill.closed_pill_family_status === "skipped") {
    postClose = await reverifyModelSelectionAfterClose(root, modelButton, options);
    if (!postClose.ok) {
      const quotaLocked = postClose.post_close_failure_reason === "effort_options_disabled";
      return selectionFailure(
        base,
        modelButton,
        state,
        availableFamilies,
        quotaLocked
          ? (postClose.post_close_disabled_reason
            ? `ChatGPT Pro effort is disabled: ${postClose.post_close_disabled_reason}`
            : "ChatGPT Pro effort is disabled (account limit reached or rollout lock); refusing unverified selection")
          : "ChatGPT model family was not independently re-read after the closed composer pill omitted it",
        quotaLocked ? "effort_options_disabled" : "post_close_model_reverification_failed",
        { closedPill: true, postClose }
      );
    }
  }
  const pickerFamilyStatus = familyVerified ? "verified" : "unverified";
  const pickerEffortStatus = effortVerified ? "verified" : "unverified";
  const familyStatus = closedPill.closed_pill_family_status === "skipped"
    ? combinedVerificationStatus(pickerFamilyStatus, postClose.post_close_family_status)
    : combinedVerificationStatus(pickerFamilyStatus, closedPill.closed_pill_family_status);
  const warnings = [];

  return {
    status: "selected",
    model_used: `${normalizeText(state.family_label)} ${verifiedEffortLabel}`,
    failure_reason: null,
    family_status: familyStatus,
    effort_status: combinedVerificationStatus(pickerEffortStatus, closedPill.closed_pill_effort_status),
    picker_family_status: pickerFamilyStatus,
    picker_effort_status: pickerEffortStatus,
    ...closedPill,
    ...postClose,
    picker_shape: state.shape,
    surface_trust: state.surface_trust,
    effort_control: effortControlDiagnostics(state),
    effort_move_method: state.effort_move_method ?? null,
    pill_text: pillText,
    family_label: state.family_label,
    family_label_candidates: state.family_label_candidates ?? [],
    family_label_source: state.family_label_source ?? null,
    picker_close_method: closeResult.method,
    picker_close_verification: closeResult.verification,
    available_options: state.effort_items.map((item) => textOf(item)).filter(Boolean),
    available_families: availableFamilies,
    effort_options: effortDiagnostics(state.effort_items),
    warning: warnings[0] ?? null,
    warnings
  };
}

async function waitForModelButton(root, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? 30000);
  const intervalMs = Number(options.intervalMs ?? 250);
  const startedAt = Date.now();
  let modelButton = findModelButton(root);
  while (Date.now() - startedAt < timeoutMs) {
    modelButton = findModelButton(root);
    if (modelButton) {
      return modelButton;
    }
    await sleep(intervalMs);
  }
  return modelButton;
}

async function openAndReadModelPicker(root, modelButton, options = {}) {
  if (!await openModelPicker(root, modelButton, options)) {
    return null;
  }
  return waitForPickerState(root, options);
}

// A picker menu can be mounted and open in the DOM before it is classifiable
// (the advanced view is still inert, the pill has no aria-controls yet), and
// before the pill's own aria-expanded flips. Detecting that raw mounted-open
// state is what lets the activation sequence abort BEFORE its trailing click
// toggles the freshly opened menu back closed.
function pickerMenuMounted(root) {
  return Array.from(root.querySelectorAll?.('[role="menu"]') ?? []).some((menu) => {
    if (menu.getAttribute?.("data-state") === "closed") return false;
    if (!pickerSurfaceIsOpen(menu)) return false;
    // Structural readability up to the document root (not opacity) gates the
    // signal, so a stale menu hidden by itself or by any ancestor wrapper
    // can never count, while the mid-animation opacity-0 menu still does.
    if (!structurallyReadablePickerItem(menu, null)) return false;
    const hasToggle = Array.from(menu.querySelectorAll?.('[role="menuitem"]') ?? [])
      .some((item) => isSelectModelViewToggle(item) && structurallyReadablePickerItem(item, menu));
    // Exactly the hallmarks classification accepts: a bare family submenu
    // (radios only) is not a picker surface and must not be counted here.
    return hasToggle || hybridFamilyView(menu, true);
  });
}

async function openModelPicker(root, modelButton, options = {}) {
  const settleMs = Number(options.settleMs ?? 150);
  // Hidden-tab hydration can take ~20s before the pill accepts activation;
  // each attempt costs roughly five seconds, so eight keeps the budget
  // bounded near forty.
  const attempts = Number(options.openAttempts ?? 8);
  const opened = () => Boolean(findPickerState(root));
  // Abort the activation gesture as soon as a picker menu is mounted-open,
  // even if it is not yet classifiable — otherwise the gesture's trailing
  // click lands on the open trigger and toggles the menu closed.
  const openSignal = () => opened() || pickerMenuMounted(root);
  let button = modelButton;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    // A hydrating page can replace the composer pill after it was first
    // resolved; events dispatched at the detached node do nothing, so
    // re-resolve the live pill on every attempt.
    if (!isMountedInRoot(root, button)) {
      button = await waitForModelButton(root, options) ?? button;
    }
    if (opened()) return true;
    const currentButton = button;
    const openedOrTriggered = () => openSignal() || modelPickerTriggerIsOpen(currentButton);
    // Radix opens on pointerdown, which always goes out before the abort
    // check, so a pre-existing mounted menu can delay but never suppress
    // activation; it only stops the trailing phases.
    if (!modelPickerTriggerIsOpen(currentButton)) {
      for (const activate of [openWithPointerEvents, pressEnter, pressSpace]) {
        try {
          if (await activate(currentButton, openedOrTriggered, { settleMs })) break;
        } catch {
          recordModelPickerActivationException(root);
          // Try the next activation path; ChatGPT changes this control frequently.
        }
      }
    }
    // Throttled background tabs can commit the menu open (or mount a
    // classifiable surface) well after the dispatched events; wait bounded
    // before retrying with a freshly resolved pill.
    if (await waitForPickerState(root, options)) return true;
    // Only a wedged EMPTY menu (mounted but with no picker hallmarks) should
    // be closed for a fresh reopen; never Escape a menu that mounted open
    // with real content but merely failed to classify this tick.
    if (modelPickerTriggerIsOpen(currentButton) && !pickerMenuMounted(root)) {
      pressActivationKey(currentButton, "Escape");
      await sleep(settleMs);
    }
  }
  // Final settle: a menu can mount-open but classify only after the advanced
  // view sheds inert on a later frame.
  return Boolean(await waitForPickerState(root, options));
}
async function openFamilyPicker(root, mainMenu, trigger, options = {}) {
  if (!trigger) {
    return null;
  }
  const opened = () => findFamilySubmenu(root, mainMenu)
    ?? activeFamilyView(root, mainMenu, trigger)
    ?? (isSelectModelViewToggle(trigger) ? null : structurallyOpenControlledSurfaceForTrigger(root, trigger));
  const settleMs = Number(options.settleMs ?? 150);
  for (const activate of [openWithHoverEvents, openWithPointerEvents, pressEnter, pressSpace]) {
    try {
      if (await activate(trigger, opened, { settleMs })) {
        return waitForFamilyMenu(root, mainMenu, trigger, options);
      }
    } catch {
      // Try the next Radix activation path.
    }
  }
  return null;
}

async function readCheckedSolFamily(root, state, options = {}) {
  const surface = state?.surface ?? state?.menu;
  const inline = familyMenuRadios(surface, true);
  if (inline.length > 0) {
    const checkedItems = inline.filter((item) => itemIsChecked(item));
    const availableFamilies = inline.map((item) => textOf(item)).filter(Boolean);
    const checkedLabel = checkedItems.length === 1 ? normalizeText(textOf(checkedItems[0])) : "";
    return {
      ok: checkedItems.length === 1 && familyIsSol(checkedLabel),
      state: {
        ...state,
        family_label: checkedLabel,
        family_label_candidates: availableFamilies,
        family_label_source: checkedItems.length === 1 ? "inline_family_radio" : null,
        family_label_ambiguous: checkedItems.length > 1
      },
      sol_option: inline.find((item) => familyIsSol(textOf(item))) ?? null,
      checked_items: checkedItems,
      available_families: availableFamilies
    };
  }
  const familyMenu = await openFamilyPicker(root, state?.menu ?? state?.surface, state?.family_trigger, options);
  const controlledSurface = structurallyOpenControlledSurfaceForTrigger(root, state?.family_trigger);
  const activeView = activeFamilyView(root, state?.menu ?? state?.surface, state?.family_trigger);
  const familyMenuStructurallyTrusted = familyMenu === controlledSurface
    || (state?.surface_trust === "aria_controls_structural" && familyMenu === activeView)
    || (familyMenu === activeView && expandedSelectModelView(state?.family_trigger, familyMenu));
  const items = familyMenuRadios(familyMenu, familyMenuStructurallyTrusted);
  const checkedItems = items.filter((item) => item.getAttribute?.("aria-checked") === "true");
  const availableFamilies = items.map((item) => textOf(item)).filter(Boolean);
  const checkedLabel = checkedItems.length === 1 ? normalizeText(textOf(checkedItems[0])) : "";
  return {
    ok: checkedItems.length === 1 && familyIsSol(checkedLabel),
    state: {
      ...state,
      family_label: checkedLabel,
      family_label_candidates: availableFamilies,
      family_label_source: checkedItems.length === 1 ? "family_menu_checked" : null,
      family_label_ambiguous: checkedItems.length > 1,
      family_menu_probe: {
        trigger_found: Boolean(state?.family_trigger),
        trigger_is_select_model_toggle: isSelectModelViewToggle(state?.family_trigger),
        trigger_expanded: state?.family_trigger?.getAttribute?.("aria-expanded") ?? null,
        menu_found: Boolean(familyMenu),
        menu_structurally_trusted: familyMenuStructurallyTrusted,
        radio_count: items.length,
        checked_count: checkedItems.length
      }
    },
    sol_option: items.find((item) => familyIsSol(textOf(item))) ?? null,
    checked_items: checkedItems,
    available_families: availableFamilies
  };
}
async function openWithHoverEvents(element, isOpen, options = {}) {
  const settleMs = Number(options.settleMs ?? 150);
  const phases = [
    ["pointerenter", "PointerEvent", { pointerId: 1, pointerType: "mouse", isPrimary: true }],
    ["mouseenter", "MouseEvent", {}],
    ["pointermove", "PointerEvent", { pointerId: 1, pointerType: "mouse", isPrimary: true }],
    ["mousemove", "MouseEvent", {}]
  ];
  return dispatchActivationPhases(element, isOpen, phases, settleMs);
}

async function openWithPointerEvents(element, isOpen, options = {}) {
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
  return dispatchActivationPhases(element, isOpen, phases, settleMs);
}

async function dispatchActivationPhases(element, isOpen, phases, settleMs) {
  for (const [type, constructorName, init] of phases) {
    dispatchSyntheticEvent(element, type, constructorName, init);
    // Deliberately abort the synthetic sequence as soon as Radix reports open:
    // a trailing click on an already-open trigger can toggle it closed again.
    if (isOpen()) return true;
    await sleep(settleMs);
    if (isOpen()) return true;
  }
  return false;
}

async function pressEnter(element, isOpen, options = {}) {
  pressActivationKey(element, "Enter");
  await sleep(Number(options.settleMs ?? 150));
  return Boolean(isOpen());
}

async function pressSpace(element, isOpen, options = {}) {
  pressActivationKey(element, " ");
  await sleep(Number(options.settleMs ?? 150));
  return Boolean(isOpen());
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

async function closeModelPicker(root, modelButton) {
  return (await closeModelPickerResult(root, modelButton)).ok;
}

async function closeModelPickerResult(root, modelButton, state = null, options = {}) {
  const methods = [];
  let verification = pickerCloseVerification(root, modelButton, state, options);
  const tryMethod = async (method, action) => {
    methods.push(method);
    try {
      await action();
    } catch {
      // Continue to the next bounded close path and fail closed if needed.
    }
    verification = await waitForPickerClose(root, modelButton, state, options);
    return verification;
  };

  if (!verification.ok) {
    const currentButton = options.owningTrigger ?? findModelButton(root) ?? modelButton;
    verification = await tryMethod("escape", () => pressActivationKey(currentButton, "Escape"));
  }

  if (!verification.ok && !verification.picker_surface_closed) {
    const familyTrigger = familyTriggerForPicker(root, state);
    if (familyTrigger) {
      const neutral = neutralComposerArea(root, modelButton);
      verification = await tryMethod("hover_leave", () => dispatchHoverLeaveEvents(familyTrigger, neutral));
      if (!verification.ok && !verification.picker_surface_closed) {
        verification = await tryMethod("trigger_escape", () => pressActivationKey(familyTrigger, "Escape"));
      }
    }
  }

  if (!verification.ok && !verification.picker_surface_closed) {
    const neutral = neutralComposerArea(root, modelButton);
    if (neutral) {
      verification = await tryMethod("neutral_click", () => realClick(neutral));
    }
  }

  return {
    ok: verification.ok,
    method: methods.join("+") || "already_closed",
    verification
  };
}

async function reverifyModelSelectionAfterClose(root, modelButton, options = {}) {
  const reopenedButton = await waitForModelButton(root, options);
  if (!reopenedButton) {
    return {
      ok: false,
      post_close_family_status: "unverified",
      post_close_effort_status: "unverified",
      post_close_picker_shape: null,
      post_close_picker_close_verification: null,
      post_close_failure_reason: "model_control_not_found"
    };
  }
  const state = await openAndReadModelPicker(root, reopenedButton, options);
  if (!state || !isSupportedPickerShape(state)) {
    if (state) await closeModelPicker(root, reopenedButton);
    return {
      ok: false,
      post_close_family_status: "unverified",
      post_close_effort_status: "unverified",
      post_close_picker_shape: state?.shape ?? null,
      post_close_picker_close_verification: null,
      post_close_failure_reason: state ? "model_picker_shape_unsupported" : "model_picker_reopen_failed"
    };
  }
  const familyProof = await readCheckedSolFamily(root, state, options);
  const verifiedState = familyProof.state ?? state;
  const familyStatus = familyProof.ok ? "verified" : "unverified";
  const effortStatus = effortIsChatProTier(verifiedState) ? "verified" : "unverified";
  const disabledPro = effortStatus === "unverified"
    ? disabledProEffortOption(verifiedState.surface ?? verifiedState.menu)
    : null;
  const close = await closeModelPickerResult(root, reopenedButton, verifiedState, { requireProPill: true });
  const closedButton = await waitForModelButton(root, options);
  const closedPill = closedPillDiagnostics(modelControlLabel(closedButton), verifiedState);
  return {
    ok: familyStatus === "verified"
      && effortStatus === "verified"
      && close.ok
      && closedPill.closed_pill_effort_status === "verified"
      && closedPill.closed_pill_family_status !== "unverified",
    post_close_family_status: familyStatus,
    post_close_effort_status: effortStatus,
    post_close_picker_shape: verifiedState.shape,
    post_close_picker_close_verification: close.verification,
    post_close_closed_pill_family_status: closedPill.closed_pill_family_status,
    post_close_closed_pill_effort_status: closedPill.closed_pill_effort_status,
    post_close_closed_pill_text: closedPill.closed_pill_text,
    post_close_failure_reason: disabledPro
      ? "effort_options_disabled"
      : familyStatus !== "verified"
        ? "post_close_family_unverified"
        : effortStatus !== "verified"
          ? "post_close_effort_unverified"
          : !close.ok
            ? "post_close_picker_close_failed"
            : closedPill.closed_pill_effort_status !== "verified"
              ? "post_close_closed_pill_effort_unverified"
              : closedPill.closed_pill_family_status === "unverified"
                ? "post_close_closed_pill_family_unverified"
                : null,
    post_close_disabled_reason: disabledPro?.reason ?? null
  };
}

async function waitForPickerClose(root, modelButton, state, options) {
  let verification = pickerCloseVerification(root, modelButton, state, options);
  const settleMs = Number(options.closeSettleMs ?? 50);
  for (let attempt = 0; attempt < 3 && !verification.ok; attempt += 1) {
    await sleep(settleMs);
    verification = pickerCloseVerification(root, modelButton, state, options);
  }
  return verification;
}

function pickerCloseVerification(root, modelButton, state, options = {}) {
  const currentButton = options.owningTrigger
    ?? findModelButton(root)
    ?? (isMountedInRoot(root, modelButton) ? modelButton : null);
  const familyTrigger = familyTriggerForPicker(root, state);
  const familySurface = familySurfaceForPicker(root, state, familyTrigger);
  const leftovers = openComposerPickerLeftovers(root);
  const leftoverOpen = leftovers.some((leftover) => leftoverSurfaceIsOpen(root, leftover.trigger));
  // A retained closed menu keeps its "Select model" toggle mounted with a
  // stale aria-expanded="true"; the toggle only counts as open inside an
  // open surface.
  const familyTriggerOpen = isMountedInRoot(root, familyTrigger)
    && pickerSurfaceIsOpen(familyTrigger)
    && (familyTrigger.getAttribute?.("aria-expanded") === "true"
      || familyTrigger.getAttribute?.("data-state") === "open");
  const modelTriggerOpen = isMountedInRoot(root, currentButton) && modelPickerTriggerIsOpen(currentButton);
  const pickerSurfaceOpen = Boolean(findPickerState(root)) || Boolean(familySurface) || leftoverOpen;
  const pillText = modelControlLabel(currentButton);
  const closedPillPro = options.requireProPill !== true || pillConfirmsEffortLabel(pillText, "Pro");
  return {
    family_trigger_closed: !familyTriggerOpen,
    picker_surface_closed: !pickerSurfaceOpen,
    model_trigger_closed: !modelTriggerOpen,
    closed_pill_pro: closedPillPro,
    closed_pill_text: pillText || null,
    ok: !familyTriggerOpen && !pickerSurfaceOpen && !modelTriggerOpen && !leftoverOpen && closedPillPro
  };
}

function familyTriggerForPicker(root, state) {
  const liveState = findPickerState(root);
  const surface = liveState?.surface ?? liveState?.menu ?? state?.surface ?? state?.menu;
  const candidate = Array.from(surface?.querySelectorAll?.('[role="menuitem"], button') ?? [])
    .find((item) => item.getAttribute?.("aria-haspopup") === "menu"
      && (/\bModel\b/i.test(textOf(item)) || /^(?:gpt|o\d)\b/i.test(normalizeText(textOf(item)))));
  return candidate ?? liveState?.family_trigger ?? state?.family_trigger ?? null;
}

function familySurfaceForPicker(root, state, familyTrigger) {
  const mainSurface = state?.menu ?? state?.surface;
  const surface = findFamilySubmenu(root, mainSurface)
    ?? structurallyOpenControlledSurfaceForTrigger(root, familyTrigger);
  return isMountedInRoot(root, surface) ? surface : null;
}

function neutralComposerArea(root, modelButton) {
  const composer = findComposer(root);
  if (composer && composer !== modelButton) return composer;
  return root.querySelector?.('[data-testid="composer"], form') ?? null;
}

function dispatchHoverLeaveEvents(element, relatedTarget) {
  const rect = relatedTarget?.getBoundingClientRect?.() ?? { left: 0, top: 0, width: 1, height: 1 };
  const clientX = Number(rect.left ?? 0) + Math.max(1, Number(rect.width ?? 1) / 2);
  const clientY = Number(rect.top ?? 0) + Math.max(1, Number(rect.height ?? 1) / 2);
  for (const [type, constructorName, init] of [
    ["pointerleave", "PointerEvent", { pointerId: 1, pointerType: "mouse", isPrimary: true, relatedTarget, clientX, clientY }],
    ["mouseleave", "MouseEvent", { relatedTarget, clientX, clientY }],
    ["pointermove", "PointerEvent", { pointerId: 1, pointerType: "mouse", isPrimary: true, relatedTarget, clientX, clientY }],
    ["mousemove", "MouseEvent", { relatedTarget, clientX, clientY }]
  ]) {
    dispatchSyntheticEvent(element, type, constructorName, init);
  }
}

function isMountedInRoot(root, node) {
  if (!node) return false;
  if (node === root || node === root.body || node === root.documentElement) return true;
  for (let ancestor = node.parentElement; ancestor; ancestor = ancestor.parentElement) {
    if (ancestor === root || ancestor === root.body || ancestor === root.documentElement) return true;
  }
  return false;
}

async function waitForPickerState(root, options = {}) {
  const timeoutMs = Number(options.pickerTimeoutMs ?? 3000);
  const intervalMs = Number(options.intervalMs ?? 100);
  const startedAt = Date.now();
  let lastState = null;
  while (Date.now() - startedAt < timeoutMs) {
    const state = findPickerState(root);
    if (state) lastState = state;
    if (state && pickerStateIsReady(state)) return state;
    await sleep(intervalMs);
  }
  return lastState;
}
function structurallyOpenControlledSurface(root) {
  const trigger = findModelButton(root);
  return structurallyOpenControlledSurfaceForTrigger(root, trigger);
}
async function selectPersonalChatProEffort(root, initialState, options = {}) {
  const settleMs = Number(options.actionSettleMs ?? 250);
  if (!initialState?.effort_row) return { ok: false, state: initialState, method: null };
  realClick(initialState.effort_row);
  await sleep(settleMs);
  let state = findPickerState(root) ?? initialState;
  const effortMenu = personalEffortMenu(root, initialState);
  const submenuItems = Array.from(effortMenu?.querySelectorAll?.('[role="menuitemradio"], [role="menuitem"]') ?? [])
    .filter((item) => isVisible(item));
  const proOption = submenuItems.find((item) => foldedModelText(textOf(item)).replace(/\s+/g, " ") === "pro");
  if (!proOption) return { ok: false, state, method: null };
  realClick(proOption);
  await sleep(settleMs);
  state = findPickerState(root) ?? state;
  return { ok: effortIsChatProTier(state), state, method: "effort_row_select" };
}

function personalEffortMenu(root, state) {
  const personalSurface = state?.surface ?? null;
  const candidates = Array.from(root?.querySelectorAll?.('[role="menu"], [role="dialog"]') ?? [])
    .filter((surface) => surface !== personalSurface
      && pickerSurfaceIsOpen(surface)
      && isVisible(surface, { allowDisabled: true })
      && Array.from(surface.querySelectorAll?.('[role="menuitemradio"], [role="menuitem"]') ?? [])
        .some((item) => isVisible(item) && foldedModelText(textOf(item)).replace(/\s+/g, " ") === "pro"));
  return candidates.length === 1 ? candidates[0] : null;
}

async function waitForFamilyMenu(root, mainMenu, trigger, options = {}) {
  const timeoutMs = Number(options.pickerTimeoutMs ?? 3000);
  const intervalMs = Number(options.intervalMs ?? 100);
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const menu = findFamilySubmenu(root, mainMenu)
      ?? activeFamilyView(root, mainMenu, trigger)
      ?? (isSelectModelViewToggle(trigger) ? null : structurallyOpenControlledSurfaceForTrigger(root, trigger));
    if (menu) return menu;
    await sleep(intervalMs);
  }
  return null;
}
function pillConfirmsEffortLabel(pillText, effortLabel) {
  const foldedPill = foldedModelText(pillText).replace(/\s+/g, " ");
  const foldedEffort = foldedModelText(effortLabel).replace(/\s+/g, " ");
  return foldedEffort === "pro" && (foldedPill === "pro" || foldedPill.endsWith(" pro"));
}

function pillConfirmsFamilyLabel(pillText, familyLabel) {
  const foldedFamily = foldedFamilyLabel(familyLabel);
  if (!foldedFamily) return false;
  const lines = normalizeText(pillText).split("\n").map((line) => normalizeText(line)).filter(Boolean);
  const candidates = lines.length > 0 ? lines : [normalizeText(pillText)];
  return candidates.some((line) => {
    const foldedLine = foldedFamilyLabel(line);
    return foldedLine === foldedFamily || foldedLine.startsWith(`${foldedFamily} `);
  });
}

function pillHasModelFamilyToken(pillText) {
  const foldedPill = foldedModelText(pillText).replace(/\s+/g, " ");
  return /\bgpt[\s.-]*\d/.test(foldedPill)
    || /\bo\d(?:[\s.-]*\d)?\b/.test(foldedPill)
    || /\b\d+(?:\.\d+)+\b/.test(foldedPill);
}
function pickerVerifiedEffortLabel(state) {
  if (!state) return null;
  if (state.shape === "personal") return state.effort_label || null;
  if (state.shape === "slider") {
    const snapshot = sliderEffortSnapshot(state.effort_slider, state.surface);
    return snapshot?.display_label ?? null;
  }
  const checked = state.effort_items?.find((item) => itemIsChecked(item));
  return checked ? (optionLabel(checked) || null) : null;
}

function verificationStatus(ok) {
  return ok ? "verified" : "unverified";
}

function closedPillDiagnostics(pillText, state) {
  const text = pillText ?? "";
  const familyLabel = state?.family_label ?? null;
  const effortLabel = pickerVerifiedEffortLabel(state);
  const familyStatus = text && familyLabel
    ? pillConfirmsFamilyLabel(text, familyLabel)
      ? "verified"
      : pillHasModelFamilyToken(text) ? "unverified" : "skipped"
    : "skipped";
  return {
    closed_pill_text: text || null,
    closed_pill_family_status: familyStatus,
    closed_pill_effort_status: text && effortLabel
      ? verificationStatus(pillConfirmsEffortLabel(text, effortLabel))
      : "skipped"
  };
}

function combinedVerificationStatus(pickerStatus, closedStatus) {
  if (pickerStatus === "unverified" || closedStatus === "unverified") return "unverified";
  if (pickerStatus === "verified") return "verified";
  return pickerStatus ?? "unverified";
}
async function moveEffortSliderToProTier(root, initialState, options = {}) {
  const settleMs = Number(options.actionSettleMs ?? 250);
  let state = initialState;
  const originalSnapshot = sliderEffortSnapshot(initialState?.effort_slider, initialState?.surface);
  const attemptKey = async (key, method) => {
    if (state?.shape !== "slider" || !state.effort_slider) return null;
    pressActivationKey(state.effort_slider, key);
    await sleep(settleMs);
    // findPickerState can transiently return null or a non-slider state when the
    // picker re-renders during settle; keep the last known slider state so the
    // loop does not collapse on a stale snapshot. The final fresh re-check below
    // is the authoritative verification.
    state = findPickerState(root) ?? state;
    return effortIsChatProTier(state) ? { ok: true, state, method } : null;
  };

  let result = await attemptKey("End", "keyboard_end");
  if (result) return result;

  const snapshot = sliderEffortSnapshot(state?.effort_slider, state?.surface);
  const arrowAttempts = Math.min(10, Math.max(1, Math.ceil((snapshot?.max ?? 5) - (snapshot?.min ?? 1)) + 1));
  for (let attempt = 0; attempt < arrowAttempts; attempt += 1) {
    result = await attemptKey("ArrowRight", "keyboard_arrow_right");
    if (result) return result;
    if (state?.shape !== "slider" || !state.effort_slider) break;
  }

  if (state?.shape === "slider" && state.effort_slider) {
    clickSliderTrackMax(state.effort_slider);
    await sleep(settleMs);
    state = findPickerState(root) ?? state;
    if (effortIsChatProTier(state)) return { ok: true, state, method: "pointer_pro" };
  }
  const finalState = findPickerState(root);
  if (finalState && effortIsChatProTier(finalState)) {
    return { ok: true, state: finalState, method: "final_fresh_recheck" };
  }
  return { ok: false, state: finalState ?? state, method: null };
}

function checkboxProbeSnapshot(root, state, checkbox) {
  const advanced = Array.from(state?.surface?.querySelectorAll?.("*") ?? [])
    .find((node) => node.getAttribute?.("data-testid") === "composer-model-picker-slider-advanced-view");
  const speedRowNode = Array.from(advanced?.querySelectorAll?.("*") ?? [])
    .find((node) => node.getAttribute?.("role") === "menuitem" && /\bSpeed\b/i.test(textOf(node)));
  const speedOptions = Array.from(speedRowNode?.querySelectorAll?.("*") ?? [])
    .filter((node) => ["radio", "menuitemradio", "option"].includes(node.getAttribute?.("role")))
    .map((node) => normalizeText(textOf(node)))
    .filter(Boolean)
    .slice(0, 12);
  const effort = sliderEffortDiagnostics(state?.effort_slider, state?.surface);
  return {
    checked: checkbox?.getAttribute?.("aria-checked") ?? null,
    pill_text: modelControlLabel(findModelButton(root)),
    advanced_rows: advancedViewRows(state?.surface),
    speed_options: speedOptions,
    effort: effort ? {
      label: effort.label,
      value_now: effort.value_now,
      value_min: effort.value_min,
      value_max: effort.value_max,
      aria_disabled: state.effort_slider?.getAttribute?.("aria-disabled") ?? null,
      disabled: Boolean(state.effort_slider?.disabled),
      aria_hidden: state.effort_slider?.getAttribute?.("aria-hidden") ?? null,
      hidden: Boolean(state.effort_slider?.hidden)
    } : null
  };
}

function clickSliderTrackMax(slider) {
  const rect = slider?.getBoundingClientRect?.();
  if (!rect || !Number.isFinite(rect.left) || !Number.isFinite(rect.top)
    || !Number.isFinite(rect.width) || !Number.isFinite(rect.height)
    || rect.width <= 0 || rect.height <= 0) {
    return false;
  }
  const clientX = rect.left + rect.width - 1;
  const clientY = rect.top + (rect.height / 2);
  for (const [type, constructorName, init] of [
    ["pointerdown", "PointerEvent", { button: 0, buttons: 1, pointerId: 1, pointerType: "mouse", isPrimary: true }],
    ["mousedown", "MouseEvent", { button: 0, buttons: 1 }],
    ["pointerup", "PointerEvent", { button: 0, buttons: 0, pointerId: 1, pointerType: "mouse", isPrimary: true }],
    ["mouseup", "MouseEvent", { button: 0, buttons: 0 }],
    ["click", "MouseEvent", { button: 0, buttons: 0, detail: 1 }]
  ]) {
    dispatchSyntheticEvent(slider, type, constructorName, { ...init, clientX, clientY });
  }
  return true;
}
function selectionFailure(base, modelButton, state, availableFamilies, warning, failureReason, options = {}) {
  const pickerFamily = isSupportedPickerShape(state) && familyIsSol(state?.family_label) ? "verified" : "unverified";
  const pickerEffort = isSupportedPickerShape(state) && effortIsChatProTier(state) ? "verified" : "unverified";
  const pillText = modelControlLabel(modelButton);
  const closedPill = options.closedPill
    ? closedPillDiagnostics(pillText, state)
    : {
        closed_pill_text: null,
        closed_pill_family_status: "skipped",
        closed_pill_effort_status: "skipped"
      };
  return {
    ...base,
    failure_reason: failureReason,
    picker_family_status: pickerFamily,
    picker_effort_status: pickerEffort,
    family_status: combinedVerificationStatus(pickerFamily, closedPill.closed_pill_family_status),
    effort_status: combinedVerificationStatus(pickerEffort, closedPill.closed_pill_effort_status),
    ...closedPill,
    ...(options.postClose ?? {}),
    picker_shape: state?.shape ?? null,
    surface_trust: state?.surface_trust ?? null,
    surface_descendants: state?.surface_trust === "aria_controls_structural"
      ? structuralSurfaceDescendants(state.surface)
      : [],
    effort_ceiling_label: state?.effort_ceiling_label ?? null,
    advanced_rows: advancedViewRows(state?.surface),
    checkbox_probe: state?.checkbox_probe ?? null,
    family_menu_probe: state?.family_menu_probe ?? null,
    effort_control: effortControlDiagnostics(state),
    effort_move_method: state?.effort_move_method ?? null,
    picker_close_method: state?.picker_close_method ?? null,
    picker_close_verification: state?.picker_close_verification ?? null,
    pill_text: pillText,
    family_label: state?.family_label ?? null,
    family_label_candidates: state?.family_label_candidates ?? [],
    family_label_source: state?.family_label_source ?? null,
    available_options: state?.effort_items?.map((item) => textOf(item)).filter(Boolean) ?? [],
    available_families: availableFamilies,
    effort_options: effortDiagnostics(state?.effort_items ?? []),
    warning
  };
}
function structuralSurfaceDescendants(surface) {
  return Array.from(surface?.querySelectorAll?.("*") ?? []).slice(0, 40).map((node) => ({
    tag: node.tagName?.toLowerCase?.() ?? "element",
    role: node.getAttribute?.("role") ?? null,
    type: node.getAttribute?.("type") ?? null,
    tabindex: node.getAttribute?.("tabindex") ?? null,
    aria_haspopup: node.getAttribute?.("aria-haspopup") ?? null,
    aria_expanded: node.getAttribute?.("aria-expanded") ?? null,
    aria_controls: node.getAttribute?.("aria-controls") ?? null,
    aria_checked: node.getAttribute?.("aria-checked") ?? null,
    aria_valuetext: node.getAttribute?.("aria-valuetext") ?? null,
    aria_valuenow: node.getAttribute?.("aria-valuenow") ?? null,
    aria_valuemin: node.getAttribute?.("aria-valuemin") ?? null,
    aria_valuemax: node.getAttribute?.("aria-valuemax") ?? null,
    data_state: node.getAttribute?.("data-state") ?? null,
    data_testid: node.getAttribute?.("data-testid") ?? null,
    text: textOf(node).slice(0, 80)
  }));
}

function visibleLegacyPickerMarkers(root) {
  return Array.from(root.querySelectorAll([
    '[data-testid="model-switcher-dropdown-button"]',
    '[data-testid="model-switcher-selected-model"]',
    '[data-testid^="model-switcher-"]'
  ].join(",")))
    .filter((node) => isVisible(node, { allowDisabled: true }))
    .map((node) => String(node.getAttribute?.("data-testid") ?? ""))
    .filter(Boolean);
}

function classTokens(node) {
  return String(node?.getAttribute?.("class") ?? "").split(/\s+/).filter(Boolean);
}

function modelPillSummaryMatches(value) {
  const folded = foldedModelText(value).replace(/\s+/g, " ");
  const effort = "instant|medium|high|extra high|pro|max|light";
  return new RegExp(`^(?:${effort})$`).test(folded)
    || new RegExp(`^\\d+(?:\\.\\d+)+(?: sol)? (?:${effort})$`).test(folded)
    || /\bgpt[\s.-]*\d/.test(folded);
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
  const requiredStableTicks = Math.max(1, Number(options.requiredStableTicks ?? 2));
  const startedAt = Date.now();
  let lastCandidate = null;
  let enabledTicks = 0;

  while (Date.now() - startedAt < timeoutMs) {
    const button = findSendButtonControl(root, { requireEnabled: true });
    if (button) {
      enabledTicks += 1;
      if (enabledTicks >= requiredStableTicks) {
        assertExpectedConversationBeforeSendClick(root, options.expectedConversationId);
        await options.beforeClick?.();
        options.verifyBeforeClick?.();
        assertExpectedConversationBeforeSendClick(root, options.expectedConversationId);
        const verifiedButton = findSendButtonControl(root, { requireEnabled: true });
        if (!verifiedButton) {
          enabledTicks = 0;
          continue;
        }
        verifiedButton.click();
        return true;
      }
    } else {
      enabledTicks = 0;
    }
    lastCandidate = findSendButtonControl(root, { requireEnabled: false }) ?? lastCandidate;
    await sleep(intervalMs);
  }

  if (lastCandidate) {
    throw new Error(`ChatGPT send button remained disabled (${describeElement(lastCandidate)}; ${sendReadinessDiagnostics(root)})`);
  }
  throw new Error(`ChatGPT send button not found (${sendReadinessDiagnostics(root)})`);
}

function assertExpectedConversationBeforeSendClick(root, expectedConversationId) {
  const expected = String(expectedConversationId ?? "").trim();
  if (!expected) {
    return;
  }
  const win = root.defaultView ?? globalThis;
  const pathname = String(win.location?.pathname ?? "");
  const currentConversationId = conversationIdFromPathname(pathname);
  if (currentConversationId === expected) {
    return;
  }
  const code = currentConversationId ? "conversation_changed" : "conversation_not_loaded";
  throw chatgptCommandError(
    code,
    `ChatGPT conversation changed before send click; expected ${expected}, current ${currentConversationId ?? "(none)"}`,
    {
      phase: "send",
      side_effect_started: true,
      requested_conversation_id: expected,
      current_conversation_id: currentConversationId,
      current_url: currentLocationForError(win),
      current_pathname: pathname
    }
  );
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
      model_slug: messageModelSlug(latestAssistant ?? latestTextEntry?.node),
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
      model_slug: messageModelSlug(standalone.node),
      diagnostics
    };
  }

  const copyScopedStandalone = latestCopyScopedStandaloneMarkdown(root, userTurns, assistantTurns, copyButtons);
  if (copyScopedStandalone) {
    const assistantCount = Math.max(assistantTurns.length, 1);
    return {
      method: "copy_scope_dom_fallback",
      text: copyScopedStandalone.text,
      is_generating: isResponseGenerating(root),
      assistant_count: assistantCount,
      user_count: userTurns.length,
      preceding_user_count: precedingTurnCount(root, copyScopedStandalone.node, userTurns),
      copy_button_count: Math.max(copyButtonCount, 1),
      has_copy_button: true,
      turn_index: assistantCount - 1,
      model_slug: messageModelSlug(copyScopedStandalone.node),
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
    model_slug: messageModelSlug(latestAssistant),
    diagnostics
  };
}

function latestCopyScopedStandaloneMarkdown(root, userTurns, assistantTurns, copyButtons) {
  if (assistantTurns.length === 0) return null;
  const latestUser = userTurns.at(-1);
  if (!latestUser) return null;
  const conversation = conversationScope(latestUser);
  if (!conversation) return null;
  const ordered = flattenTree(root.documentElement ?? root.body ?? root);
  const userIndex = ordered.indexOf(latestUser);
  if (userIndex < 0) return null;
  for (const copy of [...copyButtons].reverse()) {
    if (!isCopyControl(copy) || !containsNode(conversation, copy) || isInsideUserTurn(copy)) continue;
    const copyIndex = ordered.indexOf(copy);
    if (copyIndex <= userIndex) continue;
    const markdown = leafNodes(Array.from(conversation.querySelectorAll?.('[class*="markdown"]') ?? []))
      .filter((node) => {
        const nodeIndex = ordered.indexOf(node);
        return nodeIndex > userIndex && nodeIndex < copyIndex
          && isVisible(node, { allowDisabled: true, allowNoLayout: true })
          && !isInsideUserTurn(node) && !isNonConversationChrome(node)
          && !isCitationSourceAffordance(node);
      })
      .at(-1);
    if (!markdown) continue;
    const text = cleanAssistantText(markdown, { preserveContentStatusText: true });
    if (text) return { node: markdown, text };
  }
  return null;
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
        && !isCitationSourceAffordance(node)
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
    '[data-message-author-role="assistant"]',
    '[data-testid*="assistant-message"]',
    '[data-testid*="assistant-response"]',
    '[data-message-author-role="assistant"] [class*="markdown"]',
    '[class*="markdown"]'
  ]) {
    for (const node of Array.from(turn.querySelectorAll?.(selector) ?? [])) {
      if (!looksLikeUserTurn(node)
          && !isCitationSourceAffordance(node, turn)
          && isAssistantContentNode(node, turn)) {
        addContentNode(node);
      }
    }
  }
  const leafContentNodes = leafNodes(contentNodes);
  const textEntries = [];
  for (const node of leafContentNodes) {
    const text = cleanAssistantText(node, {
      preserveContentStatusText: node !== turn,
      assistantTurn: turn
    });
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
  // A copy control can infer a split action <div> as an assistant "turn" even when
  // that row contains no model-authored content. For inferred non-article turns,
  // accept only text structurally outside interactive controls; this rejects a
  // response-action Sources row without dropping legitimate unwrapped div text.
  const inferredText = !isAssistantMarkerNode(turn) && turn.tagName !== "ARTICLE"
    ? textContentOutsideActionControls(turn)
    : null;
  if (inferredText !== null && !normalizeText(inferredText)) {
    return { node: null, text: "" };
  }
  const fallback = inferredText === null
    ? cleanAssistantText(turn, { assistantTurn: turn })
    : cleanAssistantSourceText(inferredText, { assistantTurn: turn });
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

function isCitationSourceAffordance(node, turn = null) {
  for (let current = node; current; current = current.parentElement) {
    if (current === turn) {
      return false;
    }
    const testId = String(current.getAttribute?.("data-testid") ?? "");
    if (testId === "citations-button" || classTokens(current).includes("group/footnote")) {
      return true;
    }
  }
  return false;
}

function assistantBodyTextOf(node, turn = node) {
  const hasCitationAffordance = flattenTree(node)
    .some((candidate) => candidate !== node && isCitationSourceAffordance(candidate, turn));
  if (!hasCitationAffordance) {
    return bodyTextOf(node);
  }
  return normalizeText(textContentWithoutCitationAffordances(node, turn));
}

function textContentWithoutCitationAffordances(node, turn) {
  if (node !== turn && isCitationSourceAffordance(node, turn)) {
    return "";
  }
  const childNodes = Array.from(node?.childNodes ?? []);
  if (childNodes.length > 0) {
    return childNodes
      .map((child) => child?.nodeType === 3
        ? String(child.textContent ?? "")
        : textContentWithoutCitationAffordances(child, turn))
      .join("");
  }
  const children = Array.from(node?.children ?? []);
  if (children.length > 0) {
    return children
      .map((child) => textContentWithoutCitationAffordances(child, turn))
      .join("");
  }
  return String(node?.textContent ?? "");
}

function cleanAssistantText(node, options = {}) {
  // Body text MUST come from textContent, not innerText: a virtualized/clipped long answer node
  // returns only its rendered head via innerText (observed live as a single "I"). textContent
  // returns the full DOM text regardless of layout. Per-line control/status stripping below then
  // removes any code-block "Copy code", sr-only, thought/status, and control lines that
  // textContent may surface. Fall back to innerText only if textContent is empty (defensive).
  const source = assistantBodyTextOf(node, options.assistantTurn ?? node) || textOf(node);
  return cleanAssistantSourceText(source, options);
}

function cleanAssistantSourceText(source, options = {}) {
  const lines = source
    .split(/\n+/)
    .map((line) => normalizeText(line))
    .filter((line) => line && !isAssistantControlLine(line, options));
  return normalizeText(lines.join("\n"));
}

function textContentOutsideActionControls(node) {
  if (!node || node.tagName === "BUTTON" || node.getAttribute?.("role") === "button") {
    return "";
  }
  const childNodes = Array.from(node.childNodes ?? []);
  if (childNodes.length > 0) {
    return childNodes
      .map((child) => child?.nodeType === 3
        ? String(child.textContent ?? "")
        : textContentOutsideActionControls(child))
      .join("");
  }
  const children = Array.from(node.children ?? []);
  if (children.length > 0) {
    const aggregate = normalizeText(node.textContent ?? "");
    const childAggregate = normalizeText(children.map((child) => child.textContent ?? "").join(" "));
    if (aggregate === childAggregate) {
      return children.map((child) => textContentOutsideActionControls(child)).join("");
    }
  }
  return String(node.textContent ?? "");
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
  const effort = "instant|medium|high|extra high|pro";
  return new RegExp(`^(sol|${effort}|pro thinking|thinking)$`, "i").test(value)
    || new RegExp(`^\\d+(?:\\.\\d+)+(?:\\s+sol)?\\s+(?:${effort})$`, "i").test(value)
    || new RegExp(`^gpt[\\s.-]*\\d+(?:[\\s.-]*\\d+)*(?:\\s+sol)?(?:\\s+(?:${effort}|thinking))?$`, "i").test(value);
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
  if (firstVisible(root, [
    'button[data-testid*="stop"]',
    'button[aria-label*="Stop generating" i]',
    'button[aria-label*="Stop streaming" i]'
  ])) {
    return true;
  }

  // ChatGPT Pro can disable its composer Stop button while continuing a
  // same-turn reasoning pass. During that transition the assistant turn keeps
  // an exact "Answer now" control visible; treating the disabled Stop button as
  // idle otherwise lets the waiter return the preceding interim markdown.
  const latestAssistantTurn = findAssistantTurns(root).at(-1);
  return Boolean(latestAssistantTurn && Array.from(latestAssistantTurn.querySelectorAll("button"))
    .some((button) => normalizeText(textOf(button)) === "Answer now"
      && isVisible(button, { allowDisabled: true, allowNoLayout: true })));
}

// Best-effort: click ChatGPT's visible stop-streaming/stop-generating control if
// one is rendered. Mirrors isResponseGenerating's selector list so we click the
// same affordance we use to detect ongoing generation. Returns true if a stop
// control was found and clicked, false if generation was already idle.
// Page teardown remains best-effort; an explicit beforeStopClick authorization
// failure propagates so the caller can report that ownership was unverified.
function findStopGenerating(root = document) {
  return firstVisible(root, [
    'button[data-testid*="stop"]',
    'button[aria-label*="Stop generating" i]',
    'button[aria-label*="Stop streaming" i]'
  ]);
}

export function clickStopGenerating(root = document) {
  const button = findStopGenerating(root);
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

async function clickStopGeneratingAuthorized(root, beforeStopClick) {
  const button = findStopGenerating(root);
  if (!button) {
    return false;
  }
  await beforeStopClick();
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
// waited_ms }. Page teardown is best-effort, but beforeStopClick authorization
// failures propagate so a stale owner cannot click a reused Stop control.
export async function confirmGenerationStopped(root = document, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? 5000);
  const intervalMs = Number(options.intervalMs ?? 250);
  const beforeStopClick = typeof options.beforeStopClick === "function"
    ? options.beforeStopClick
    : null;
  const startedAt = Date.now();
  let stopped = false;
  let reclicked = false;
  try {
    // Already idle (no stop control) → confirmed_idle without waiting.
    if (!isResponseGenerating(root)) {
      return { stopped: false, confirmed_idle: true, waited_ms: 0 };
    }
    stopped = beforeStopClick
      ? await clickStopGeneratingAuthorized(root, beforeStopClick)
      : clickStopGenerating(root);
    while (Date.now() - startedAt < timeoutMs) {
      await sleep(intervalMs);
      if (!isResponseGenerating(root)) {
        return { stopped, confirmed_idle: true, waited_ms: Date.now() - startedAt };
      }
      // Still generating after the first interval: re-click once in case the
      // first click hit a transitional control, then keep polling.
      if (!reclicked) {
        reclicked = true;
        const clicked = beforeStopClick
          ? await clickStopGeneratingAuthorized(root, beforeStopClick)
          : clickStopGenerating(root);
        if (clicked) {
          stopped = true;
        }
      }
    }
    // Timed out still generating: report not-idle so the caller can warn the
    // user the run may still be live server-side.
    return { stopped, confirmed_idle: false, waited_ms: Date.now() - startedAt };
  } catch (error) {
    if (error?.code === "ownership_unverified") {
      throw error;
    }
    // Page torn down / navigated mid-poll. Treat as best-effort: we clicked
    // (maybe), we cannot confirm idle.
    return { stopped, confirmed_idle: false, waited_ms: Date.now() - startedAt };
  }
}
function firstVisible(root, selectors) {
  return firstMatching(root, selectors, { allowHidden: false });
}

function isTranscriptModelControl(node) {
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

async function waitForUploadComplete(root, file, options = {}) {
  const timeoutMs = Number(options.timeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS);
  const intervalMs = Number(options.intervalMs ?? DEFAULT_WAIT_INTERVAL_MS);
  const baselineAttachments = options.baselineAttachments ?? new Set();
  const requiredStableTicks = Math.max(1, Number(options.requiredStableTicks ?? 2));
  const emptyComposerVariantStableTicks = Math.max(
    1,
    Number(options.emptyComposerVariantStableTicks ?? 8)
  );
  const startedAt = Date.now();
  let lastState = "";
  let enabledTicks = 0;
  let emptyComposerVariantTicks = 0;
  while (Date.now() - startedAt < timeoutMs) {
    const error = uploadErrorText(root);
    if (error) {
      throw new Error(`ChatGPT file upload failed: ${error}`);
    }
    const attached = hasAttachmentNamed(root, file.name, baselineAttachments);
    const pending = hasUploadPending(root);
    const sendControl = findSendButtonControl(root, { requireEnabled: false });
    const sendEnabled = Boolean(sendControl && isEnabled(sendControl));
    const legacyCommitSignal = uploadCommitted(root);
    const emptyComposerVariant = Boolean(
      sendControl
      && !sendEnabled
      && !editableText(findComposer(root))
    );
    lastState = `attached=${attached}, pending=${pending}, send_enabled=${sendEnabled}, empty_composer_variant=${emptyComposerVariant}, diagnostics=${sendReadinessDiagnostics(root)}`;
    if (attached && !pending) {
      // Older ChatGPT variants enable Send once the attachment is committed,
      // even with an empty composer. Keep that stronger signal as the fast path.
      if (legacyCommitSignal) {
        enabledTicks += 1;
        emptyComposerVariantTicks = 0;
        if (enabledTicks >= requiredStableTicks) {
          return { upload_commit_signal: "send_enabled" };
        }
      } else if (emptyComposerVariant) {
        // Newer variants keep Send disabled until prompt text exists. A bounded
        // run of a committed chip, no pending marker, a present disabled Send,
        // and an empty composer distinguishes that UI from a transient upload.
        enabledTicks = 0;
        emptyComposerVariantTicks += 1;
        if (emptyComposerVariantTicks >= emptyComposerVariantStableTicks) {
          return { upload_commit_signal: "empty_composer_variant" };
        }
      } else {
        enabledTicks = 0;
        emptyComposerVariantTicks = 0;
      }
    } else {
      enabledTicks = 0;
      emptyComposerVariantTicks = 0;
    }
    await sleep(intervalMs);
  }
  throw new Error(`ChatGPT file upload did not complete for ${file.name} (${lastState})`);
}

// Before prompt insertion, older ChatGPT variants expose upload commitment by
// enabling Send. If a minimal DOM has no Send control, preserve the historical
// chip-only fallback instead of blocking the upload step indefinitely.
function uploadCommitted(root) {
  const present = findSendButtonControl(root, { requireEnabled: false });
  if (!present) {
    return true;
  }
  return Boolean(findSendButtonControl(root, { requireEnabled: true }));
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
  const needle = normalizedAttachmentText(filename);
  if (!needle) {
    return false;
  }
  const candidates = findAttachmentCandidates(root);
  const nameMatched = candidates.some((node) => {
    if (baselineAttachments.has(attachmentNodeKey(node))) {
      return false;
    }
    const text = [
      textOf(node),
      node.getAttribute?.("aria-label"),
      node.getAttribute?.("title")
    ].filter(Boolean).join(" ");
    return attachmentTextMatchesFilename(text, needle);
  });
  if (nameMatched) {
    return true;
  }

  const newTiles = findAttachmentTiles(root, { composerOnly: true })
    .filter((node) => !baselineAttachments.has(attachmentNodeKey(node)));
  return newTiles.length === 1;
}

function attachmentTextMatchesFilename(text, filename) {
  const haystack = normalizedAttachmentText(text);
  if (!haystack || !filename) {
    return false;
  }
  if (haystack.includes(filename)) {
    return true;
  }

  const lastDot = filename.lastIndexOf(".");
  const stem = lastDot > 0 ? filename.slice(0, lastDot) : filename;
  const extension = lastDot > 0 ? filename.slice(lastDot) : "";
  if (!stem) {
    return false;
  }

  const boundary = "[^\\p{L}\\p{N}._-]";
  const pattern = new RegExp(
    `(^|${boundary})${escapeRegExp(stem)}\\s*\\(\\d+\\)${escapeRegExp(extension)}($|${boundary})`,
    "iu"
  );
  return pattern.test(haystack);
}

function normalizedAttachmentText(value) {
  return normalizeText(value).replace(/\s+/g, " ").toLowerCase();
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
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

function manualHandoffPathname(url) {
  try {
    return new URL(String(url ?? ""), "https://chatgpt.com").pathname.toLowerCase();
  } catch {
    return "";
  }
}

function manualHandoffSurfaces(root, { hasTranscript = false } = {}) {
  const explicitInterstitialSelectors = [
    '[role="alert"]',
    '[role="dialog"]',
    '[aria-live="assertive"]'
  ];
  const selectors = hasTranscript
    ? explicitInterstitialSelectors
    : ["main", '[role="main"]', ...explicitInterstitialSelectors];
  const candidates = uniqueElements(
    selectors.flatMap((selector) => Array.from(root.querySelectorAll?.(selector) ?? []))
  )
    .filter((node) => isVisible(node, { allowDisabled: true }));
  return candidates.filter((candidate) => !candidates.some(
    (other) => other !== candidate && containsNode(other, candidate)
  ));
}

function hasManualHandoffShell(root) {
  return MANUAL_HANDOFF_SHELL_SELECTORS.some(
    (selector) => (root.querySelectorAll?.(selector)?.length ?? 0) > 0
  );
}

function collectManualHandoffSurfaceText(node, chunks) {
  if (!node
      || isConversationTurnSurface(node)
      || isManualHandoffShellNode(node)
      || isManualHandoffEditableNode(node)) {
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
    collectManualHandoffSurfaceText(child, chunks);
  }
}

function isManualHandoffShellNode(node) {
  return Boolean(node?.closest?.(MANUAL_HANDOFF_SHELL_SELECTORS.join(",")));
}

function isManualHandoffEditableNode(node) {
  const tag = String(node?.tagName ?? "").toLowerCase();
  if (tag === "input"
      || tag === "textarea"
      || node?.getAttribute?.("contenteditable") === "true") {
    return true;
  }
  return Boolean(node?.closest?.('input, textarea, [contenteditable="true"]'));
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
    model_slug: messageModelSlug(node),
    text_chars: textOf(node).length,
    // text_content_chars exposes the layout-independent textContent length next to the
    // innerText-derived text_chars. On the truncation failure mode this node will show
    // text_chars=1 ("I") while text_content_chars holds the full answer length — a single
    // native inspect then settles whether "I" is an extraction artifact or genuine output.
    text_content_chars: bodyTextOf(node).length,
    text: textOf(node).slice(0, 240)
  };
}

function messageModelSlug(node) {
  const direct = String(node?.getAttribute?.("data-message-model-slug") ?? "").trim();
  if (direct) return direct;
  const owner = node?.closest?.("[data-message-model-slug]")
    ?? node?.querySelectorAll?.("[data-message-model-slug]")?.[0];
  return String(owner?.getAttribute?.("data-message-model-slug") ?? "").trim() || null;
}

function uniqueElements(nodes) {
  return nodes.filter((node, index) => node && nodes.indexOf(node) === index);
}

export function modelSelectionDiagnostics(root = document) {
  const modelButton = findModelButton(root);
  const state = findPickerState(root);
  const familyMenu = findFamilySubmenu(root, state?.menu ?? state?.surface);
  const controlledId = modelButton?.getAttribute?.("aria-controls") ?? null;
  const controlledNode = controlledId ? root.getElementById?.(controlledId) : null;
  return {
    requested_model: CHATGPT_SOL_CHAT_PRO_MODEL,
    current_model_label: modelControlLabel(modelButton),
    current_matches_requested: Boolean(familyIsSol(state?.family_label) && effortIsChatProTier(state)),
    surface_groups: chatSurfaceDiagnostics(root),
    family_status: familyIsSol(state?.family_label) ? "verified" : "unverified",
    effort_status: effortIsChatProTier(state) ? "verified" : "unverified",
    family_label: state?.family_label ?? null,
    picker_shape: state?.shape ?? null,
    surface_trust: state?.surface_trust ?? null,
    effort_control: effortControlDiagnostics(state),
    model_button: modelButton ? elementSummary(modelButton) : null,
    model_button_wiring: modelButton ? {
      aria_expanded: modelButton.getAttribute?.("aria-expanded") ?? null,
      data_state: modelButton.getAttribute?.("data-state") ?? null,
      aria_haspopup: modelButton.getAttribute?.("aria-haspopup") ?? null,
      aria_controls: controlledId,
      controlled_node: controlledNode ? pickerNodeDiagnostics(controlledNode) : null
    } : null,
    picker_activation_exceptions: modelPickerActivationExceptions.get(root) ?? 0,
    advanced_picker_candidates: advancedPickerDiagnostics(root),
    mounted_picker_roles: {
      sliders: Array.from(root.querySelectorAll('[role="slider"]')).slice(0, 10).map(pickerNodeDiagnostics),
      menus: Array.from(root.querySelectorAll('[role="menu"]')).slice(0, 10).map(pickerNodeDiagnostics),
      dialogs: Array.from(root.querySelectorAll('[role="dialog"]')).slice(0, 10).map(pickerNodeDiagnostics)
    },
    visible_options: state?.effort_items?.map((item) => textOf(item)).filter(Boolean).slice(0, 20) ?? [],
    visible_families: familyMenuRadios(familyMenu).map((item) => textOf(item)).filter(Boolean).slice(0, 20),
    legacy_picker: visibleLegacyPickerMarkers(root).slice(0, 10),
    composer: elementSummary(findComposer(root)),
    model_control_scopes: modelControlScopes(root).slice(0, 5).map(elementSummary)
  };
}

function chatSurfaceDiagnostics(root) {
  return Array.from(root?.querySelectorAll?.(CHAT_SURFACE_GROUP_SELECTOR) ?? [])
    .slice(0, 10)
    .map((group) => ({
      group: surfaceVisibilityDiagnostics(group, { allowNoLayout: true }),
      chat: surfaceNodeDiagnostics(group, CHAT_SURFACE_CHAT_SELECTOR),
      work: surfaceNodeDiagnostics(group, CHAT_SURFACE_WORK_SELECTOR)
    }));
}

function surfaceNodeDiagnostics(group, selector) {
  return Array.from(group?.querySelectorAll?.(selector) ?? [])
    .slice(0, 10)
    .map((node) => surfaceVisibilityDiagnostics(node));
}

function surfaceVisibilityDiagnostics(node, options = {}) {
  const rect = node?.getBoundingClientRect?.();
  const style = node?.ownerDocument?.defaultView?.getComputedStyle?.(node);
  const ancestors = pickerAncestorDiagnostics(node);
  return {
    visible: isVisible(node, { allowDisabled: true, ...options }),
    positive_layout: hasPositiveLayout(node),
    hidden: Boolean(node?.hidden),
    aria_hidden: node?.getAttribute?.("aria-hidden") ?? null,
    inert: node?.getAttribute?.("inert") != null,
    aria_checked: node?.getAttribute?.("aria-checked") ?? null,
    data_state: node?.getAttribute?.("data-state") ?? null,
    width: Number(rect?.width) || 0,
    height: Number(rect?.height) || 0,
    display: style?.display ?? null,
    visibility: style?.visibility ?? null,
    opacity: style?.opacity ?? null,
    pointer_events: style?.pointerEvents ?? null,
    content_visibility: style?.contentVisibility ?? null,
    ancestor_chain: ancestors,
    first_non_rendered_ancestor: ancestors.find((ancestor) => ancestor.non_rendered) ?? null
  };
}

const modelPickerActivationExceptions = new WeakMap();

function recordModelPickerActivationException(root) {
  modelPickerActivationExceptions.set(root, (modelPickerActivationExceptions.get(root) ?? 0) + 1);
}

function advancedPickerDiagnostics(root) {
  const matches = Array.from(root.querySelectorAll('div, [role="dialog"]'))
    .filter((node) => /\bAdvanced\b/i.test(textOf(node)) && /\bEffort\b/i.test(textOf(node)));
  return matches
    .filter((node) => !matches.some((candidate) => candidate !== node && node.contains?.(candidate)))
    .slice(0, 10)
    .map((node) => ({
      ...pickerNodeDiagnostics(node),
      sliders: Array.from(node.querySelectorAll('[role="slider"]')).slice(0, 10).map(pickerNodeDiagnostics)
    }));
}

function pickerNodeDiagnostics(node) {
  const ancestors = pickerAncestorDiagnostics(node);
  const style = node?.ownerDocument?.defaultView?.getComputedStyle?.(node);
  let checkVisibility = null;
  try {
    checkVisibility = typeof node?.checkVisibility === "function"
      ? node.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true, contentVisibilityAuto: true })
      : null;
  } catch {
    checkVisibility = "threw";
  }
  return {
    tag: node?.tagName?.toLowerCase?.() ?? "element",
    role: node?.getAttribute?.("role") ?? null,
    data_state: node?.getAttribute?.("data-state") ?? null,
    aria_hidden: node?.getAttribute?.("aria-hidden") ?? null,
    inert: node?.getAttribute?.("inert") != null,
    opacity: style?.opacity ?? null,
    pointer_events: style?.pointerEvents ?? null,
    content_visibility: style?.contentVisibility ?? null,
    check_visibility: checkVisibility,
    inner_text_chars: normalizeText(node?.innerText ?? "").length,
    text_content_chars: normalizeText(node?.textContent ?? "").length,
    text: textOf(node).slice(0, 240),
    ancestor_chain: ancestors,
    first_non_rendered_ancestor: ancestors.find((ancestor) => ancestor.non_rendered) ?? null
  };
}

function pickerAncestorDiagnostics(node) {
  const ancestors = [];
  let current = node?.parentElement;
  while (current) {
    const style = current.ownerDocument?.defaultView?.getComputedStyle?.(current);
    const display = style?.display ?? null;
    const visibility = style?.visibility ?? null;
    const contentVisibility = style?.contentVisibility ?? null;
    ancestors.push({
      tag: current.tagName?.toLowerCase?.() ?? "element",
      id: String(current.getAttribute?.("id") ?? "").slice(0, 120),
      class: String(current.getAttribute?.("class") ?? "").slice(0, 160),
      hidden: Boolean(current.hidden),
      aria_hidden: current.getAttribute?.("aria-hidden") ?? null,
      inert: current.getAttribute?.("inert") != null,
      display,
      visibility,
      content_visibility: contentVisibility,
      non_rendered: display === "none" || visibility === "hidden" || contentVisibility === "hidden"
    });
    if (current === current.ownerDocument?.body) break;
    current = current.parentElement;
  }
  return ancestors;
}
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
      || style.contentVisibility === "hidden"
      || (!options.allowTransparent && style.opacity === "0")
    )) {
      return false;
    }
    current = current.parentElement;
  }
  const elementStyle = element.ownerDocument?.defaultView?.getComputedStyle?.(element);
  if (!options.allowPointerEventsNone && elementStyle?.pointerEvents === "none") {
    return false;
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

const activeJobs = new Map();
const siteAdapterPromises = new Map();
const siteAdapterModules = Object.freeze({
  chatgpt: "src/sites/chatgpt.js"
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  handleMessage(message)
    .then((payload) => sendResponse({ ok: true, payload }))
    .catch((error) => sendResponse(errorResponse(error)));
  return true;
});

async function handleMessage(message) {
  switch (message?.type) {
    case "yoetz_prepare_job":
      return prepareJob(message.job);
    case "yoetz_bind_job":
      return bindJob(message.job);
    case "yoetz_upload_file":
      return uploadJobFile(message.job, message.file);
    case "yoetz_configure_model":
      return configureModel(message.job);
    case "yoetz_send_prompt":
      return sendPrompt(message.job, message.prompt);
    case "yoetz_extract_response":
      return extractJobResponse(message.job);
    case "yoetz_fetch_conversation":
      return fetchSiteConversationAnswer(message.job, message.conversation_id);
    case "yoetz_cancel_send":
      return cancelSend(message.job);
    case "yoetz_inspect_page":
      return inspectPage(message.run_id, {
        conversation_id: message.conversation_id,
        include_page_text: Boolean(message.include_page_text),
        recipe: message.recipe
      });
    case "yoetz_auth_probe":
      return authProbe(message.recipe);
    case "yoetz_probe":
      return probe(message.recipe);
    default:
      throw new Error(`unknown content-script command ${message?.type}`);
  }
}

// Best-effort cancel: click the site's stop control, then WAIT for generation to
// actually go idle before returning so the service worker does not remove the
// tab while the abort request to OpenAI is still in flight (closing the tab
// early lets the server keep generating). Returns confirmed_idle so the worker
// can report a truthful stop status to the CLI.
//
// Intentionally does NOT call assertJobOwnership — the cancel may arrive after
// the tab has navigated, lost its window.name marker, or after the content
// script reloaded (in which case activeJobs is empty). Cancel is a kill, not a
// safe-tab-only operation; the service worker is already going to remove the
// tab right after this regardless of outcome. confirmGenerationStopped never
// throws, so cancel stays best-effort.
async function cancelSend(job) {
  const { confirmGenerationStopped } = await domHelpers(job);
  const result = await confirmGenerationStopped(document);
  return {
    stopped: Boolean(result?.stopped),
    confirmed_idle: Boolean(result?.confirmed_idle),
    waited_ms: Number(result?.waited_ms ?? 0)
  };
}

async function prepareJob(job) {
  const {
    classifyManualHandoff,
    ensureConversationLoaded,
    ensureFreshChat,
    getPageText,
    markOwnership,
    ownedWindowName
  } = await domHelpers(job);
  activeJobs.delete(job.job_id);
  const handoff = classifyManualHandoff({
    url: location.href,
    title: document.title,
    text: getPageText(document)
  });
  const conversationId = conversationIdForJob(job);
  if (!handoff && conversationId) {
    assertUrlRunMarker(job);
  }
  const conversation = !handoff && conversationId
    ? await ensureConversationLoaded(document, conversationId, conversationLoadOptionsForJob(job))
    : null;
  const freshChat = !handoff && !conversationId
    ? await ensureFreshChat(document, job)
    : null;
  if (!handoff) {
    if (conversationId) {
      assertUrlRunMarker(job);
    }
    window.name = ownedWindowName(job);
    markOwnership(document, job);
    activeJobs.set(job.job_id, { ...job, prepare_complete: true });
  }
  return {
    url: location.href,
    title: document.title,
    window_name: window.name,
    conversation,
    fresh_chat: freshChat,
    manual_handoff: handoff
  };
}

async function uploadJobFile(job, filePayload) {
  const { parseOwnedWindowName, uploadFile } = await domHelpers(job);
  const adapter = await siteAdapter(job);
  assertJobOwnership(job, parseOwnedWindowName, ownershipOptionsForJob(job, "upload", adapter));
  const bytes = base64ToUint8Array(filePayload.bytes_base64);
  const file = new File([bytes], filePayload.filename || "yoetz-bundle.md", {
    type: filePayload.mime_type || "text/markdown"
  });
  await uploadFile(document, file, { timeoutMs: Number(job.upload_timeout_ms) || 120000 });
  return { filename: file.name, size: file.size };
}

async function configureModel(job) {
  const { configureModelState, parseOwnedWindowName } = await domHelpers(job);
  const adapter = await siteAdapter(job);
  assertJobOwnership(job, parseOwnedWindowName, ownershipOptionsForJob(job, "model_selection", adapter));
  return configureModelState(document, job);
}

async function sendPrompt(job, prompt) {
  const adapter = await siteAdapter(job);
  const {
    clickSend,
    insertPrompt,
    parseOwnedWindowName,
    sendAcceptanceBaseline,
    waitForSendAccepted
  } = await domHelpers(job);
  assertJobOwnership(job, parseOwnedWindowName, ownershipOptionsForJob(job, "send", adapter));
  const baseline = sendAcceptanceBaseline(document);
  await insertPrompt(document, prompt, { timeoutMs: 20000 });
  assertJobOwnership(job, parseOwnedWindowName, ownershipOptionsForJob(job, "send", adapter));
  const clickOptions = { timeoutMs: Number(job.send_timeout_ms) || 120000 };
  const expectedConversationId = expectedConversationIdForJob(job);
  if (expectedConversationId) {
    clickOptions.expectedConversationId = expectedConversationId;
  }
  await clickSend(document, clickOptions);
  let accepted;
  try {
    accepted = await waitForSendAccepted(document, baseline, {
      timeoutMs: Number(job.send_timeout_ms) || 120000
    });
  } catch (error) {
    throw commandError(
      "send_acceptance_unknown",
      `${adapter.displayName} send click was committed, but Yoetz could not confirm ${adapter.displayName} accepted the prompt before timeout. If a response eventually appears, do not rerun automatically: ${String(error?.message ?? error)}`,
      {
        phase: "send",
        side_effect_started: true
      }
    );
  }
  const submitted = sendAcceptanceBaseline(document);
  return {
    sent: true,
    ...accepted,
    url: location.href,
    conversation_id: adapter.conversationIdFromUrl(location.href),
    submitted_user_count: submitted.user_count,
    submitted_assistant_count: submitted.assistant_count
  };
}

async function extractJobResponse(job) {
  const adapter = await siteAdapter(job);
  const {
    classifyWaitManualHandoff,
    extractResponse,
    parseOwnedWindowName
  } = await domHelpers(job);
  assertJobOwnership(job, parseOwnedWindowName, { adapter });
  const conversationId = adapter.conversationIdFromUrl(location.href);
  const expectedConversationId = expectedConversationIdForJob(job);
  if (expectedConversationId && conversationId !== expectedConversationId) {
    throw commandError(
      "conversation_changed",
      `tab moved from ${adapter.displayName} conversation ${expectedConversationId} to ${conversationId ?? "(none)"}`,
      {
        phase: "wait_response",
        side_effect_started: true,
        requested_conversation_id: expectedConversationId,
        current_conversation_id: conversationId
      }
    );
  }
  const extraction = extractResponse(document);
  // During response wait, page text includes the user prompt and model output.
  // Handoff classification here must stay on transport/page metadata only.
  const handoff = classifyWaitManualHandoff({
    url: location.href,
    title: document.title,
    extraction
  });
  return {
    ...extraction,
    manual_handoff: handoff,
    url: location.href,
    conversation_id: conversationId
  };
}

// Ask the selected adapter for a backend answer when its finality strategy supports one.
// The worker owns when to call this fallback; the adapter owns site-specific API semantics.
async function fetchSiteConversationAnswer(job, requestedConversationId) {
  const adapter = await siteAdapter(job);
  const { parseOwnedWindowName } = adapter.dom;
  return adapter.fetchConversationAnswer({
    job,
    requestedConversationId,
    parseOwnedWindowName,
    assertJobOwnership,
    expectedConversationId: expectedConversationIdForJob(job),
    locationHref: location.href,
    commandError
  });
}

async function inspectPage(runId, options = {}) {
  const adapter = await siteAdapter(options.recipe);
  const { extractResponse, getPageText, modelSelectionDiagnostics, parseOwnedWindowName } = await domHelpers(options.recipe);
  const parsed = parseOwnedWindowName(window.name);
  const urlRunId = runIdFromUrl(location.href);
  const conversationId = adapter.conversationIdFromUrl(location.href);
  const conversationTarget = String(options.conversation_id ?? "").trim();
  const runMatches = !runId || parsed?.run_id === runId || urlRunId === runId;
  const conversationMatches = Boolean(conversationTarget && conversationId === conversationTarget);
  if (!runMatches && !conversationMatches) {
    throw commandError("run_mismatch", `tab is not owned by Yoetz run or conversation ${runId}`);
  }
  const extraction = extractResponse(document);
  const pageText = getPageText(document);
  const result = {
    url: location.href,
    title: document.title,
    conversation_id: conversationId,
    window_name: window.name,
    ownership: parsed,
    active_job_ids: Array.from(activeJobs.keys()),
    extraction,
    model_selection: modelSelectionDiagnostics(document),
    // Runtime build marker for the CONTENT SCRIPT specifically. Content scripts already injected
    // into open tabs do NOT refresh when the extension is reloaded (only the service worker
    // does), so a stale content script can emit old diagnostics (e.g. snippets without
    // text_content_chars) even when the SW build is current. Surfacing the content-script
    // manifest version here lets an operator detect that stale-injected-script case directly.
    content_script_build: contentScriptBuild(),
    page_text_chars: pageText.length
  };
  if (options.include_page_text) {
    result.page_text_tail = pageText.slice(-500);
  }
  return result;
}

async function authProbe(recipe) {
  const adapter = await siteAdapter(recipe);
  const { classifyManualHandoff, getPageText } = await domHelpers(recipe);
  const text = getPageText(document);
  const handoff = classifyManualHandoff({
    url: location.href,
    title: document.title,
    text
  });
  const authenticated = !handoff;
  return {
    status: authenticated ? "authenticated" : handoff.state,
    authenticated,
    manual_handoff: handoff,
    message: authenticated
      ? `${adapter.displayName} authenticated in this Chrome profile`
      : handoff.message,
    url: location.href,
    title: document.title,
    text_chars: text.length
  };
}

async function probe(recipe) {
  const { getPageText } = await domHelpers(recipe);
  return {
    url: location.href,
    title: document.title,
    text: getPageText(document).slice(0, 2000)
  };
}

async function bindJob(job) {
  const adapter = await siteAdapter(job);
  const { markOwnership, parseOwnedWindowName } = await domHelpers(job);
  const parsed = parseOwnedWindowName(window.name);
  if (parsed?.job_id !== job.job_id || parsed?.run_id !== job.run_id) {
    throw commandError(
      "ownership_lost",
      `tab ownership marker mismatch for job ${job.job_id}`,
      {
        phase: "wait_response",
        side_effect_started: true
      }
    );
  }
  const urlRunId = runIdFromUrl(location.href);
  if (urlRunId && urlRunId !== job.run_id) {
    throw commandError(
      "ownership_lost",
      `tab URL ownership marker mismatch for job ${job.job_id}`,
      {
        phase: "wait_response",
        side_effect_started: true
      }
    );
  }
  const conversationId = adapter.conversationIdFromUrl(location.href);
  const expectedConversationId = expectedConversationIdForJob(job);
  if (expectedConversationId && conversationId !== expectedConversationId) {
    throw commandError(
      "conversation_changed",
      `tab moved from ${adapter.displayName} conversation ${expectedConversationId} to ${conversationId ?? "(none)"}`,
      {
        phase: "wait_response",
        side_effect_started: true,
        requested_conversation_id: expectedConversationId,
        current_conversation_id: conversationId
      }
    );
  }
  markOwnership(document, job);
  activeJobs.set(job.job_id, { ...job, prepare_complete: true });
  return {
    rebound: true,
    url: location.href,
    title: document.title,
    window_name: window.name
  };
}

function assertJobOwnership(job, parseOwnedWindowName, options = {}) {
  const parsed = parseOwnedWindowName(window.name);
  const active = activeJobs.get(job.job_id);
  if (!active?.prepare_complete) {
    throw new Error(`job ${job.job_id} is not active in this tab`);
  }
  if (parsed?.job_id !== job.job_id || parsed?.run_id !== job.run_id) {
    throw new Error(`tab ownership marker mismatch for job ${job.job_id}`);
  }
  if (options.requireConversation) {
    const actualConversationId = options.adapter.conversationIdFromUrl(location.href);
    if (actualConversationId === options.requireConversation) {
      return;
    }
    const code = actualConversationId ? "conversation_changed" : "conversation_not_loaded";
    throw commandError(
      code,
      `job ${job.job_id} expected ${options.adapter.displayName} conversation ${options.requireConversation}, current conversation is ${actualConversationId ?? "(none)"}`,
      {
        phase: options.phase ?? "upload",
        side_effect_started: false,
        requested_conversation_id: options.requireConversation,
        current_conversation_id: actualConversationId
      }
    );
  }
  if (options.requireFresh && options.adapter.isConversationUrl(location.href)) {
    throw commandError("fresh_chat_lost", `job ${job.job_id} is no longer on a fresh ${options.adapter.displayName} page`, {
      phase: "upload",
      side_effect_started: false
    });
  }
}

function ownershipOptionsForJob(job, phase, adapter) {
  const conversationId = conversationIdForJob(job);
  return conversationId
    ? { adapter, requireConversation: conversationId, phase }
    : { adapter, requireFresh: true, phase };
}

function assertUrlRunMarker(job) {
  const urlRunId = runIdFromUrl(location.href);
  if (urlRunId !== job.run_id) {
    throw commandError("run_mismatch", `tab is not owned by Yoetz run ${job.run_id}`, {
      phase: "upload",
      side_effect_started: false
    });
  }
}

function conversationIdForJob(job) {
  return String(job?.conversation_id ?? "").trim() || null;
}

function expectedConversationIdForJob(job) {
  return String(job?.expected_conversation_id ?? job?.submitted_conversation_id ?? job?.conversation_id ?? "").trim() || null;
}

function conversationLoadOptionsForJob(job) {
  const options = {};
  const timeoutMs = Number(job?.upload_timeout_ms);
  if (Number.isFinite(timeoutMs) && timeoutMs > 0) {
    options.timeoutMs = timeoutMs;
  }
  const intervalMs = Number(job?.upload_interval_ms);
  if (Number.isFinite(intervalMs) && intervalMs > 0) {
    options.intervalMs = intervalMs;
  }
  return options;
}

async function domHelpers(jobOrRecipe) {
  return (await siteAdapter(jobOrRecipe)).dom;
}

async function siteAdapter(jobOrRecipe) {
  const requested = typeof jobOrRecipe === "string"
    ? jobOrRecipe
    : jobOrRecipe?.recipe;
  const recipe = requested == null ? "chatgpt" : requested;
  const modulePath = typeof recipe === "string" ? siteAdapterModules[recipe] : null;
  if (!modulePath) {
    throw commandError(
      "unsupported_recipe",
      `recipe ${JSON.stringify(recipe)} is not available in this content-script build; rejected before side effects`,
      { phase: "profile", side_effect_started: false }
    );
  }
  if (!siteAdapterPromises.has(recipe)) {
    siteAdapterPromises.set(recipe, import(chrome.runtime.getURL(modulePath)));
  }
  const module = await siteAdapterPromises.get(recipe);
  if (!module?.siteAdapter) {
    throw commandError(
      "unsupported_recipe",
      `recipe ${JSON.stringify(recipe)} did not expose a site adapter; rejected before side effects`,
      { phase: "profile", side_effect_started: false }
    );
  }
  return module.siteAdapter;
}

function base64ToUint8Array(value) {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function commandError(code, message, detail = {}) {
  const error = new Error(message);
  error.code = code;
  error.phase = detail.phase;
  error.side_effect_started = detail.side_effect_started;
  for (const [key, value] of Object.entries({
    ...conversationLocationDetail(code),
    ...detail
  })) {
    if (!(key in error) && value !== undefined) {
      error[key] = value;
    }
  }
  return error;
}

function conversationLocationDetail(code) {
  if (!String(code ?? "").startsWith("conversation_")) {
    return {};
  }
  return {
    current_url: String(location.href ?? ""),
    current_pathname: String(location.pathname ?? "")
  };
}

function contentScriptBuild() {
  try {
    return chrome.runtime?.getManifest?.().version ?? "unknown";
  } catch {
    return "unknown";
  }
}

function runIdFromUrl(value) {
  try {
    return new URL(value).searchParams.get("_yoetz");
  } catch {
    return null;
  }
}

function errorResponse(error) {
  const response = {
    ok: false,
    error: String(error?.message ?? error)
  };
  if (error?.code) {
    response.code = error.code;
  }
  if (error?.phase) {
    response.phase = error.phase;
  }
  if (typeof error?.side_effect_started === "boolean") {
    response.side_effect_started = error.side_effect_started;
  }
  for (const key of [
    "requested_conversation_id",
    "current_conversation_id",
    "current_url",
    "current_pathname"
  ]) {
    if (error?.[key] !== undefined) {
      response[key] = error[key];
    }
  }
  return response;
}

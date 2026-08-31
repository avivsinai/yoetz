const activeJobs = new Map();
const siteAdapterPromises = new Map();
const siteAdapterModules = Object.freeze({
  chatgpt: "src/sites/chatgpt.js",
  claude: "src/sites/claude.js"
});
// This literal is stamped by the release script. A content script already injected into an open
// tab keeps its old source after an extension reload, so the live manifest version alone is not a
// reliable freshness check.
const CONTENT_SCRIPT_BUILD = "0.5.62";
const NATIVE_JOB_COMMANDS_CAPABILITY = "native_job_commands_v1";
const CHATGPT_CLICK_BOUND_SEND_RECEIPT_CAPABILITY = "chatgpt_click_bound_send_receipt_v1";
const CONTENT_SCRIPT_INSTANCE_ID = `cs_${cryptoRandomId()}`;
const SECURE_CONTENT_SCRIPT_COMMANDS = new Set([
  "yoetz_prepare_job",
  "yoetz_bind_job",
  "yoetz_upload_file",
  "yoetz_configure_model",
  "yoetz_send_prompt",
  "yoetz_extract_response",
  "yoetz_fetch_conversation",
  "yoetz_cancel_send"
]);
const LIFECYCLE_RETRY_ATTEMPTS = 3;
const LIFECYCLE_RETRY_DELAY_MS = 250;

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  handleMessage(message)
    .then((payload) => sendResponse({ ok: true, payload }))
    .catch((error) => sendResponse(errorResponse(error)));
  return true;
});

window.addEventListener("pagehide", (event) => {
  if (event.persisted) {
    void notifyPersistedLifecycle("pagehide");
  }
});

window.addEventListener("pageshow", (event) => {
  if (event.persisted) {
    void notifyPersistedLifecycle("pageshow");
  }
});

async function notifyPersistedLifecycle(event) {
  const jobIds = Array.from(activeJobs.keys());
  if (jobIds.length === 0) {
    return;
  }
  const message = {
    type: "yoetz_content_lifecycle",
    event,
    persisted: true,
    job_ids: jobIds
  };
  const attempts = event === "pageshow" ? LIFECYCLE_RETRY_ATTEMPTS : 1;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      const response = await chrome.runtime.sendMessage(message);
      if (!response?.ok) {
        throw new Error(response?.error ?? "service worker rejected content lifecycle event");
      }
      return;
    } catch (error) {
      if (attempt === attempts) {
        console.warn(`Yoetz ${event} reconnect notification failed: ${String(error?.message ?? error)}`);
        return;
      }
      await new Promise((resolve) => setTimeout(resolve, LIFECYCLE_RETRY_DELAY_MS));
    }
  }
}

async function handleMessage(message) {
  if (message?.type === "yoetz_secure_command") {
    assertContentScriptContract(message);
    const payload = message.payload;
    message = { ...payload, type: message.command };
  }
  switch (message?.type) {
    case "yoetz_prepare_job":
      return prepareJob(message.job);
    case "yoetz_bind_job":
      return bindJob(message.job);
    case "yoetz_upload_file":
      return uploadJobFile(message.job, message.file);
    case "yoetz_configure_model":
      return configureModel(message.job, { reset: message.reset === true });
    case "yoetz_send_prompt":
      return sendPrompt(message.job, message.prompt);
    case "yoetz_extract_response":
      return extractJobResponse(message.job, message.blocking_context);
    case "yoetz_fetch_conversation":
      return fetchSiteConversationAnswer(message.job, message.conversation_id);
    case "yoetz_cancel_send":
      return cancelSend(message.job);
    case "yoetz_verify_job_ownership":
      return verifyJobOwnership(message.job);
    case "yoetz_inspect_page":
      return inspectPage(message.run_id, {
        job_id: message.job_id,
        workspace_id: message.workspace_id,
        ownership_nonce: message.ownership_nonce,
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

function assertContentScriptContract(message) {
  const command = message?.command;
  const payload = message?.payload;
  const contract = message?.content_script_contract;
  const job = payload?.job;
  const recipe = String(job?.recipe ?? "").trim();
  const required = Array.isArray(contract?.required_content_script_capabilities)
    ? contract.required_content_script_capabilities
    : [];
  const actual = contentScriptCapabilitiesForRecipe(recipe);
  const failures = [];
  if (!SECURE_CONTENT_SCRIPT_COMMANDS.has(command)) {
    failures.push(`unsupported command ${JSON.stringify(command)}`);
  }
  if (!payload || typeof payload !== "object" || Array.isArray(payload) || payload.type !== command) {
    failures.push("payload command mismatch");
  }
  if (contract?.content_script_instance_id !== CONTENT_SCRIPT_INSTANCE_ID) {
    failures.push("content-script instance mismatch");
  }
  if (contract?.content_script_build !== CONTENT_SCRIPT_BUILD) {
    failures.push("content-script build mismatch");
  }
  if (contract?.content_script_recipe !== recipe) {
    failures.push("content-script recipe mismatch");
  }
  if (required.length !== actual.length || actual.some((capability) => !required.includes(capability))) {
    failures.push("content-script capability mismatch");
  }
  if (failures.length === 0) {
    return;
  }
  throw commandError(
    "content_script_contract_mismatch",
    `refusing ${String(command ?? "unknown")} for ${failures.join(", ")}`,
    {
      phase: contentScriptCommandPhase(command, job),
      side_effect_started: contentScriptCommandHasSideEffect(command, job),
      content_script_instance_id: CONTENT_SCRIPT_INSTANCE_ID,
      expected_content_script_instance_id: contract?.content_script_instance_id ?? null,
      content_script_build: CONTENT_SCRIPT_BUILD,
      expected_content_script_build: contract?.content_script_build ?? null,
      content_script_recipe: recipe || null,
      expected_content_script_recipe: contract?.content_script_recipe ?? null,
      required_content_script_capabilities: required
    }
  );
}

function contentScriptCapabilitiesForRecipe(recipe) {
  return [
    NATIVE_JOB_COMMANDS_CAPABILITY,
    ...(recipe === "chatgpt" ? [CHATGPT_CLICK_BOUND_SEND_RECEIPT_CAPABILITY] : [])
  ];
}

function contentScriptCommandPhase(command, job) {
  if (command === "yoetz_configure_model") {
    return "model_selection";
  }
  if (command === "yoetz_send_prompt" || command === "yoetz_cancel_send") {
    return "send";
  }
  if (command === "yoetz_extract_response" || command === "yoetz_fetch_conversation") {
    return "wait_response";
  }
  if (command === "yoetz_bind_job" && job?.status === "waiting_response") {
    return "wait_response";
  }
  return "upload";
}

function contentScriptCommandHasSideEffect(command, job) {
  return [
    "yoetz_send_prompt",
    "yoetz_extract_response",
    "yoetz_fetch_conversation",
    "yoetz_cancel_send"
  ].includes(command)
    || (command === "yoetz_bind_job" && job?.status === "waiting_response");
}

// Best-effort cancel: click the site's stop control, then WAIT for generation to
// actually go idle before returning so the service worker does not remove the
// tab while the abort request to OpenAI is still in flight (closing the tab
// early lets the server keep generating). Returns confirmed_idle so the worker
// can report a truthful stop status to the CLI.
//
// Intentionally does NOT call assertJobOwnership — the cancel may arrive after
// the content script reloaded (in which case activeJobs is empty). Instead,
// verify the durable marker and current conversation immediately before each
// provider Stop click. The service worker also probes before sending this
// command and before removing the tab.
async function cancelSend(job) {
  const { confirmGenerationStopped } = await domHelpers(job);
  const beforeStopClick = async () => {
    await verifyJobOwnership(job);
  };
  const result = await confirmGenerationStopped(document, { beforeStopClick });
  return {
    stopped: Boolean(result?.stopped),
    confirmed_idle: Boolean(result?.confirmed_idle),
    waited_ms: Number(result?.waited_ms ?? 0)
  };
}

async function prepareJob(job) {
  const {
    classifyBlockingState,
    classifyManualHandoff,
    ensureConversationLoaded,
    ensureFreshChat,
    manualHandoffContext,
    markOwnership,
    ownedWindowName
  } = await domHelpers(job);
  activeJobs.delete(job.job_id);
  const handoffContext = manualHandoffContext(document);
  const handoff = classifyManualHandoff({
    url: location.href,
    title: handoffContext.title,
    text: handoffContext.text
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
    assertNoBlockingState(classifyBlockingState, {
      phase: "upload",
      side_effect_started: false,
      send_committed: false
    });
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
  const timeoutMs = Number(job.upload_timeout_ms) || 120000;
  const uploadOptions = { timeoutMs };
  if (adapter.recipe === "claude") {
    const stallTimeoutMs = Number(job.attachment_stall_timeout_ms);
    if (Number.isFinite(stallTimeoutMs) && stallTimeoutMs > timeoutMs) {
      uploadOptions.stallTimeoutMs = stallTimeoutMs;
    }
    uploadOptions.initialAttachmentTrace = job.attachment_trace;
  }
  const uploadResult = await uploadFile(document, file, uploadOptions);
  return {
    filename: file.name,
    size: file.size,
    ...(uploadResult?.upload_commit_signal
      ? { upload_commit_signal: uploadResult.upload_commit_signal }
      : {})
  };
}

async function configureModel(job, options = {}) {
  const phase = options.phase ?? "model_selection";
  const sideEffectStarted = options.side_effect_started === true;
  const sendCommitted = options.send_committed === true;
  const {
    classifyBlockingState,
    configureModelState,
    parseOwnedWindowName,
    resetModelSelectionState
  } = await domHelpers(job);
  const adapter = await siteAdapter(job);
  assertJobOwnership(job, parseOwnedWindowName, ownershipOptionsForJob(job, phase, adapter));
  assertNoBlockingState(classifyBlockingState, {
    phase,
    side_effect_started: sideEffectStarted,
    send_committed: sendCommitted
  });
  if (options.reset) {
    await resetModelSelectionState(document);
  }
  let selection;
  try {
    selection = await configureModelState(document, job);
  } catch (error) {
    if (phase !== "send") {
      throw error;
    }
    throw commandError(
      "model_selection_not_verified_before_send",
      `ChatGPT GPT-5.6 Sol Pro was not verified immediately before send: ${String(error?.message ?? error)}`,
      {
        phase: "send",
        side_effect_started: true,
        send_committed: false,
        requested_model: job.model ?? "gpt-5-6-sol-chat-pro",
        model_selection_error_code: error?.code ?? null
      }
    );
  }
  assertNoBlockingState(classifyBlockingState, {
    phase,
    side_effect_started: sideEffectStarted,
    send_committed: sendCommitted
  });
  if (adapter.recipe === "chatgpt" && selection?.surface_evidence_seen === true) {
    const activeJob = activeJobs.get(job.job_id);
    if (activeJob) {
      activeJob.surface_evidence_seen = true;
    }
  }
  return selection;
}

async function sendPrompt(job, prompt) {
  const adapter = await siteAdapter(job);
  const {
    classifyBlockingState,
    clickSend,
    insertPrompt,
    parseOwnedWindowName,
    sendAcceptanceBaseline,
    verifyChatgptModelSelectionBeforeSend,
    waitForSendAccepted
  } = await domHelpers(job);
  assertJobOwnership(job, parseOwnedWindowName, ownershipOptionsForJob(job, "send", adapter));
  // side_effect_started tracks provider-visible job effects, not whether prompt
  // text was inserted. The worker reaches sendPrompt only after bundle upload
  // committed, so every failure from this point is post-side-effect even when
  // send_committed remains false.
  assertNoBlockingState(classifyBlockingState, {
    phase: "send",
    side_effect_started: true,
    send_committed: false
  });
  let finalModelSelection = null;
  let surfaceEvidenceSeen = job.surface_evidence_seen === true
    || activeJobs.get(job.job_id)?.surface_evidence_seen === true;
  job.surface_evidence_seen = surfaceEvidenceSeen;
  const baseline = sendAcceptanceBaseline(document);
  await insertPrompt(document, prompt, { timeoutMs: 20000 });
  assertJobOwnership(job, parseOwnedWindowName, ownershipOptionsForJob(job, "send", adapter));
  assertNoBlockingState(classifyBlockingState, {
    phase: "send",
    side_effect_started: true,
    send_committed: false
  });
  const clickOptions = {
    timeoutMs: Number(job.send_timeout_ms) || 120000,
    requiredStableTicks: 2
  };
  const expectedConversationId = expectedConversationIdForJob(job);
  if (expectedConversationId) {
    clickOptions.expectedConversationId = expectedConversationId;
  }
  if (adapter.recipe === "chatgpt") {
    clickOptions.beforeClick = async () => {
      job.surface_evidence_seen = surfaceEvidenceSeen
        || activeJobs.get(job.job_id)?.surface_evidence_seen === true;
      finalModelSelection = await configureModel(job, {
        phase: "send",
        side_effect_started: true,
        send_committed: false
      });
      if (!adapter.isAcceptableModelSelection(finalModelSelection)) {
        throw commandError(
          "model_selection_not_verified_before_send",
          `ChatGPT GPT-5.6 Sol Pro was not verified immediately before send: ${finalModelSelection?.failure_reason ?? finalModelSelection?.status ?? "unknown"}`,
          {
            phase: "send",
            side_effect_started: true,
            send_committed: false,
            requested_model: finalModelSelection?.requested_model ?? job.model ?? "gpt-5-6-sol-chat-pro",
            model_selection_status: finalModelSelection?.status ?? "unknown",
            model_selection_failure_reason: finalModelSelection?.failure_reason ?? null
          }
        );
      }
      surfaceEvidenceSeen = Boolean(
        surfaceEvidenceSeen
        || finalModelSelection.surface_evidence_seen === true
        || activeJobs.get(job.job_id)?.surface_evidence_seen === true
      );
      job.surface_evidence_seen = surfaceEvidenceSeen;
      finalModelSelection = {
        ...finalModelSelection,
        surface_evidence_seen: surfaceEvidenceSeen
      };
      return finalModelSelection;
    };
    clickOptions.verifyBeforeClick = () => {
      const proof = verifyChatgptModelSelectionBeforeSend(document, finalModelSelection);
      surfaceEvidenceSeen = Boolean(surfaceEvidenceSeen || proof.surface_evidence_seen === true);
      job.surface_evidence_seen = surfaceEvidenceSeen;
      if (!proof.ok) {
        throw commandError(
          "model_selection_not_verified_before_send",
          `ChatGPT GPT-5.6 Sol Pro proof changed or was incomplete immediately before send: ${proof.failure_reason ?? "unknown"}`,
          {
            phase: "send",
            side_effect_started: true,
            send_committed: false,
            requested_model: finalModelSelection?.requested_model ?? job.model ?? "gpt-5-6-sol-chat-pro",
            model_selection_status: finalModelSelection?.status ?? "unknown",
            model_selection_failure_reason: proof.failure_reason,
            surface_failure_reason: proof.failure_reason,
            surface_state: proof.surface_state,
            surface_observed_values: proof.surface_observed_values
          }
        );
      }
      finalModelSelection = {
        ...finalModelSelection,
        model_used: finalModelSelection?.requested_model === "current"
          ? proof.current_closed_pill_text
          : finalModelSelection?.model_used ?? null,
        surface_evidence_seen: surfaceEvidenceSeen,
        surface_state: proof.surface_state,
        surface_observed_values: proof.surface_observed_values,
        surface_proof_kind: proof.surface_proof_kind,
        surface_chat_state: proof.surface_chat_state,
        surface_work_state: proof.surface_work_state,
        surface_visible_toggle_count: proof.surface_visible_toggle_count,
        surface_composer_aria: proof.surface_composer_aria,
        click_bound: true,
        click_bound_closed_pill_text: proof.current_closed_pill_text,
        click_bound_closed_pill_family_status: proof.current_closed_pill_family_status,
        click_bound_closed_pill_effort_status: proof.current_closed_pill_effort_status
      };
      return proof;
    };
  }
  await clickSend(document, clickOptions);
  let accepted;
  try {
    accepted = await waitForSendAccepted(document, baseline, {
      timeoutMs: Number(job.send_timeout_ms) || 120000
    });
  } catch (error) {
    if (error?.code === "usage_credits_exhausted") {
      throw error;
    }
    throw commandError(
      "send_acceptance_unknown",
      `${adapter.displayName} send click was committed, but Yoetz could not confirm ${adapter.displayName} accepted the prompt before timeout. If a response eventually appears, do not rerun automatically: ${String(error?.message ?? error)}`,
      {
        phase: "send",
        side_effect_started: true,
        send_committed: true
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
    submitted_assistant_count: submitted.assistant_count,
    ...(finalModelSelection
      ? {
        final_model_selection: {
          status: finalModelSelection.status ?? null,
          model_used: finalModelSelection.model_used ?? null,
          requested_model: finalModelSelection.requested_model ?? null,
          family_status: finalModelSelection.family_status ?? null,
          effort_status: finalModelSelection.effort_status ?? null,
          picker_family_status: finalModelSelection.picker_family_status ?? null,
          picker_effort_status: finalModelSelection.picker_effort_status ?? null,
          picker_shape: finalModelSelection.picker_shape ?? null,
          post_close_family_status: finalModelSelection.post_close_family_status ?? null,
          post_close_effort_status: finalModelSelection.post_close_effort_status ?? null,
          post_close_picker_shape: finalModelSelection.post_close_picker_shape ?? null,
          post_close_picker_close_verification: finalModelSelection.post_close_picker_close_verification ?? null,
          post_close_closed_pill_family_status: finalModelSelection.post_close_closed_pill_family_status ?? null,
          post_close_closed_pill_effort_status: finalModelSelection.post_close_closed_pill_effort_status ?? null,
          post_close_closed_pill_text: finalModelSelection.post_close_closed_pill_text ?? null,
          post_close_failure_reason: finalModelSelection.post_close_failure_reason ?? null,
          closed_pill_family_status: finalModelSelection.closed_pill_family_status ?? null,
          closed_pill_effort_status: finalModelSelection.closed_pill_effort_status ?? null,
          closed_pill_text: finalModelSelection.closed_pill_text ?? null,
          surface_evidence_seen: finalModelSelection.surface_evidence_seen === true,
          surface_state: finalModelSelection.surface_state ?? null,
          surface_observed_values: finalModelSelection.surface_observed_values ?? [],
          surface_proof_kind: finalModelSelection.surface_proof_kind ?? null,
          surface_chat_state: finalModelSelection.surface_chat_state ?? null,
          surface_work_state: finalModelSelection.surface_work_state ?? null,
          surface_visible_toggle_count: finalModelSelection.surface_visible_toggle_count ?? 0,
          surface_composer_aria: finalModelSelection.surface_composer_aria ?? null,
          picker_close_verification: finalModelSelection.picker_close_verification ?? null,
          click_bound: finalModelSelection.click_bound === true,
          click_bound_closed_pill_text: finalModelSelection.click_bound_closed_pill_text ?? null,
          click_bound_closed_pill_family_status: finalModelSelection.click_bound_closed_pill_family_status ?? null,
          click_bound_closed_pill_effort_status: finalModelSelection.click_bound_closed_pill_effort_status ?? null
        }
      }
      : {})
  };
}

async function extractJobResponse(job, blockingContext = null) {
  const adapter = await siteAdapter(job);
  const {
    classifyBlockingState,
    classifyWaitManualHandoff,
    extractResponse,
    manualHandoffContext,
    parseOwnedWindowName
  } = await domHelpers(job);
  assertJobOwnership(job, parseOwnedWindowName, { adapter });
  const blockingDetail = blockingContext === "pre_send_baseline"
    ? { phase: "send", side_effect_started: true, send_committed: false }
    : { phase: "wait_response", side_effect_started: true, send_committed: true };
  assertNoBlockingState(classifyBlockingState, blockingDetail);
  const conversationId = adapter.conversationIdFromUrl(location.href);
  const expectedConversationId = expectedConversationIdForJob(job);
  if (expectedConversationId
      && conversationId !== expectedConversationId
      && !isExpectedConversationIdAssignment(job, adapter, expectedConversationId, conversationId)) {
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
  assertNoBlockingState(classifyBlockingState, blockingDetail);
  // During response wait, extraction text includes the user prompt and model output.
  // Handoff classification stays on route metadata and the adapter's transcript-free context.
  const handoffContext = manualHandoffContext(document);
  const handoff = classifyWaitManualHandoff({
    url: location.href,
    title: handoffContext.title,
    text: handoffContext.text,
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
  const conversationId = adapter.conversationIdFromUrl(location.href);
  const jobId = String(options.job_id ?? "").trim();
  const workspaceId = String(options.workspace_id ?? "").trim();
  const ownershipNonce = String(options.ownership_nonce ?? "").trim();
  const jobMatches = Boolean(jobId && parsed?.job_id === jobId);
  const runMatches = Boolean(runId && parsed?.run_id === runId);
  const workspaceMatches = Boolean(workspaceId && parsed?.workspace_id === workspaceId);
  const nonceMatches = Boolean(ownershipNonce && parsed?.ownership_nonce === ownershipNonce);
  if (!jobMatches || !runMatches || !workspaceMatches || !nonceMatches) {
    throw commandError("run_mismatch", `tab is not owned by Yoetz job ${jobId || "(unknown)"}, run ${runId}, workspace ${workspaceId || "(unknown)"}`);
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
    current_model_chip_state: modelSelectionDiagnostics(document),
    // Runtime build marker for the CONTENT SCRIPT specifically. Content scripts already injected
    // into open tabs do NOT refresh when the extension is reloaded (only the service worker
    // does), so a stale content script can emit old diagnostics (e.g. snippets without
    // text_content_chars) even when the SW build is current. Surfacing the content-script
    // manifest version here lets an operator detect that stale-injected-script case directly.
    content_script_build: contentScriptBuild(),
    content_script_instance_id: CONTENT_SCRIPT_INSTANCE_ID,
    page_text_chars: pageText.length
  };
  if (options.include_page_text) {
    result.page_text_tail = pageText.slice(-500);
  }
  return result;
}

async function authProbe(recipe) {
  const adapter = await siteAdapter(recipe);
  const {
    classifyManualHandoff,
    getPageText,
    manualHandoffContext
  } = await domHelpers(recipe);
  const text = getPageText(document);
  const handoffContext = manualHandoffContext(document);
  const handoff = classifyManualHandoff({
    url: location.href,
    title: handoffContext.title,
    text: handoffContext.text
  });
  const authenticated = !handoff && handoffContext.authenticated;
  const status = handoff?.state ?? (authenticated ? "authenticated" : "authentication_unknown");
  return {
    status,
    authenticated,
    manual_handoff: handoff,
    message: handoff?.message
      ?? (authenticated
        ? `${adapter.displayName} authenticated in this Chrome profile`
        : `${adapter.displayName} authentication could not be confirmed because its composer is not visible`),
    url: location.href,
    title: document.title,
    text_chars: text.length
  };
}

async function probe(recipe) {
  const adapter = await siteAdapter(recipe);
  const { getPageText } = adapter.dom;
  return {
    recipe: adapter.recipe,
    capabilities: [
      NATIVE_JOB_COMMANDS_CAPABILITY,
      ...(adapter.recipe === "chatgpt"
        ? [CHATGPT_CLICK_BOUND_SEND_RECEIPT_CAPABILITY]
        : [])
    ],
    content_script_build: contentScriptBuild(),
    content_script_instance_id: CONTENT_SCRIPT_INSTANCE_ID,
    url: location.href,
    title: document.title,
    text: getPageText(document).slice(0, 2000)
  };
}

async function bindJob(job) {
  const adapter = await siteAdapter(job);
  const { markOwnership, parseOwnedWindowName } = await domHelpers(job);
  const bindDetail = job.status === "waiting_for_file"
    ? { phase: "upload", side_effect_started: false }
    : { phase: "wait_response", side_effect_started: true };
  const parsed = parseOwnedWindowName(window.name);
  if (!ownershipMarkerMatchesJob(parsed, job)) {
    throw commandError(
      "ownership_lost",
      `tab ownership marker mismatch for job ${job.job_id}`,
      bindDetail
    );
  }
  const urlRunId = runIdFromUrl(location.href);
  if (urlRunId && urlRunId !== job.run_id) {
    throw commandError(
      "ownership_lost",
      `tab URL ownership marker mismatch for job ${job.job_id}`,
      bindDetail
    );
  }
  const conversationId = adapter.conversationIdFromUrl(location.href);
  const expectedConversationId = expectedConversationIdForJob(job);
  if (expectedConversationId
      && conversationId !== expectedConversationId
      && !isExpectedConversationIdAssignment(job, adapter, expectedConversationId, conversationId)) {
    throw commandError(
      "conversation_changed",
      `tab moved from ${adapter.displayName} conversation ${expectedConversationId} to ${conversationId ?? "(none)"}`,
      {
        ...bindDetail,
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

async function verifyJobOwnership(job) {
  const adapter = await siteAdapter(job);
  const { parseOwnedWindowName } = adapter.dom;
  const parsed = parseOwnedWindowName(window.name);
  const urlRunId = runIdFromUrl(location.href);
  const expectedOrigin = new URL(adapter.homeUrl).origin;
  const currentOrigin = new URL(location.href).origin;
  if (currentOrigin !== expectedOrigin || !adapter.isAllowedTabUrl(location.href)) {
    throw commandError(
      "ownership_unverified",
      "tab origin is not an allowed " + adapter.displayName + " page",
      {
        phase: "upload",
        side_effect_started: false,
        expected_origin: expectedOrigin,
        current_origin: currentOrigin,
        current_url: location.href
      }
    );
  }
  if (!ownershipMarkerMatchesJob(parsed, job)) {
    throw commandError(
      "ownership_unverified",
      "tab durable ownership marker does not match the requested job",
      {
        phase: "upload",
        side_effect_started: false,
        expected_job_id: job?.job_id ?? null,
        expected_run_id: job?.run_id ?? null,
        current_ownership: parsed,
        current_url: location.href
      }
    );
  }
  if (urlRunId && urlRunId !== job.run_id) {
    throw commandError(
      "ownership_unverified",
      "tab URL ownership marker does not match the requested run",
      {
        phase: "upload",
        side_effect_started: false,
        expected_run_id: job?.run_id ?? null,
        current_url_run_id: urlRunId,
        current_url: location.href
      }
    );
  }
  const expectedConversationId = expectedConversationIdForJob(job);
  const currentConversationId = adapter.conversationIdFromUrl(location.href);
  if (expectedConversationId
      && currentConversationId !== expectedConversationId
      && !isExpectedConversationIdAssignment(job, adapter, expectedConversationId, currentConversationId)) {
    throw commandError(
      "ownership_unverified",
      "tab conversation does not match the requested job conversation",
      {
        phase: "upload",
        side_effect_started: false,
        requested_conversation_id: expectedConversationId,
        current_conversation_id: currentConversationId,
        current_url: location.href
      }
    );
  }
  return {
    owned: true,
    job_id: job.job_id,
    run_id: job.run_id,
    ...(job.workspace_id != null ? { workspace_id: job.workspace_id } : {}),
    ...(job.ownership_nonce != null ? { ownership_nonce: job.ownership_nonce } : {}),
    origin: currentOrigin,
    window_name: window.name,
    url: location.href
  };
}

function ownershipMarkerMatchesJob(parsed, job) {
  return parsed?.job_id === job?.job_id
    && parsed?.run_id === job?.run_id
    && (job?.workspace_id == null || parsed?.workspace_id === job.workspace_id)
    && (job?.ownership_nonce == null || parsed?.ownership_nonce === job.ownership_nonce);
}

function assertJobOwnership(job, parseOwnedWindowName, options = {}) {
  const phase = options.phase ?? "upload";
  const sideEffectStarted = options.side_effect_started === true;
  const parsed = parseOwnedWindowName(window.name);
  const active = activeJobs.get(job.job_id);
  if (!active?.prepare_complete) {
    throw new Error(`job ${job.job_id} is not active in this tab`);
  }
  if (!ownershipMarkerMatchesJob(parsed, job)) {
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
        phase,
        side_effect_started: sideEffectStarted,
        requested_conversation_id: options.requireConversation,
        current_conversation_id: actualConversationId
      }
    );
  }
  if (options.requireFresh && options.adapter.isConversationUrl(location.href)) {
    throw commandError("fresh_chat_lost", `job ${job.job_id} is no longer on a fresh ${options.adapter.displayName} page`, {
      phase,
      side_effect_started: sideEffectStarted
    });
  }
}

function ownershipOptionsForJob(job, phase, adapter) {
  const conversationId = conversationIdForJob(job);
  const sideEffectStarted = phase === "send" || phase === "wait_response";
  return conversationId
    ? { adapter, requireConversation: conversationId, phase, side_effect_started: sideEffectStarted }
    : { adapter, requireFresh: true, phase, side_effect_started: sideEffectStarted };
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

function isExpectedConversationIdAssignment(job, adapter, expectedConversationId, currentConversationId) {
  return Boolean(adapter.isExpectedConversationIdAssignment?.(
    job,
    expectedConversationId,
    currentConversationId
  ));
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

function assertNoBlockingState(classifyBlockingState, detail) {
  const blockingState = classifyBlockingState?.(document, { forceScan: true });
  if (!blockingState) {
    return;
  }
  throw commandError(blockingState.code, blockingState.message, {
    ...blockingState,
    ...detail
  });
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
  return CONTENT_SCRIPT_BUILD;
}

function cryptoRandomId() {
  const bytes = new Uint8Array(12);
  if (globalThis.crypto?.getRandomValues) {
    globalThis.crypto.getRandomValues(bytes);
  } else {
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = Math.floor(Math.random() * 256);
    }
  }
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
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
  if (error?.attachment_trace !== undefined) {
    response.attachment_trace = error.attachment_trace;
  }
  for (const key of [
    "state",
    "provider_message",
    "provider_dom",
    "requested_model",
    "model_selection_status",
    "model_selection_failure_reason",
    "model_selection_error_code",
    "send_committed",
    "content_script_instance_id",
    "expected_content_script_instance_id",
    "content_script_build",
    "expected_content_script_build",
    "content_script_recipe",
    "expected_content_script_recipe",
    "required_content_script_capabilities",
    "requested_conversation_id",
    "current_conversation_id",
    "current_url",
    "current_pathname",
    "surface_failure_reason",
    "surface_state",
    "surface_observed_values"
  ]) {
    if (error?.[key] !== undefined) {
      response[key] = error[key];
    }
  }
  return response;
}

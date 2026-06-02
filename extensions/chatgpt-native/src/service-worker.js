import { ChunkAssembler, uint8ArrayToBase64 } from "./chunks.js";
import {
  EXTENSION_ID,
  NATIVE_HOST,
  PROTOCOL_VERSION,
  TRANSPORT,
  errorEnvelope,
  makeEnvelope,
  progress,
  validateEnvelope
} from "./protocol.js";
import { chatgptConversationJobUrl, chatgptJobUrl } from "./chatgpt-dom.js";

const DEFAULT_WAIT_TIMEOUT_MS = 90 * 60 * 1000;
const JOB_TTL_MS = 3 * 60 * 60 * 1000;
const HEARTBEAT_ALARM = "yoetz-heartbeat";
const RECONNECT_ALARM = "yoetz-reconnect";
const TERMINAL_STATUSES = new Set(["complete", "cancelled", "failed", "manual_handoff", "state_lost", "terminal_delivery_lost"]);
const EXTENSION_ID_STORAGE_KEY = "yoetz_extension_instance_id";
const MIN_STABLE_IDLE_MS = Number(globalThis.__YOETZ_MIN_STABLE_IDLE_MS ?? 90000);
// Require multiple stable polls so final controls cannot win before late text hydration.
const STABLE_IDLE_INTERVAL_MULTIPLIER = Number(globalThis.__YOETZ_STABLE_IDLE_INTERVAL_MULTIPLIER ?? 3);
const MAX_FINAL_AFFORDANCE_IDLE_MS = Math.max(
  MIN_STABLE_IDLE_MS,
  Number(globalThis.__YOETZ_MAX_FINAL_AFFORDANCE_IDLE_MS ?? 5 * 60 * 1000) || 5 * 60 * 1000
);
// Once ChatGPT has exposed a final assistant affordance (copy control) AND scoped
// text is extractable AND generation has stopped, the response is structurally
// complete. Confirm it over a SHORT stable window at a FAST cadence instead of
// waiting out the full MIN_STABLE_IDLE_MS late-hydration floor. The dual-candidate
// latch still re-arms this window on any text growth (selectFinalAffordanceCandidate
// -> resetTimer), so a still-hydrating response cannot complete early; this only
// removes the dead wall-clock wait after the text has genuinely settled. This is the
// post-affordance confirm window and is always clamped to the slower idle floor so it
// can only ever shorten the wait, never lengthen it.
const MIN_AFFORDANCE_CONFIRM_MS = Math.max(
  0,
  Number(globalThis.__YOETZ_MIN_AFFORDANCE_CONFIRM_MS ?? 8000)
);
// Fast poll cadence used only while a final affordance is latched, so the short
// confirm window is actually sampled across several polls instead of a single coarse
// 30s tick overshooting it.
const AFFORDANCE_CONFIRM_POLL_MS = Math.max(
  250,
  Number(globalThis.__YOETZ_AFFORDANCE_CONFIRM_POLL_MS ?? 1500) || 1500
);
const MAX_NATIVE_OUTBOUND_BYTES = Math.max(
  1024,
  Number(globalThis.__YOETZ_MAX_NATIVE_OUTBOUND_BYTES ?? 64 * 1024 * 1024) || 64 * 1024 * 1024
);
const WAITING_RESPONSE_PROGRESS_INTERVAL_MS = Math.max(50, Number(globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS ?? 60000) || 60000);
const JOBS_KEY_PREFIX = "jobs.";
const LEGACY_JOBS_KEY = "jobs";
// Cap for the tail of last_response_progress_text persisted to chrome.storage.session.
// The full streaming text remains on the in-memory job for delta calculation; only the
// tail is written to disk so a multi-MB Pro response cannot blow the 10MB session quota.
const RESPONSE_TEXT_PERSIST_TAIL = 8 * 1024;

const jobs = new Map();
const terminalJobIds = new Map();
const chunks = new ChunkAssembler();
let nativePort = null;
let extensionIdentityPromise = null;
let connectionGeneration = 0;

chrome.runtime.onInstalled.addListener(() => {
  connectNative();
});

chrome.runtime.onStartup.addListener(() => {
  connectNative();
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message?.type === "yoetz_popup_status") {
    getStatus().then(sendResponse);
    return true;
  }
  if (message?.type === "yoetz_reconnect") {
    reconnectNative();
    sendResponse({ ok: true });
    return true;
  }
  return false;
});

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === HEARTBEAT_ALARM) {
    if (nativePort) {
      postNative(makeEnvelope("heartbeat", { payload: { status: "alive" } }));
    } else {
      connectNative();
    }
    // Sweep expired job shards opportunistically on the heartbeat tick, not per
    // persist, so a job that writes its shard repeatedly does not pay O(jobs)
    // for the TTL scan on every save.
    cleanupExpiredJobs().catch(() => {
      // Best effort — sweep is purely a storage hygiene concern.
    });
  }
  if (alarm.name === RECONNECT_ALARM) {
    connectNative();
  }
});

connectNative();

function connectNative() {
  if (nativePort) {
    return;
  }
  try {
    nativePort = chrome.runtime.connectNative(NATIVE_HOST);
    connectionGeneration += 1;
    nativePort.onMessage.addListener(handleNativeMessage);
    nativePort.onDisconnect.addListener(handleNativeDisconnect);
    setStatus("connected");
    postHello();
    startHeartbeat();
    restoreJobsFromStorage({ emitLostState: true }).catch((error) => {
      setStatus("restore_failed", String(error?.message ?? error));
    });
  } catch (error) {
    setStatus("missing_native_host", String(error?.message ?? error));
    scheduleReconnect();
  }
}

function reconnectNative() {
  if (nativePort) {
    try {
      nativePort.disconnect();
    } catch {
      // Best effort disconnect before reconnect.
    }
  }
  nativePort = null;
  chrome.alarms.clear(RECONNECT_ALARM);
  connectNative();
}

async function handleNativeMessage(message) {
  const validation = validateEnvelope(message);
  if (!validation.ok) {
    const delivered = postNative(errorEnvelope(null, validation.code, validation.message, { request_id: message?.request_id }));
    if (delivered && validation.code === "version_mismatch") {
      await setStatus("version_mismatch", validation.message);
    }
    return;
  }

  try {
    enforceMessageCapability(message);
    switch (message.type) {
      case "job_start":
        await startJob(message);
        break;
      case "job_file_chunk":
        await acceptFileChunk(message);
        break;
      case "job_cancel":
        await cancelJob(message);
        break;
      case "pair_request":
        await completePairing(message);
        break;
      case "heartbeat":
        postNative(makeEnvelope("heartbeat", { payload: { status: "alive" } }));
        break;
      case "reconnect":
        await handleReconnect(message);
        break;
      case "inspect_run":
        await handleInspectRun(message);
        break;
      case "request_identity_permission":
        await handleRequestIdentityPermission(message);
        break;
      default:
        postNative(errorEnvelope(message, "unsupported_type", `unsupported service-worker message ${message.type}`));
    }
  } catch (error) {
    const job = message?.job_id ? jobs.get(message.job_id) : null;
    if (job) {
      const code = error?.code ?? "extension_error";
      const detail = errorContextForJob(job, error);
      await failJob(
        job,
        code,
        jobErrorMessage(job, error, code, detail),
        detail
      );
    } else {
      postNative(errorEnvelope(message, "extension_error", String(error?.message ?? error), {
        request_id: message?.request_id
      }));
    }
  }
}

async function startJob(message) {
  cleanupTerminalJobIds();
  if (jobs.has(message.job_id) || terminalJobIds.has(message.job_id)) {
    postNative(errorEnvelope(messageJob(message), "duplicate_job", `job ${message.job_id} is already known to this extension instance`, {
      request_id: message.request_id,
      phase: "profile",
      side_effect_started: false
    }));
    return;
  }
  const job = normalizeJob(message);
  job.started_at = Date.now();
  job.updated_at = Date.now();
  job.connection_generation = connectionGeneration;

  if (job.conversation_error) {
    await failJob(job, "invalid_conversation", job.conversation_error.message, {
      phase: "upload",
      side_effect_started: false
    });
    return;
  }

  const targetProfile = await validateTargetProfile(job);
  if (!targetProfile.ok) {
    await failJob(job, targetProfile.code, targetProfile.message, targetProfile.detail);
    return;
  }

  job.expected_conversation_id = job.conversation_id ?? null;
  job.status = "opening_tab";
  jobs.set(job.job_id, job);
  await persistJob(job);

  const url = job.expected_conversation_id
    ? chatgptConversationJobUrl(job.expected_conversation_id, job.run_id)
    : chatgptJobUrl(job.run_id);
  const tab = await chrome.tabs.create({ url, active: false });
  job.tab_id = tab.id;
  job.updated_at = Date.now();
  await persistJob(job);
  const inspectCommand = inspectCommandForJob(job);
  if (!postNative(progress(job, "tab_opened", {
    tab_id: tab.id,
    url,
    inspect_command: inspectCommand,
    message: `opened yoetz-owned ChatGPT tab ${url}; inspect with: ${inspectCommand}`
  }))) {
    await recordTerminalDeliveryLost(job, "upload");
    return;
  }

  await waitForChatgptTab(tab.id);
  await waitForContentScript(tab.id);
  const prepared = await sendToTab(tab.id, { type: "yoetz_prepare_job", job });
  if (prepared.manual_handoff) {
    postNative(progress(job, "manual_handoff", prepared.manual_handoff));
    await failJob(job, "manual_handoff", prepared.manual_handoff.message, {
      state: prepared.manual_handoff.state,
      phase: "upload",
      side_effect_started: true,
      terminal_status: "manual_handoff"
    });
    return;
  }
  const modelSelection = await sendToTab(tab.id, { type: "yoetz_configure_model", job });
  job.model_used = modelSelection.model_used ?? null;
  job.model_selection_status = modelSelection.status ?? "unavailable";
  job.warnings = [
    ...(Array.isArray(modelSelection.warnings) ? modelSelection.warnings : []),
    ...(modelSelection.warning ? [modelSelection.warning] : [])
  ];
  if (!postNative(progress(job, "model_selection", modelSelection))) {
    await recordTerminalDeliveryLost(job, "model_selection");
    return;
  }
  if (!isAcceptableModelSelection(modelSelection)) {
    await failJob(job, "model_selection_failed", `Requested ChatGPT model was not selected: ${modelSelection.status ?? "unknown"}`, {
      phase: "model_selection",
      side_effect_started: false,
      requested_model: job.model,
      model_used: job.model_used,
      model_selection_status: job.model_selection_status,
      model_selection: modelSelection
    });
    return;
  }

  await maybeGroupTab(tab.id, job);
  job.status = "waiting_for_file";
  job.updated_at = Date.now();
  await persistJob(job);
  if (!postNative(progress(job, "ready_for_file", { tab_id: tab.id, message: "ChatGPT tab is ready for bundle upload" }))) {
    await recordTerminalDeliveryLost(job, "upload");
  }
}

async function acceptFileChunk(message) {
  const job = requireJob(message.job_id);
  assertJobConnectionCurrent(job);
  if (!["waiting_for_file", "receiving_file"].includes(job.status)) {
    await failJob(job, "unexpected_chunk", `job ${job.job_id} is not accepting file chunks in status ${job.status}`, {
      phase: "upload",
      side_effect_started: Boolean(job.tab_id)
    });
    return;
  }

  let ack;
  try {
    ack = chunks.accept(message);
  } catch (error) {
    const errorText = String(error?.message ?? error);
    await failJob(job, errorText.includes("oversize_chunk") ? "oversize_chunk" : "invalid_chunk", errorText, {
      phase: "upload",
      side_effect_started: Boolean(job.tab_id)
    });
    return;
  }
  const ackDelivered = postNative(makeEnvelope("job_file_chunk_ack", {
    request_id: message.request_id,
    job_id: job.job_id,
    run_id: job.run_id,
    workspace_id: job.workspace_id,
    capability_token: job.capability_token,
    payload: ack
  }));
  if (!ackDelivered) {
    chunks.discard(job.job_id);
    await recordTerminalDeliveryLost(job, "upload");
    return;
  }
  if (!ack.complete) {
    // Only persist on the status transition from waiting_for_file → receiving_file.
    // Subsequent in-flight chunks live in the in-memory ChunkAssembler; persisting
    // every chunk would amplify storage I/O and, with multi-MB uploads split across
    // many chunks, hammer chrome.storage.session for no recovery benefit.
    const previousStatus = job.status;
    job.updated_at = Date.now();
    if (previousStatus !== "receiving_file") {
      job.status = "receiving_file";
      await persistJob(job);
    } else {
      job.status = "receiving_file";
    }
    return;
  }

  const file = chunks.takeFile(job.job_id);
  job.status = "file_received";
  job.updated_at = Date.now();
  await persistJob(job);
  assertJobConnectionCurrent(job);
  await runJobWithFile(job, file);
}

async function runJobWithFile(job, file) {
  if (job.cancelled) return;
  assertJobConnectionCurrent(job);
  job.status = "uploading_file";
  job.updated_at = Date.now();
  await persistJob(job);
  await sendToTab(job.tab_id, {
    type: "yoetz_upload_file",
    job,
    file: {
      filename: file.filename,
      mime_type: file.mimeType,
      bytes_base64: uint8ArrayToBase64(file.bytes)
    }
  });
  assertJobConnectionCurrent(job);
  if (job.cancelled) return;
  if (!postNative(progress(job, "file_uploaded", {
    filename: file.filename,
    bytes: file.bytes.byteLength,
    message: `bundle uploaded (${file.bytes.byteLength} bytes); sending prompt`
  }))) {
    await recordTerminalDeliveryLost(job, "upload");
    return;
  }

  const prompt = job.prompt ?? "";
  if (prompt) {
    job.response_baseline = await sendToTab(job.tab_id, { type: "yoetz_extract_response", job });
    assertJobConnectionCurrent(job);
    job.status = "sending_prompt";
    job.updated_at = Date.now();
    await persistJob(job);
    const sendResult = await sendToTab(job.tab_id, { type: "yoetz_send_prompt", job, prompt });
    job.submitted_url = sendResult?.url ?? null;
    job.submitted_conversation_id = sendResult?.conversation_id ?? null;
    job.submitted_user_count = Number.isFinite(Number(sendResult?.submitted_user_count))
      ? Number(sendResult.submitted_user_count)
      : null;
    job.submitted_assistant_count = Number.isFinite(Number(sendResult?.submitted_assistant_count))
      ? Number(sendResult.submitted_assistant_count)
      : null;
    assertJobConnectionCurrent(job);
    if (job.cancelled) return;
    const inspectCommand = inspectCommandForJob(job);
    if (!postNative(progress(job, "prompt_sent", {
      timeout_ms: responseWaitTimeoutMs(job),
      inspect_command: inspectCommand,
      yoetz_url: chatgptJobUrl(job.run_id),
      submitted_url: job.submitted_url,
      conversation_id: conversationIdForJob(job),
      conversation_url: conversationUrlForId(conversationIdForJob(job)),
      message: `prompt sent; waiting for ChatGPT response (timeout ${formatDurationForMessage(responseWaitTimeoutMs(job))}); inspect with: ${inspectCommand}`
    }))) {
      await recordTerminalDeliveryLost(job, "send");
      return;
    }
  } else {
    postNative(progress(job, "manual_handoff", { state: "prompt_required", message: "no prompt supplied" }));
    await failJob(job, "manual_handoff", "no prompt supplied", {
      state: "prompt_required",
      phase: "send",
      side_effect_started: true,
      terminal_status: "manual_handoff"
    });
    return;
  }

  job.status = "waiting_response";
  job.response_wait_started_at = Date.now();
  job.updated_at = Date.now();
  await persistJob(job);
  const extraction = await waitForResponse(job);
  assertJobConnectionCurrent(job);
  if (job.cancelled || !extraction) return;
  await completeJobWithExtraction(job, extraction);
}

async function completeJobWithExtraction(job, extraction) {
  const conversationId = conversationIdForJob(job, extraction);
  const completeEnvelope = makeEnvelope("job_complete", {
    job_id: job.job_id,
    run_id: job.run_id,
    workspace_id: job.workspace_id,
    capability_token: job.capability_token,
    payload: {
      tab_id: job.tab_id,
      response: extraction.text,
      extraction_method: extraction.method,
      completion_reason: extraction.completion_reason,
      stable_for_ms: extraction.stable_for_ms,
      assistant_turn_count: extraction.assistant_turn_count ?? extraction.assistant_count ?? 0,
      copy_button_count: extraction.copy_button_count ?? 0,
      conversation_id: conversationId,
      conversation_url: conversationUrlForId(conversationId),
      model_used: job.model_used ?? null,
      model_selection_status: job.model_selection_status ?? "unavailable",
      warnings: [
        ...(job.warnings ?? []),
        ...(extraction.text ? [] : ["empty ChatGPT response extracted"]),
        ...(extraction.warning ? [extraction.warning] : [])
      ]
    }
  });
  const completeBytes = nativeEnvelopeByteLength(completeEnvelope);
  if (completeBytes > MAX_NATIVE_OUTBOUND_BYTES) {
    const inspectCommand = inspectCommandForJob(job);
    await failJob(
      job,
      "response_too_large",
      `ChatGPT response is too large to deliver through chrome-extension-native (${completeBytes} bytes > ${MAX_NATIVE_OUTBOUND_BYTES}); inspect the owned tab with: ${inspectCommand}`,
      {
        phase: "wait_response",
        side_effect_started: true,
        completion_reason: extraction.completion_reason,
        extraction_method: extraction.method,
        response_length: extraction.text?.length ?? 0,
        native_message_bytes: completeBytes,
        max_native_message_bytes: MAX_NATIVE_OUTBOUND_BYTES,
        inspect_command: inspectCommand
      }
    );
    return;
  }
  job.status = "complete";
  job.updated_at = Date.now();
  await persistJob(job);
  const delivered = postNative(completeEnvelope);
  if (!delivered) {
    await recordTerminalDeliveryLost(job, "wait_response");
    return;
  }
  rememberTerminalJob(job.job_id);
  jobs.delete(job.job_id);
  chunks.discard(job.job_id);
}

function conversationIdForJob(job, extraction = null) {
  return job?.submitted_conversation_id ?? extraction?.conversation_id ?? job?.conversation_id ?? null;
}

function conversationUrlForId(conversationId) {
  if (!conversationId) {
    return null;
  }
  return `https://chatgpt.com/c/${encodeURIComponent(conversationId)}`;
}

async function resumeWaitingResponseJob(job) {
  try {
    await waitForChatgptTab(job.tab_id);
    await waitForContentScript(job.tab_id);
    const rebound = await sendToTab(job.tab_id, { type: "yoetz_bind_job", job });
    postNative(progress(job, "content_script_recovered", {
      restored: true,
      url: rebound?.url ?? null,
      title: rebound?.title ?? null
    }));
    const extraction = await waitForResponse(job);
    assertJobConnectionCurrent(job);
    if (job.cancelled || !extraction) return;
    await completeJobWithExtraction(job, extraction);
  } catch (error) {
    if (!jobs.has(job.job_id) || TERMINAL_STATUSES.has(job.status)) {
      return;
    }
    await failJob(
      job,
      error?.code ?? "extension_error",
      String(error?.message ?? error),
      errorContextForJob(job, error)
    );
  }
}

async function cancelJob(message) {
  const job = requireJob(message.job_id);
  assertJobConnectionCurrent(job);
  job.cancelled = true;
  job.status = "cancelled";
  job.updated_at = Date.now();
  chunks.discard(job.job_id);
  await persistJob(job);

  // Best-effort: tell the content script to click ChatGPT's stop control so we
  // do not keep consuming the user's ChatGPT quota for a generation they
  // cancelled. The content script's cancelSend handler does NOT require the
  // ownership marker — cancel is a kill, and the tab may have navigated away,
  // lost its window.name marker, or had its content script reloaded. We never
  // let a content-script failure block the rest of the cancel teardown.
  let stopClicked = false;
  if (job.tab_id) {
    try {
      const stopResult = await sendToTab(job.tab_id, { type: "yoetz_cancel_send", job });
      stopClicked = Boolean(stopResult?.stopped);
    } catch {
      // Tab may already be gone / content script unreachable; cancel proceeds.
    }
  }

  // Close the tab so generation cannot continue in the background. V1 chooses
  // hard removal over chrome.tabGroups.update({ collapsed: true }) into a
  // "yoetz-cancelled" group — removal is the simpler contract (no group cleanup
  // to manage, no risk of a collapsed-but-still-streaming tab consuming quota).
  // If a future revision wants to preserve the tab for forensics, route that
  // here through the tabGroups API instead of chrome.tabs.remove.
  if (job.tab_id && chrome.tabs?.remove) {
    try {
      await chrome.tabs.remove(job.tab_id);
    } catch {
      // Tab already closed by the user, or removal racing with navigation.
    }
  }

  postNative(progress(job, "cancelled", { tab_id: job.tab_id, stop_clicked: stopClicked }));
  postNative(makeEnvelope("job_cancel", {
    request_id: message.request_id,
    job_id: job.job_id,
    run_id: job.run_id,
    workspace_id: job.workspace_id,
    capability_token: job.capability_token,
    payload: { cancelled: true, stop_clicked: stopClicked }
  }));
  // Mark terminal and evict from the in-memory map AFTER the cancel envelope is
  // posted and the job's terminal status is persisted. Subsequent extract /
  // send / chunk messages for this job_id will hit requireJob → "unknown job",
  // and a fresh job_start with the same id will be rejected as duplicate_job
  // until the terminalJobIds TTL expires.
  rememberTerminalJob(job.job_id);
  jobs.delete(job.job_id);
}

async function completePairing(message) {
  postNative(makeEnvelope("pair_complete", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    payload: {
      extension_id: EXTENSION_ID,
      extension_version: chrome.runtime.getManifest().version,
      protocol_version: PROTOCOL_VERSION,
      paired: true
    }
  }));
}

async function handleReconnect(message) {
  if (message.payload?.intent === "reload_extension") {
    postNative(makeEnvelope("reconnect", {
      request_id: message.request_id,
      job_id: message.job_id,
      run_id: message.run_id,
      workspace_id: message.workspace_id,
      payload: {
        status: "reloading"
      }
    }));
    setTimeout(() => chrome.runtime.reload(), 50);
    return;
  }
  await recoverJobs(message);
}

async function handleInspectRun(message) {
  const runId = String(message.payload?.run_id ?? "").trim();
  if (!runId) {
    postNative(errorEnvelope(messageJob(message), "missing_run_id", "inspect_run requires payload.run_id", {
      request_id: message.request_id
    }));
    return;
  }
  const tabs = await chrome.tabs.query({ url: "https://chatgpt.com/*" });
  const matches = [];
  const errors = [];
  for (const tab of tabs) {
    if (!tab?.id) {
      continue;
    }
    try {
      const inspection = sanitizeInspection(await sendToTab(tab.id, {
        type: "yoetz_inspect_page",
        run_id: runId,
        conversation_id: runId
      }));
      matches.push({
        tab_id: tab.id,
        url: tab.url ?? inspection?.url ?? null,
        title: tab.title ?? inspection?.title ?? null,
        inspection
      });
    } catch (error) {
      const message = String(error?.message ?? error);
      const isRunMismatch = error?.code === "run_mismatch";
      errors.push({
        tab_id: tab.id,
        url: isRunMismatch ? null : (tab.url ?? null),
        title: isRunMismatch ? null : (tab.title ?? null),
        code: error?.code ?? undefined,
        error: message
      });
    }
  }
  if (matches.length === 0) {
    postNative(errorEnvelope(messageJob(message), "run_not_found", `no Yoetz ChatGPT tab found for run ${runId}`, {
      request_id: message.request_id,
      run_id: runId,
      inspected_tabs: errors
    }));
    return;
  }
  postNative(makeEnvelope("job_complete", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: runId,
    payload: {
      run_id: runId,
      // Runtime build marker for the SERVICE WORKER. Lets an operator confirm the live SW is the
      // expected build before trusting (or distrusting) the diagnostics fields below — if this
      // does not match the shipped version, Chrome is running a stale service worker and any
      // missing P2 fields are a reload problem, not a code bug. Each inspected tab also carries
      // content_script_build (see inspectPage) since content scripts in already-open tabs do not
      // refresh on extension reload even when the SW does.
      service_worker_build: serviceWorkerBuild(),
      tabs: matches
    }
  }));
}

// Runtime build marker for the service worker (manifest version of the LIVE SW). Used in the
// inspect payload so an operator can confirm the running SW is the expected build before
// trusting/distrusting the diagnostics fields. Defensive: never throws inside handleInspectRun.
function serviceWorkerBuild() {
  try {
    return chrome.runtime?.getManifest?.().version ?? "unknown";
  } catch {
    return "unknown";
  }
}

function sanitizeInspection(inspection) {
  if (!inspection || typeof inspection !== "object") {
    return inspection;
  }
  const sanitized = { ...inspection };
  delete sanitized.page_text_tail;
  if (sanitized.extraction?.diagnostics && typeof sanitized.extraction.diagnostics === "object") {
    sanitized.extraction = {
      ...sanitized.extraction,
      diagnostics: diagnosticPayload(sanitized.extraction.diagnostics)
    };
  }
  return sanitized;
}

async function handleRequestIdentityPermission(message) {
  const requested = ["identity.email"];
  let alreadyGranted = false;
  let granted = false;
  let error = null;
  try {
    if (chrome.permissions?.contains) {
      alreadyGranted = await chrome.permissions.contains({ permissions: requested });
    }
    if (alreadyGranted) {
      granted = true;
    } else if (chrome.permissions?.request) {
      granted = await chrome.permissions.request({ permissions: requested });
    } else {
      error = "chrome.permissions.request is unavailable in this Chrome version";
    }
  } catch (caught) {
    error = String(caught?.message ?? caught);
  }
  if (granted) {
    extensionIdentityPromise = null;
  }
  postNative(makeEnvelope("job_complete", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    capability_token: message.capability_token,
    payload: {
      status: "ok",
      permission: "identity.email",
      granted,
      already_granted: alreadyGranted,
      error
    }
  }));
}

function messageJob(message) {
  return {
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    capability_token: message.capability_token,
    request_id: message.request_id
  };
}

async function recoverJobs(message) {
  await restoreJobsFromStorage({ emitLostState: true });
  postNative(makeEnvelope("reconnect", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    payload: {
      restored_jobs: Array.from(jobs.keys())
    }
  }));
}

async function restoreJobsFromStorage({ emitLostState = false } = {}) {
  const stored = (await chrome.storage.session.get(null)) ?? {};
  const restored = [];

  // Migrate from the legacy single-map shape ({ jobs: { id: job, ... } }) to the
  // sharded shape ({ "jobs.<id>": job }). Older extensions wrote the whole map on
  // every save, which lost concurrent updates and amplified storage cost; the new
  // shape writes only the touched job. On first run after upgrade we lift entries
  // out of the legacy map, write them as shards, and delete the legacy key so the
  // next run takes the shard fast path.
  const legacyMap = stored[LEGACY_JOBS_KEY];
  if (legacyMap && typeof legacyMap === "object") {
    const migratedShards = {};
    for (const job of Object.values(legacyMap)) {
      if (!job?.job_id) {
        continue;
      }
      restored.push(job);
      migratedShards[jobsStorageKey(job.job_id)] = strippedJobForStorage(job);
    }
    if (Object.keys(migratedShards).length > 0) {
      await chrome.storage.session.set(migratedShards);
    }
    if (chrome.storage.session.remove) {
      await chrome.storage.session.remove(LEGACY_JOBS_KEY);
    }
  }

  for (const [key, value] of Object.entries(stored)) {
    if (!key.startsWith(JOBS_KEY_PREFIX) || !value) {
      continue;
    }
    restored.push(value);
  }

  for (const job of restored) {
    if (!job?.job_id) {
      continue;
    }
    if (Date.now() - (job.updated_at ?? job.started_at ?? 0) > JOB_TTL_MS) {
      continue;
    }
    if (TERMINAL_STATUSES.has(job.status)) {
      continue;
    }
    if (jobs.has(job.job_id)) {
      continue;
    }
    if (canResumeJobAfterWorkerRestart(job)) {
      job.connection_generation = connectionGeneration;
      job.updated_at = Date.now();
      jobs.set(job.job_id, job);
      await persistJob(job);
      postNative(progress(job, "ready_for_file", {
        tab_id: job.tab_id,
        restored: true,
        message: "ChatGPT tab is ready for bundle upload"
      }));
      continue;
    }
    if (canResumeWaitingResponseAfterWorkerRestart(job)) {
      job.connection_generation = connectionGeneration;
      job.response_wait_started_at = job.response_wait_started_at ?? Date.now();
      if (
        !job.last_response_progress_text
        && job.last_response_progress_length === job.last_response_progress_tail?.length
      ) {
        job.last_response_progress_text = job.last_response_progress_tail;
      }
      job.updated_at = Date.now();
      jobs.set(job.job_id, job);
      await persistJob(job);
      if (!postNative(progress(job, "waiting_response", {
        tab_id: job.tab_id,
        restored: true,
        inspect_command: inspectCommandForJob(job),
        message: "restored ChatGPT response wait after service-worker restart"
      }))) {
        await recordTerminalDeliveryLost(job, "wait_response");
        continue;
      }
      void resumeWaitingResponseJob(job);
      continue;
    }
    if (emitLostState) {
      const lostStatus = job.status;
      await failJob(job, "state_lost", `job ${job.job_id} lost in-memory extension state after service-worker restart`, {
        phase: job.delivery_lost_phase ?? phaseForStatus(lostStatus) ?? "upload",
        side_effect_started: Boolean(job.tab_id),
        terminal_status: "state_lost"
      });
    } else {
      jobs.set(job.job_id, job);
    }
  }
}

function canResumeJobAfterWorkerRestart(job) {
  // The tab is prepared and no file chunks have been accepted yet. There is no
  // in-memory ChunkAssembler state to reconstruct, so the native process can
  // continue by sending the first chunk after reconnect.
  return job.status === "waiting_for_file" && Boolean(job.tab_id);
}

function canResumeWaitingResponseAfterWorkerRestart(job) {
  // The prompt has already been accepted by ChatGPT and the only remaining
  // mutable state is the DOM polling loop. Rebind the content script to the
  // persisted owned tab and continue structural-finality polling.
  return job.status === "waiting_response" && Boolean(job.tab_id);
}

function normalizeJob(message) {
  const payload = message.payload ?? {};
  const conversation = normalizeConversationId(payload.conversation_id);
  return {
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    capability_token: message.capability_token,
    request_id: message.request_id,
    prompt: payload.prompt ?? "",
    model: "extended-pro",
    wait_timeout_ms: payload.wait_timeout_ms ?? DEFAULT_WAIT_TIMEOUT_MS,
    wait_interval_ms: payload.wait_interval_ms ?? 30000,
    upload_timeout_ms: payload.upload_timeout_ms ?? 120000,
    send_timeout_ms: payload.send_timeout_ms ?? 120000,
    browser_context_id: payload.browser_context_id ?? null,
    profile_email: payload.profile_email ?? null,
    extension_instance_id: payload.extension_instance_id ?? null,
    extension_profile_id: payload.extension_profile_id ?? null,
    conversation_id: conversation.ok ? conversation.id : null,
    conversation_error: conversation.ok ? null : conversation,
    bundle_size: payload.bundle_size ?? 0,
    file_name: payload.file_name ?? "yoetz-bundle.md",
    model_selection_status: "unavailable",
    model_used: null,
    warnings: [],
    status: "starting"
  };
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

function requireJob(jobId) {
  const job = jobs.get(jobId);
  if (!job) {
    throw new Error(`unknown job ${jobId}`);
  }
  return job;
}

function errorContextForJob(job, error = null) {
  if (!job) {
    return {};
  }
  const phase = phaseForStatus(job.status) ?? (job.tab_id ? "upload" : undefined);
  const detail = {
    phase: error?.phase ?? phase,
    side_effect_started: typeof error?.side_effect_started === "boolean"
      ? error.side_effect_started
      : Boolean(job.tab_id)
  };
  if (job.run_id) {
    detail.inspect_command = inspectCommandForJob(job);
  }
  if (isConversationFailureCode(error?.code)) {
    detail.requested_conversation_id = error?.requested_conversation_id
      ?? job.expected_conversation_id
      ?? job.conversation_id
      ?? null;
    detail.current_conversation_id = error?.current_conversation_id ?? null;
    detail.current_url = error?.current_url ?? job.submitted_url ?? null;
    detail.current_pathname = error?.current_pathname ?? null;
  }
  return detail;
}

function jobErrorMessage(job, error, code, detail = {}) {
  const base = String(error?.message ?? error);
  if (!isConversationFailureCode(code)) {
    return base;
  }
  const requested = detail.requested_conversation_id ?? job?.expected_conversation_id ?? job?.conversation_id ?? "(unknown)";
  const currentUrl = detail.current_url ?? "(unknown)";
  const phase = detail.phase ?? phaseForStatus(job?.status) ?? "upload";
  const inspect = detail.inspect_command ?? inspectCommandForJob(job);
  return `${base}. requested conversation ${requested}; current URL ${currentUrl}; phase ${phase}; inspect with: ${inspect}`;
}

function isConversationFailureCode(code) {
  return String(code ?? "").startsWith("conversation_");
}

function postHello() {
  extensionIdentity().then((identity) => {
    if (!nativePort) {
      return;
    }
    postNative(makeEnvelope("hello", {
      payload: {
        extension_id: EXTENSION_ID,
        extension_version: chrome.runtime.getManifest().version,
        protocol_version: PROTOCOL_VERSION,
        extension_instance_id: identity.extension_instance_id,
        profile_email: identity.profile_email || null,
        profile_id: identity.profile_id || null
      }
    }));
  }).catch(async (error) => {
    setStatus("connected", `profile identity unavailable: ${String(error?.message ?? error)}`);
    if (!nativePort) {
      return;
    }
    let extensionInstanceId = null;
    try {
      extensionInstanceId = await extensionInstanceIdFromStorage();
    } catch {
      // Keep hello best-effort even if local storage is unavailable.
    }
    postNative(makeEnvelope("hello", {
      payload: {
        extension_id: EXTENSION_ID,
        extension_version: chrome.runtime.getManifest().version,
        protocol_version: PROTOCOL_VERSION,
        extension_instance_id: extensionInstanceId,
        profile_email: null,
        profile_id: null
      }
    }));
  });
}

async function validateTargetProfile(job) {
  const requestedEmail = normalizeEmail(job.profile_email);
  const requestedExtensionInstanceId = normalizeSelector(job.extension_instance_id);
  const requestedExtensionProfileId = normalizeSelector(job.extension_profile_id);
  if (job.browser_context_id) {
    return {
      ok: false,
      code: "unsupported_browser_context",
      message: "chrome-extension-native cannot target browser_context_id; use profile_email or a CDP transport",
      detail: {
        phase: "profile",
        side_effect_started: false,
        browser_context_id: job.browser_context_id
      }
    };
  }
  if (!requestedEmail && !requestedExtensionInstanceId && !requestedExtensionProfileId) {
    return { ok: true };
  }

  const identity = await extensionIdentity();
  if (requestedExtensionInstanceId && identity.extension_instance_id !== requestedExtensionInstanceId) {
    return {
      ok: false,
      code: "extension_instance_mismatch",
      message: `chrome-extension-native extension instance mismatch: requested ${job.extension_instance_id}, extension profile is ${identity.extension_instance_id}`,
      detail: {
        phase: "profile",
        side_effect_started: false,
        requested_extension_instance_id: job.extension_instance_id,
        extension_instance_id: identity.extension_instance_id
      }
    };
  }
  if (requestedExtensionProfileId && identity.profile_id !== requestedExtensionProfileId) {
    return {
      ok: false,
      code: "extension_profile_mismatch",
      message: `chrome-extension-native extension profile id mismatch: requested ${job.extension_profile_id}, extension profile is ${identity.profile_id || "unavailable"}`,
      detail: {
        phase: "profile",
        side_effect_started: false,
        requested_extension_profile_id: job.extension_profile_id,
        extension_profile_id: identity.profile_id || null,
        extension_instance_id: identity.extension_instance_id
      }
    };
  }
  if (!requestedEmail) {
    return { ok: true };
  }
  const actualEmail = normalizeEmail(identity.profile_email);
  if (!actualEmail) {
    return {
      ok: false,
      code: "profile_identity_unavailable",
      message: `chrome-extension-native cannot verify requested profile_email ${job.profile_email}; Chrome did not expose a signed-in Chrome profile email for this extension profile`,
      detail: {
        phase: "profile",
        side_effect_started: false,
        requested_profile_email: job.profile_email,
        extension_instance_id: identity.extension_instance_id
      }
    };
  }
  if (actualEmail !== requestedEmail) {
    return {
      ok: false,
      code: "profile_mismatch",
      message: `chrome-extension-native Chrome profile email mismatch: requested ${job.profile_email}, extension profile is ${identity.profile_email}`,
      detail: {
        phase: "profile",
        side_effect_started: false,
        requested_profile_email: job.profile_email,
        extension_profile_email: identity.profile_email,
        extension_instance_id: identity.extension_instance_id
      }
    };
  }
  return { ok: true };
}

async function extensionIdentity() {
  if (!extensionIdentityPromise) {
    extensionIdentityPromise = loadExtensionIdentity();
  }
  return extensionIdentityPromise;
}

async function loadExtensionIdentity() {
  const extensionInstanceId = await extensionInstanceIdFromStorage();
  let profile = {};
  // identity.email is now an optional permission. If chrome.permissions.contains
  // is available and reports the permission is not granted, skip the call entirely
  // — Chrome would throw "The 'identity.email' permission is required." Otherwise
  // attempt the call and rely on try/catch to keep routing instance-id-only when
  // the permission is missing or Chrome is signed out.
  let permissionGranted = true;
  if (chrome.permissions?.contains) {
    try {
      permissionGranted = await chrome.permissions.contains({
        permissions: ["identity.email"]
      });
    } catch {
      permissionGranted = true;
    }
  }
  if (permissionGranted && chrome.identity?.getProfileUserInfo) {
    try {
      profile = await chrome.identity.getProfileUserInfo({ accountStatus: "ANY" });
    } catch {
      profile = {};
    }
  }

  return {
    extension_instance_id: extensionInstanceId,
    profile_email: profile?.email || "",
    profile_id: profile?.id || ""
  };
}

async function extensionInstanceIdFromStorage() {
  const stored = await chrome.storage.local.get(EXTENSION_ID_STORAGE_KEY);
  let extensionInstanceId = stored?.[EXTENSION_ID_STORAGE_KEY];
  if (!extensionInstanceId) {
    extensionInstanceId = `ext_${cryptoRandomId()}`;
    await chrome.storage.local.set({ [EXTENSION_ID_STORAGE_KEY]: extensionInstanceId });
  }
  return extensionInstanceId;
}

function normalizeEmail(value) {
  return String(value ?? "").trim().toLowerCase();
}

function normalizeSelector(value) {
  return String(value ?? "").trim();
}

function cryptoRandomId() {
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function waitForChatgptTab(tabId) {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const tab = await chrome.tabs.get(tabId);
    if (tab.status === "complete" && tab.url?.startsWith("https://chatgpt.com/")) {
      return;
    }
    await sleep(500);
  }
  throw new Error(`ChatGPT tab ${tabId} did not load`);
}

async function waitForContentScript(tabId) {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try {
      await sendToTab(tabId, { type: "yoetz_probe" });
      return;
    } catch {
      await sleep(500);
    }
  }
  throw new Error(`Yoetz content script did not become ready in ChatGPT tab ${tabId}`);
}

async function sendToTab(tabId, message) {
  const response = await chrome.tabs.sendMessage(tabId, message);
  if (!response?.ok) {
    throw tabCommandError(response);
  }
  return response.payload;
}

function tabCommandError(response) {
  const error = new Error(response?.error ?? "content script command failed");
  if (response?.code) {
    error.code = response.code;
  }
  if (response?.phase) {
    error.phase = response.phase;
  }
  if (typeof response?.side_effect_started === "boolean") {
    error.side_effect_started = response.side_effect_started;
  }
  for (const key of [
    "requested_conversation_id",
    "current_conversation_id",
    "current_url",
    "current_pathname"
  ]) {
    if (response?.[key] !== undefined) {
      error[key] = response[key];
    }
  }
  return error;
}

async function maybeGroupTab(tabId, job) {
  if (!chrome.tabGroups || !chrome.tabs.group) {
    return;
  }
  try {
    const groupId = await chrome.tabs.group({ tabIds: [tabId] });
    await chrome.tabGroups.update(groupId, {
      title: `Yoetz ${job.run_id}`,
      color: "blue"
    });
    postNative(progress(job, "tab_grouped", { group_id: groupId }));
  } catch (error) {
    postNative(progress(job, "tab_group_skipped", { reason: String(error?.message ?? error) }));
  }
}

async function waitForResponse(job) {
  const startedAt = Number(job.response_wait_started_at) || Date.now();
  job.response_wait_started_at = startedAt;
  const interval = Math.max(500, Math.min(Number(job.wait_interval_ms) || 30000, 30000));
  const finalAffordanceIdleMs = responseStableIdleThresholdMs(interval);
  // The post-affordance confirm window is clamped to the idle floor so it can only
  // shorten the wait for a settled response, never extend it past the late-hydration
  // ceiling (and so test envs that drive MIN_STABLE_IDLE_MS below the confirm default
  // still complete promptly).
  const affordanceConfirmMs = Math.min(MIN_AFFORDANCE_CONFIRM_MS, finalAffordanceIdleMs);
  let best = { method: "none", text: "", is_generating: true };
  let last = { method: "none", text: "", is_generating: true };
  let finalAffordanceCandidate = null;
  let bestFinalAffordanceCandidate = null;
  let finalAffordanceCandidateSinceMs = 0;
  let extractionFailureSinceMs = 0;
  let lastWaitingProgressAt = startedAt;
  const timeoutMs = responseWaitTimeoutMs(job);
  while (Date.now() - startedAt <= timeoutMs) {
    assertJobConnectionCurrent(job);
    if (job.cancelled) {
      return null;
    }
    const extraction = await extractResponseForJob(job);
    assertJobConnectionCurrent(job);
    assertJobConversationCurrent(job, extraction);
    if (extraction?.manual_handoff) {
      postNative(progress(job, "manual_handoff", extraction.manual_handoff));
      await failJob(job, "manual_handoff", extraction.manual_handoff.message, {
        state: extraction.manual_handoff.state,
        phase: "wait_response",
        side_effect_started: true,
        terminal_status: "manual_handoff",
        diagnostics: diagnosticPayload(extraction.diagnostics)
      });
      return null;
    }
    last = extraction ?? last;
    const postSend = isPostSendExtraction(job, extraction);
    const postSendAssistantActivity = isPostSendAssistantActivity(job, extraction, true);
    if (postSend && extraction?.text && extraction.text.length >= best.text.length) {
      best = extraction;
    }
    if (postSend && extraction?.text) {
      postResponseProgress(job, extraction);
      assertJobConnectionCurrent(job);
    }
    const extractionIdle = !extraction?.is_generating;
    const scopedExtractionCandidate = Boolean(
      postSend
      && extractionIdle
      && extraction?.method !== "page_text_fallback"
    );
    const finalAffordance = Boolean(scopedExtractionCandidate && hasFinalAssistantAffordance(extraction));
    // Broad page text is diagnostic only; final controls without scoped text
    // means extraction failed, not that page chrome is safe to return.
    const finalAffordanceWithoutScopedText = Boolean(
      postSendAssistantActivity
      && extraction?.method === "page_text_fallback"
      && !extraction?.is_generating
      && hasFinalAssistantAffordance(extraction)
    );
    let stableForMs = 0;
    if (finalAffordance) {
      // Once ChatGPT exposes final assistant controls, scope and turn checks
      // have already ruled out pre-send content. From here we track the best
      // scoped candidate by text growth so late page chrome cannot replace a
      // completed response, and transient generating blips cannot forget it.
      const bestSelection = selectFinalAffordanceCandidate(bestFinalAffordanceCandidate, extraction);
      bestFinalAffordanceCandidate = bestSelection.candidate;
      const candidateSelection = selectFinalAffordanceCandidate(
        finalAffordanceCandidate ?? bestFinalAffordanceCandidate,
        extraction
      );
      if (!finalAffordanceCandidate || candidateSelection.candidate !== finalAffordanceCandidate) {
        if (!finalAffordanceCandidate || candidateSelection.resetTimer) {
          finalAffordanceCandidateSinceMs = Date.now();
        }
        finalAffordanceCandidate = candidateSelection.candidate;
      } else if (!finalAffordanceCandidateSinceMs) {
        finalAffordanceCandidateSinceMs = Date.now();
      }
      stableForMs = Date.now() - finalAffordanceCandidateSinceMs;
      // The latch above only re-stamps finalAffordanceCandidateSinceMs on a
      // timer-resetting candidate change (first candidate or text growth), so
      // stableForMs is "time since the scoped text last grew". Once that has held
      // for the short confirm window, the response is settled — emit instead of
      // burning the full idle floor.
      if (stableForMs >= affordanceConfirmMs) {
        return completedExtraction(finalAffordanceCandidate, "copy_button", stableForMs);
      }
    } else if (extraction?.is_generating) {
      finalAffordanceCandidate = null;
      finalAffordanceCandidateSinceMs = 0;
    } else if (!postSendAssistantActivity) {
      finalAffordanceCandidate = null;
      bestFinalAffordanceCandidate = null;
      finalAffordanceCandidateSinceMs = 0;
    }
    const awaitingFinalAffordance = Boolean(scopedExtractionCandidate && !finalAffordance);
    if (finalAffordanceWithoutScopedText) {
      if (!extractionFailureSinceMs) {
        extractionFailureSinceMs = Date.now();
      }
      const extractionFailureStableForMs = Date.now() - extractionFailureSinceMs;
      if (extractionFailureStableForMs >= finalAffordanceIdleMs) {
        await failJob(job, "response_extraction_failed", finalAffordanceExtractionFailureMessage(job, extraction, extractionFailureStableForMs), {
          phase: "wait_response",
          side_effect_started: true,
          completion_reason: "final_affordance_without_scoped_text",
          stable_for_ms: extractionFailureStableForMs,
          extraction_method: extraction.method,
          response_length: extraction.text?.length ?? 0,
          assistant_count: extraction.assistant_count ?? 0,
          turn_index: extraction.turn_index ?? -1,
          copy_button_count: extraction.copy_button_count ?? 0,
          diagnostics: diagnosticPayload(extraction.diagnostics)
        });
        return null;
      }
    } else {
      extractionFailureSinceMs = 0;
    }
    const nextDelay = finalAffordance
      // Poll fast while confirming a latched final affordance so the short confirm
      // window is sampled across several ticks rather than overshot by a coarse poll.
      ? Math.min(interval, AFFORDANCE_CONFIRM_POLL_MS)
      : (finalAffordanceWithoutScopedText
          ? Math.min(interval, Math.max(finalAffordanceIdleMs, 500))
          : interval);
    const nowMs = Date.now();
    const elapsedMs = nowMs - startedAt;
    if (nowMs - lastWaitingProgressAt >= WAITING_RESPONSE_PROGRESS_INTERVAL_MS) {
      const waitingDetail = {
        elapsed_ms: elapsedMs,
        timeout_ms: timeoutMs,
        next_poll_ms: nextDelay,
        stable_for_ms: stableForMs,
        final_affordance: finalAffordance,
        extraction_failure_candidate: finalAffordanceWithoutScopedText
      };
      if (awaitingFinalAffordance) {
        waitingDetail.awaiting_final_affordance = true;
        waitingDetail.inspect_command = inspectCommandForJob(job);
      }
      postWaitingResponseProgress(job, extraction, waitingDetail);
      lastWaitingProgressAt = nowMs;
    }
    await sleep(nextDelay);
  }
  const inspectCommand = inspectCommandForJob(job);
  const timeoutSummary = `ChatGPT response did not reach stable completion before timeout (baseline_assistant_count=${job.response_baseline?.assistant_count ?? 0}, best_method=${best.method}, best_text_chars=${best.text?.length ?? 0}, best_assistant_count=${best.assistant_count ?? 0}, best_turn_index=${best.turn_index ?? -1}, best_copy_button_count=${best.copy_button_count ?? 0}, best_is_generating=${Boolean(best.is_generating)}, last_method=${last.method}, last_text_chars=${last.text?.length ?? 0}, last_assistant_count=${last.assistant_count ?? 0}, last_turn_index=${last.turn_index ?? -1}, last_copy_button_count=${last.copy_button_count ?? 0}, last_is_generating=${Boolean(last.is_generating)}, last_diagnostics=${diagnosticSummary(last.diagnostics)}). The owned ChatGPT tab is left open; if it finishes later, recover with: ${inspectCommand}`;
  await failJob(job, "response_timeout", timeoutSummary, {
    phase: "wait_response",
    side_effect_started: true,
    completion_reason: "timeout",
    timeout_ms: timeoutMs,
    inspect_command: inspectCommand,
    baseline_method: job.response_baseline?.method ?? "none",
    baseline_response_length: job.response_baseline?.text?.length ?? 0,
    baseline_assistant_count: job.response_baseline?.assistant_count ?? 0,
    baseline_turn_index: job.response_baseline?.turn_index ?? -1,
    baseline_diagnostics: diagnosticPayload(job.response_baseline?.diagnostics),
    best_method: best.method,
    best_response_length: best.text?.length ?? 0,
    best_assistant_count: best.assistant_count ?? 0,
    best_turn_index: best.turn_index ?? -1,
    best_copy_button_count: best.copy_button_count ?? 0,
    best_is_generating: Boolean(best.is_generating),
    best_diagnostics: diagnosticPayload(best.diagnostics),
    last_method: last.method,
    last_response_length: last.text?.length ?? 0,
    last_assistant_count: last.assistant_count ?? 0,
    last_turn_index: last.turn_index ?? -1,
    last_copy_button_count: last.copy_button_count ?? 0,
    last_is_generating: Boolean(last.is_generating),
    last_diagnostics: diagnosticPayload(last.diagnostics)
  });
  return null;
}

function postResponseProgress(job, extraction) {
  const text = String(extraction?.text ?? "");
  if (!text || text === job.last_response_progress_text) {
    return;
  }
  const previous = String(job.last_response_progress_text ?? "");
  const delta = text.startsWith(previous) ? text.slice(previous.length) : text;
  job.last_response_progress_text = text;
  postNative(progress(job, "response_observed", {
    message: `response observed (${text.length} chars${extraction?.is_generating ? ", still generating" : ""})`,
    response_delta: delta,
    response_length: text.length,
    response_tail: text.slice(-500),
    extraction_method: extraction.method,
    is_generating: Boolean(extraction.is_generating),
    assistant_count: extraction.assistant_count ?? 0,
    turn_index: extraction.turn_index ?? -1,
    copy_button_count: extraction.copy_button_count ?? 0,
    has_copy_button: Boolean(extraction.has_copy_button)
  }));
}

function postWaitingResponseProgress(job, extraction, detail = {}) {
  const elapsedMs = Number(detail.elapsed_ms ?? 0);
  const timeoutMs = Number(detail.timeout_ms ?? responseWaitTimeoutMs(job));
  const finalityStatus = detail.awaiting_final_affordance ? ", waiting for final assistant controls" : "";
  const scopedCopyStatus = extraction?.has_copy_button ? ", scoped_copy_button=true" : ", scoped_copy_button=false";
  postNative(progress(job, "waiting_response", {
    ...detail,
    inspect_command: detail.inspect_command ?? inspectCommandForJob(job),
    message: `waiting for ChatGPT response (${formatDurationForMessage(elapsedMs)} elapsed of ${formatDurationForMessage(timeoutMs)} timeout; method=${extraction?.method ?? "none"}, assistant_count=${extraction?.assistant_count ?? 0}, copy_buttons=${extraction?.copy_button_count ?? 0}${scopedCopyStatus}${extraction?.is_generating ? ", generating" : ""}${finalityStatus})`,
    extraction_method: extraction?.method ?? "none",
    is_generating: Boolean(extraction?.is_generating),
    assistant_count: extraction?.assistant_count ?? 0,
    turn_index: extraction?.turn_index ?? -1,
    copy_button_count: extraction?.copy_button_count ?? 0,
    has_copy_button: Boolean(extraction?.has_copy_button),
    response_length: extraction?.text?.length ?? 0
  }));
}

function responseWaitTimeoutMs(job) {
  return Number(job?.wait_timeout_ms || DEFAULT_WAIT_TIMEOUT_MS);
}

function inspectCommandForJob(job) {
  const selector = job.extension_instance_id ? ` --extension-instance-id ${job.extension_instance_id}` : "";
  return `yoetz browser extension inspect --chatgpt --run-id ${job.run_id}${selector}`;
}

function formatDurationForMessage(ms) {
  const seconds = Math.max(0, Math.round(Number(ms || 0) / 1000));
  if (seconds < 60) {
    return `${seconds}s`;
  }
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  if (remainder === 0) {
    return `${minutes}m`;
  }
  return `${minutes}m ${remainder}s`;
}

function diagnosticSummary(diagnostics) {
  const payload = diagnosticPayload(diagnostics);
  return payload ? JSON.stringify(payload) : "none";
}

function diagnosticPayload(diagnostics) {
  if (!diagnostics) {
    return null;
  }
  return {
    // page_text_content_chars (textContent length) is surfaced alongside the snippet
    // text_content_chars so an operator running `yoetz browser extension inspect` can compare it
    // to the innerText-derived page_text_chars and settle the innerText-vs-textContent truncation
    // fork. Snippets are passed through verbatim below, so each already carries text_content_chars
    // from elementSummary; this only had to re-add the page-level field that the projection dropped.
    page_text_chars: diagnostics.page_text_chars ?? null,
    page_text_content_chars: diagnostics.page_text_content_chars ?? null,
    counts: diagnostics.counts ?? {},
    assistant_turn_snippets: (diagnostics.assistant_turn_snippets ?? []).slice(-3),
    article_snippets: (diagnostics.article_snippets ?? []).slice(-3),
    markdown_snippets: (diagnostics.markdown_snippets ?? []).slice(-3),
    stop_control_snippets: (diagnostics.stop_control_snippets ?? []).slice(0, 3)
  };
}

async function extractResponseForJob(job) {
  try {
    return await sendToTab(job.tab_id, { type: "yoetz_extract_response", job });
  } catch (error) {
    if (!isRecoverableContentScriptError(error) || job.content_script_recovery_attempted) {
      throw error;
    }
    await recoverContentScriptJob(job, error);
    return sendToTab(job.tab_id, { type: "yoetz_extract_response", job });
  }
}

async function recoverContentScriptJob(job, error) {
  job.content_script_recovery_attempted = true;
  job.updated_at = Date.now();
  await persistJob(job);
  postNative(progress(job, "content_script_recovering", {
    reason: String(error?.message ?? error)
  }));
  await waitForContentScript(job.tab_id);
  const rebound = await sendToTab(job.tab_id, { type: "yoetz_bind_job", job });
  postNative(progress(job, "content_script_recovered", {
    url: rebound?.url ?? null,
    title: rebound?.title ?? null
  }));
}

function isRecoverableContentScriptError(error) {
  const message = String(error?.message ?? error);
  return /Could not establish connection|Receiving end does not exist|Extension context invalidated|message port closed|is not active in this tab/i.test(message);
}

function isPostSendExtraction(job, extraction) {
  if (!extraction || extraction.method === "page_text_fallback") {
    return false;
  }
  return isPostSendAssistantActivity(job, extraction);
}

function isPostSendAssistantActivity(job, extraction, allowUnknownTurnIndex = false) {
  if (!extraction) {
    return false;
  }
  const submittedUserCount = nonNegativeFiniteNumber(job.submitted_user_count);
  const precedingUserCount = nonNegativeFiniteNumber(extraction.preceding_user_count);
  if (submittedUserCount !== null && precedingUserCount !== null) {
    if (precedingUserCount < submittedUserCount) {
      return false;
    }
  }
  const baselineCount = Number(job.response_baseline?.assistant_count ?? 0);
  const currentCount = Number(extraction.assistant_count ?? 0);
  const currentTurnIndex = Number(extraction.turn_index ?? -1);
  if (currentCount > baselineCount && currentTurnIndex >= baselineCount) {
    return true;
  }
  if (allowUnknownTurnIndex && currentCount > baselineCount && currentTurnIndex < 0) {
    return true;
  }
  return baselineCount === 0 && currentCount > 0;
}

function nonNegativeFiniteNumber(value) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? number : null;
}

function isAcceptableModelSelection(selection) {
  return selection?.status === "selected"
    && selection?.requested_model === "extended-pro"
    && selection?.extended_status === "required"
    && modelUsedLooksLikeProExtended(selection?.model_used);
}

function modelUsedLooksLikeProExtended(value) {
  const folded = String(value ?? "").toLowerCase();
  return /\bpro\b/.test(folded) && /\bextended\b/.test(folded);
}

function hasFinalAssistantAffordance(extraction) {
  // ChatGPT shows assistant copy controls when a turn is externally complete.
  // Pair this with the scoped extraction and !is_generating checks above.
  return Boolean(!extraction?.is_generating && extraction?.has_copy_button);
}

function responseStableIdleThresholdMs(intervalMs) {
  const interval = Math.max(0, Number(intervalMs) || 0);
  return Math.min(
    MAX_FINAL_AFFORDANCE_IDLE_MS,
    Math.max(MIN_STABLE_IDLE_MS, interval * STABLE_IDLE_INTERVAL_MULTIPLIER)
  );
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

function normalizedResponseText(value) {
  return String(value ?? "")
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function nativeEnvelopeByteLength(message) {
  const json = JSON.stringify(message);
  if (typeof TextEncoder !== "undefined") {
    return new TextEncoder().encode(json).byteLength;
  }
  return json.length;
}

function finalAffordanceExtractionFailureMessage(job, extraction, stableForMs) {
  return `ChatGPT rendered a final assistant affordance but Yoetz could not extract scoped assistant text (method=${extraction?.method ?? "none"}, assistant_count=${extraction?.assistant_count ?? 0}, turn_index=${extraction?.turn_index ?? -1}, copy_button_count=${extraction?.copy_button_count ?? 0}, stable_for_ms=${stableForMs}). Inspect the owned tab with \`yoetz browser extension inspect --chatgpt --run-id ${job.run_id}\` before rerunning.`;
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

function enforceMessageCapability(message) {
  if (!message?.job_id || message.type === "job_start") {
    return;
  }
  const job = jobs.get(message.job_id);
  if (!job?.capability_token) {
    return;
  }
  if (message.capability_token === job.capability_token) {
    return;
  }
  throw commandError("capability_mismatch", `capability token mismatch for job ${message.job_id}`, {
    phase: phaseForStatus(job.status) ?? "upload",
    side_effect_started: Boolean(job.tab_id)
  });
}

function assertJobConnectionCurrent(job) {
  if (job.connection_generation === connectionGeneration) {
    return;
  }
  throw commandError("connection_generation_changed", `job ${job.job_id} was interrupted by native connection restart`, {
    phase: phaseForStatus(job.status) ?? "upload",
    side_effect_started: Boolean(job.tab_id)
  });
}

function assertJobConversationCurrent(job, extraction) {
  const expectedConversationId = job.expected_conversation_id ?? job.submitted_conversation_id ?? job.conversation_id;
  if (!expectedConversationId || !extraction?.conversation_id) {
    return;
  }
  if (expectedConversationId === extraction.conversation_id) {
    return;
  }
  throw commandError("conversation_changed", `job ${job.job_id} moved from ChatGPT conversation ${expectedConversationId} to ${extraction.conversation_id}`, {
    phase: "wait_response",
    side_effect_started: true
  });
}

function commandError(code, message, detail = {}) {
  const error = new Error(message);
  error.code = code;
  if (detail.phase) {
    error.phase = detail.phase;
  }
  if (typeof detail.side_effect_started === "boolean") {
    error.side_effect_started = detail.side_effect_started;
  }
  return error;
}

function rememberTerminalJob(jobId) {
  if (!jobId) {
    return;
  }
  terminalJobIds.set(jobId, Date.now() + JOB_TTL_MS);
  cleanupTerminalJobIds();
}

function cleanupTerminalJobIds() {
  const now = Date.now();
  for (const [jobId, expiresAt] of terminalJobIds.entries()) {
    if (expiresAt <= now) {
      terminalJobIds.delete(jobId);
    }
  }
}

async function failJob(job, code, message, detail = {}) {
  const { terminal_status: terminalStatus, ...payloadDetail } = detail;
  if (job) {
    job.status = terminalStatus ?? "failed";
    job.updated_at = Date.now();
    chunks.discard(job.job_id);
    await persistJob(job);
  }
  const delivered = postNative(errorEnvelope(job, code, message, payloadDetail));
  if (job) {
    if (delivered) {
      rememberTerminalJob(job.job_id);
      jobs.delete(job.job_id);
    } else {
      await recordTerminalDeliveryLost(job, payloadDetail.phase ?? phaseForStatus(job.status) ?? "upload");
    }
  }
}

async function recordTerminalDeliveryLost(job, phase) {
  job.status = "terminal_delivery_lost";
  job.delivery_lost_phase = phase;
  job.updated_at = Date.now();
  await persistJob(job);
}

async function persistJob(job) {
  if (!job?.job_id) {
    return;
  }
  // Shard by job_id so concurrent jobs no longer fight over a single { jobs: {...} }
  // read-modify-write. Each job owns its own key and only rewrites itself; lost
  // updates from interleaved persists are no longer possible.
  await chrome.storage.session.set({
    [jobsStorageKey(job.job_id)]: strippedJobForStorage(job)
  });
}

function jobsStorageKey(jobId) {
  return `${JOBS_KEY_PREFIX}${jobId}`;
}

// Build a JSON-cloneable, size-bounded view of a job for chrome.storage.session.
// last_response_progress_text on the live job holds the FULL streaming text so
// postResponseProgress can compute deltas against the previous tick — but that
// text can be multi-MB on long Pro responses, and persisting it on every status
// transition (or on failJob's error path) would chew through the 10MB session
// quota and risk masking the real failure with a quota throw. We persist only a
// bounded tail plus the length, which is enough to reconstruct progress context
// after a restart without bloating storage.
function strippedJobForStorage(job) {
  const { last_response_progress_text: fullText, ...rest } = job;
  if (typeof fullText === "string" && fullText.length > 0) {
    rest.last_response_progress_length = fullText.length;
    rest.last_response_progress_tail = fullText.length > RESPONSE_TEXT_PERSIST_TAIL
      ? fullText.slice(-RESPONSE_TEXT_PERSIST_TAIL)
      : fullText;
  }
  return rest;
}

async function cleanupExpiredJobs() {
  const stored = (await chrome.storage.session.get(null)) ?? {};
  const cutoff = Date.now() - JOB_TTL_MS;
  const expiredKeys = [];
  for (const [key, value] of Object.entries(stored)) {
    if (!key.startsWith(JOBS_KEY_PREFIX) || !value) {
      continue;
    }
    const stamp = value.updated_at ?? value.started_at ?? 0;
    if (stamp < cutoff) {
      expiredKeys.push(key);
    }
  }
  if (expiredKeys.length > 0 && chrome.storage.session.remove) {
    await chrome.storage.session.remove(expiredKeys);
  }
}

function handleNativeDisconnect() {
  connectionGeneration += 1;
  nativePort = null;
  stopHeartbeat();
  const message = chrome.runtime.lastError?.message;
  setStatus(message ? "missing_native_host" : "disconnected", message);
  scheduleReconnect();
}

function scheduleReconnect() {
  chrome.alarms.create(RECONNECT_ALARM, { delayInMinutes: 0.5 });
}

function startHeartbeat() {
  stopHeartbeat();
  chrome.alarms.create(HEARTBEAT_ALARM, { periodInMinutes: 0.5 });
}

function stopHeartbeat() {
  chrome.alarms.clear(HEARTBEAT_ALARM);
}

function postNative(message) {
  if (!nativePort) {
    return false;
  }
  try {
    nativePort.postMessage(message);
    return true;
  } catch (error) {
    const detail = String(error?.message ?? error);
    connectionGeneration += 1;
    nativePort = null;
    stopHeartbeat();
    void setStatus("missing_native_host", `native port write failed: ${detail}`);
    scheduleReconnect();
    return false;
  }
}

async function setStatus(status, detail = "") {
  await chrome.storage.session.set({
    status: {
      status,
      detail,
      updated_at: new Date().toISOString()
    }
  });
}

async function getStatus() {
  const stored = await chrome.storage.session.get("status");
  return stored.status ?? { status: "disconnected", detail: "", updated_at: null };
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function phaseForStatus(status) {
  const phaseByStatus = {
    starting: "upload",
    opening_tab: "upload",
    waiting_for_file: "upload",
    receiving_file: "upload",
    file_received: "upload",
    uploading_file: "upload",
    sending_prompt: "send",
    waiting_response: "wait_response",
    complete: "wait_response",
    terminal_delivery_lost: "wait_response"
  };
  return phaseByStatus[status];
}

import { ChunkAssembler, uint8ArrayToBase64 } from "./chunks.js";
import { completionWarnings } from "./completion-warnings.js";
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
import { advertisedRecipes, siteAdapterForRecipe } from "./sites/index.js";

const DEFAULT_WAIT_TIMEOUT_MS = 90 * 60 * 1000;
const JOB_TTL_MS = 3 * 60 * 60 * 1000;
const HEARTBEAT_ALARM = "yoetz-heartbeat";
const RECONNECT_ALARM = "yoetz-reconnect";
const TERMINAL_STATUSES = new Set(["complete", "cancelled", "failed", "manual_handoff", "state_lost", "terminal_delivery_lost"]);
const EXTENSION_ID_STORAGE_KEY = "yoetz_extension_instance_id";
const ADVERTISED_RECIPES = Object.freeze(advertisedRecipes());
const TERMINAL_ACK_CAPABILITY = "terminal_ack";
const NATIVE_JOB_COMMANDS_CAPABILITY = "native_job_commands_v1";
const CHATGPT_CLICK_BOUND_SEND_RECEIPT_CAPABILITY = "chatgpt_click_bound_send_receipt_v1";
// This literal is stamped by the release script and must match the content script. The managed
// extension manifest can receive a local reload suffix, so it is not the content-script identity.
const CONTENT_SCRIPT_BUILD = "0.5.65";
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
const ADVERTISED_CAPABILITIES = Object.freeze([
  TERMINAL_ACK_CAPABILITY,
  NATIVE_JOB_COMMANDS_CAPABILITY,
  CHATGPT_CLICK_BOUND_SEND_RECEIPT_CAPABILITY
]);
const MIN_STABLE_IDLE_MS = Number(globalThis.__YOETZ_MIN_STABLE_IDLE_MS ?? 90000);
// Require multiple stable polls so final controls cannot win before late text hydration.
const STABLE_IDLE_INTERVAL_MULTIPLIER = Number(globalThis.__YOETZ_STABLE_IDLE_INTERVAL_MULTIPLIER ?? 3);
const MAX_FINAL_AFFORDANCE_IDLE_MS = Math.max(
  MIN_STABLE_IDLE_MS,
  Number(globalThis.__YOETZ_MAX_FINAL_AFFORDANCE_IDLE_MS ?? 5 * 60 * 1000) || 5 * 60 * 1000
);
const MIN_UNSCOPED_COPY_STABLE_TEXT_CHARS = Math.max(
  1,
  Number(globalThis.__YOETZ_MIN_UNSCOPED_COPY_STABLE_TEXT_CHARS ?? 4096) || 4096
);
const RENDER_FREEZE_SHORT_RESPONSE_MAX_CHARS = Math.max(
  1,
  Number(globalThis.__YOETZ_RENDER_FREEZE_SHORT_RESPONSE_MAX_CHARS ?? 32) || 32
);
const MIN_RENDER_FREEZE_IDLE_MS = Math.max(
  0,
  Number(globalThis.__YOETZ_MIN_RENDER_FREEZE_IDLE_MS ?? MIN_STABLE_IDLE_MS) || MIN_STABLE_IDLE_MS
);
const MAX_RENDER_REFRESH_ATTEMPTS = Math.max(
  0,
  Number.isFinite(Number(globalThis.__YOETZ_MAX_RENDER_REFRESH_ATTEMPTS))
    ? Number(globalThis.__YOETZ_MAX_RENDER_REFRESH_ATTEMPTS)
    : 1
);
// Once a site adapter exposes its final structural signal with scoped text and
// generation stopped, the candidate is sampled at a fast cadence. Adapters with
// a durable final affordance may use the short confirmation window; adapters
// whose signal is idle DOM state require the full stable-idle window. In both
// cases text growth re-arms the candidate timer, so late hydration cannot win.
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
// Once assistant text is visible after send, shorten only the observation
// cadence. The stable-idle threshold remains derived from the configured job
// interval so adaptive polling cannot weaken finality.
const POST_SEND_ASSISTANT_ACTIVITY_POLL_MS = Math.max(
  250,
  Number(globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS ?? 2000) || 2000
);
const RESPONSE_FINALITY_STALL_MS = Math.max(
  0,
  Number.isFinite(Number(globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS))
    ? Number(globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS)
    : 5 * 60 * 1000
);
const MAX_NATIVE_OUTBOUND_BYTES = Math.max(
  1024,
  Number(globalThis.__YOETZ_MAX_NATIVE_OUTBOUND_BYTES ?? 64 * 1024 * 1024) || 64 * 1024 * 1024
);
const MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES = 1024 * 1024;
const TERMINAL_RETRY_INTERVAL_MS = 30000;
const TERMINAL_OUTBOX_KEY_PREFIX = "terminal-outbox.";
const TERMINAL_ACK_KEY_PREFIX = "terminal-ack.";
const CANCEL_PENDING_KEY_PREFIX = "cancel-pending.";
const MAX_TERMINAL_ID_CHARS = 512;
const WAITING_RESPONSE_PROGRESS_INTERVAL_MS = Math.max(50, Number(globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS ?? 60000) || 60000);
const CONTENT_SCRIPT_RECONNECT_ATTEMPTS = Math.max(
  1,
  Number(globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS ?? 40) || 40
);
const CONTENT_SCRIPT_RECONNECT_DELAY_MS = Math.max(
  0,
  Number(globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS ?? 500) || 500
);
const MAX_CONTENT_SCRIPT_RECOVERY_INCIDENTS = 5;
const BACKEND_API_FETCH_COOLDOWN_MS = Math.max(
  0,
  Number.isFinite(Number(globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS))
    ? Number(globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS)
    : 60000
);
const BACKEND_API_CONFIRMATION_MS = Math.max(
  0,
  Number.isFinite(Number(globalThis.__YOETZ_BACKEND_API_CONFIRMATION_MS))
    ? Number(globalThis.__YOETZ_BACKEND_API_CONFIRMATION_MS)
    : 5000
);
const MAX_BACKEND_API_CONSECUTIVE_FAILURES = 3;
const CHATGPT_DOM_ONLY_FINALITY_WARNING = "ChatGPT finality_anchor=dom_only: backend API positive-finality proof was unavailable; response relied on DOM-only completion";
const JOBS_KEY_PREFIX = "jobs.";
const LEGACY_JOBS_KEY = "jobs";
// Cap for the tail of last_response_progress_text persisted to chrome.storage.session.
// The full streaming text remains on the in-memory job for delta calculation; only the
// tail is written to disk so a multi-MB Pro response cannot blow the 10MB session quota.
const RESPONSE_TEXT_PERSIST_TAIL = 8 * 1024;
const ATTACHMENT_TRACE_TIMESTAMP_KEYS = Object.freeze([
  "final_chunk_ack_at_ms",
  "input_resolved_at_ms",
  "files_assigned_at_ms",
  "change_dispatched_at_ms",
  "matching_thumbnail_at_ms",
  "remove_control_at_ms",
  "send_present_at_ms",
  "send_enabled_at_ms",
  "soft_timeout_at_ms",
  "hard_timeout_at_ms"
]);
const ATTACHMENT_TRACE_PENDING_LEGS = new Set([
  "matching_thumbnail",
  "remove_control",
  "send_present",
  "send_enabled"
]);

const jobs = new Map();
const terminalJobIds = new Map();
const contentScriptRecoveries = new Map();
const cancellationOperations = new Map();
const suspensionGates = new Map();
const nativeRestorePromises = new Map();
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
  if (message?.type === "yoetz_content_lifecycle") {
    handleContentLifecycle(message, sender)
      .then((payload) => sendResponse({ ok: true, payload }))
      .catch((error) => sendResponse({ ok: false, error: String(error?.message ?? error) }));
    return true;
  }
  return false;
});

async function handleContentLifecycle(message, sender) {
  if (message.persisted !== true || !["pagehide", "pageshow"].includes(message.event)) {
    throw new Error("invalid persisted content lifecycle event");
  }
  const tabId = sender?.tab?.id;
  if (!Number.isInteger(tabId)) {
    throw new Error("content lifecycle event is missing its sender tab");
  }
  const requestedIds = new Set(Array.isArray(message.job_ids) ? message.job_ids : []);
  const ownedJobs = Array.from(jobs.values()).filter((job) => (
    requestedIds.has(job.job_id)
    && job.tab_id === tabId
    && !TERMINAL_STATUSES.has(job.status)
  ));
  if (message.event === "pagehide") {
    for (const job of ownedJobs) {
      job.content_script_suspended_at = Date.now();
      job.updated_at = Date.now();
      await persistJob(job);
      postNative(progress(job, "content_script_suspended", {
        tab_id: tabId,
        persisted: true,
        message: "owned background tab entered bfcache; waiting for a persisted pageshow rebind"
      }));
    }
    return { event: message.event, classified: ownedJobs.length };
  }

  let rebound = 0;
  for (const job of ownedJobs) {
    if (job.status === "selecting_model") {
      if (!job.content_script_suspended_at) {
        continue;
      }
      const attempt = Number(job.model_selection_attempt ?? 0) + 1;
      job.model_selection_attempt = attempt;
      openSuspensionGate(job);
      job.content_script_suspended_at = null;
      job.updated_at = Date.now();
      await persistJob(job);
      postNative(progress(job, "model_selection_restarting", {
        tab_id: tabId,
        persisted: true,
        attempt,
        message: "owned background tab returned from bfcache; restarting model selection from a closed picker"
      }));
      try {
        await completeModelSelection(job, tabId, attempt, { reset: true });
      } catch (error) {
        await handlePollerError(job, error);
      }
      rebound += 1;
      continue;
    }
    if (job.status !== "waiting_response") {
      continue;
    }
    if (!job.content_script_suspended_at && contentScriptRecoveries.get(job.job_id)?.settled !== "pending") {
      continue;
    }
    try {
      openSuspensionGate(job);
      await recoverContentScriptJob(job, new Error("owned tab restored from bfcache"), {
        source: "pageshow",
        restoredFromBfcache: true
      });
      rebound += 1;
    } catch (error) {
      const inspectCommand = inspectCommandForJob(job);
      await failJob(
        job,
        "content_script_reconnect_failed",
        `${adapterForJob(job).displayName} background tab returned from bfcache, but Yoetz could not reconnect its content script. The prompt was already submitted and the owned tab is left open. Inspect it before rerunning with: ${inspectCommand}. Do not rerun automatically.`,
        {
          phase: "wait_response",
          side_effect_started: true,
          send_committed: true,
          persisted: true,
          reconnect_reason: String(error?.message ?? error),
          inspect_command: inspectCommand
        }
      );
    }
  }
  return { event: message.event, classified: ownedJobs.length, rebound };
}

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === HEARTBEAT_ALARM) {
    if (nativePort) {
      postNative(makeEnvelope("heartbeat", { payload: { status: "alive" } }));
      void retryPendingTerminalJobs();
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
    const port = chrome.runtime.connectNative(NATIVE_HOST);
    connectionGeneration += 1;
    const generation = connectionGeneration;
    nativePort = port;
    // A disconnected native port can still deliver queued callbacks after a
    // replacement port is installed. Fence every callback by both identity and
    // generation so an old connection cannot mutate or tear down the new one.
    port.onMessage.addListener((message) => {
      if (nativePort !== port || connectionGeneration !== generation) {
        return;
      }
      void handleNativeMessage(message, port, generation);
    });
    port.onDisconnect.addListener(() => {
      if (nativePort !== port || connectionGeneration !== generation) {
        return;
      }
      handleNativeDisconnect(port, generation);
    });
    const restorePromise = reconcileAcknowledgedTerminalTombstones()
      .then(() => restoreJobsFromStorage({ emitLostState: true }))
      .then(() => retryPendingTerminalJobs());
    nativeRestorePromises.set(generation, restorePromise);
    void restorePromise.then(
      () => {
        if (nativeRestorePromises.get(generation) === restorePromise) {
          nativeRestorePromises.delete(generation);
        }
      },
      (error) => {
        setStatus("restore_failed", String(error?.message ?? error));
      }
    );
    setStatus("connected");
    postHello();
    startHeartbeat();
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

async function handleNativeMessage(message, sourcePort = nativePort, sourceGeneration = connectionGeneration) {
  if (nativePort !== sourcePort || connectionGeneration !== sourceGeneration) {
    return;
  }
  const restorePromise = nativeRestorePromises.get(sourceGeneration);
  if (restorePromise) {
    try {
      await restorePromise;
    } catch {
      // Do not synthesize a terminal for a target that may exist only in the
      // durable store. The next native connection retries restoration.
      return;
    }
    if (nativePort !== sourcePort || connectionGeneration !== sourceGeneration) {
      return;
    }
  }
  const validation = validateEnvelope(message);
  if (!validation.ok) {
    const delivered = await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), validation.code, validation.message, {
        request_id: message?.request_id,
        phase: "profile",
        side_effect_started: false
      }),
      { status: "failed", phase: "profile" }
    );
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
      case "terminal_ack":
        await handleTerminalAck(message);
        break;
      default:
        await postTerminalMessage(
          message,
          errorEnvelope(message, "unsupported_type", `unsupported service-worker message ${message.type}`),
          { status: "failed", phase: "profile" }
        );
    }
  } catch (error) {
    if (["capability_mismatch", "run_mismatch"].includes(error?.code)) {
      await postTerminalMessage(
        message,
        errorEnvelope(messageJob(message), error.code, String(error?.message ?? error), {
          request_id: message?.request_id,
          phase: error?.phase ?? "profile",
          side_effect_started: false
        }),
        { status: "failed", phase: error?.phase ?? "profile" }
      );
      return;
    }
    const job = message?.job_id ? jobs.get(message.job_id) : null;
    if (job) {
      await handlePollerError(job, error);
      return;
    }
    if (!terminalJobIds.has(message?.job_id)) {
      await postTerminalMessage(
        message,
        errorEnvelope(message, "extension_error", String(error?.message ?? error), {
          request_id: message?.request_id,
          phase: phaseForStatus(job?.status) ?? "profile",
          side_effect_started: Boolean(job?.tab_id)
        }),
        { status: "failed", phase: phaseForStatus(job?.status) ?? "profile" }
      );
    }
  }
}

async function startJob(message) {
  cleanupTerminalJobIds();
  const existing = jobs.get(message.job_id);
  if (existing) {
    assertMessageOwnsJob(message, existing);
  }
  if (existing?.terminal_envelope && !existing.terminal_delivered_at) {
    // The retained terminal owns this job_id. Replay it instead of creating a
    // second terminal sequence that could ACK and clear the original job.
    await postTerminalJob(existing, existing.terminal_envelope, {
      status: existing.status,
      phase: phaseForStatus(existing.status) ?? "profile"
    });
    return;
  }
  if (existing) {
    // A live job owns this route. The native host rejects duplicate local
    // clients before forwarding them; this notice is only an in-extension
    // fallback and must never look terminal to the owner.
    postNative(progress(existing, "duplicate_job", {
      request_id: message.request_id,
      code: "duplicate_job",
      message: `job ${message.job_id} is already known to this extension instance`
    }));
    return;
  }
  if (terminalJobIds.has(message.job_id)) {
    await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), "duplicate_job", `job ${message.job_id} is already known to this extension instance`, {
        request_id: message.request_id,
        phase: "profile",
        side_effect_started: false
      }),
      { status: "failed", phase: "profile" }
    );
    return;
  }
  const oversizedSelector = ["profile_email", "extension_instance_id", "extension_profile_id"]
    .find((field) => typeof message.payload?.[field] === "string"
      && message.payload[field].length > MAX_TERMINAL_ID_CHARS);
  if (oversizedSelector) {
    await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), "selector_too_long", "profile selector exceeds the maximum supported length", {
        request_id: message.request_id,
        phase: "profile",
        side_effect_started: false,
        field: oversizedSelector,
        max_length: MAX_TERMINAL_ID_CHARS
      }),
      { status: "failed", phase: "profile" }
    );
    return;
  }
  let adapter;
  try {
    adapter = siteAdapterForRecipe(message.payload?.recipe);
  } catch (error) {
    await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), error?.code ?? "unsupported_recipe", String(error?.message ?? error), {
        request_id: message.request_id,
        phase: "profile",
        side_effect_started: false
      }),
      { status: "failed", phase: "profile" }
    );
    return;
  }
  const job = normalizeJob(message, adapter);
  job.started_at = Date.now();
  job.updated_at = Date.now();
  job.connection_generation = connectionGeneration;
  jobs.set(job.job_id, job);
  const continuationEpoch = Number(job.continuation_epoch ?? 0);

  if (job.conversation_error) {
    await failJob(job, "invalid_conversation", job.conversation_error.message, {
      phase: "upload",
      side_effect_started: false
    });
    return;
  }

  const targetProfile = await validateTargetProfile(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  if (!targetProfile.ok) {
    await failJob(job, targetProfile.code, targetProfile.message, targetProfile.detail);
    return;
  }

  job.expected_conversation_id = job.conversation_id ?? null;
  job.status = "opening_tab";
  await persistJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }

  const url = job.expected_conversation_id
    ? adapter.conversationJobUrl(job.expected_conversation_id, job.run_id)
    : adapter.jobUrl(job.run_id);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const tab = await createJobTab(url, adapter);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    await discardCreatedJobTab(tab);
    return;
  }
  job.tab_id = tab.id;
  job.updated_at = Date.now();
  await persistJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const inspectCommand = inspectCommandForJob(job);
  if (!postNative(progress(job, "tab_opened", {
    tab_id: tab.id,
    url,
    inspect_command: inspectCommand,
    message: `opened yoetz-owned ${adapter.displayName} tab ${url}; inspect with: ${inspectCommand}`
  }))) {
    await recordTerminalDeliveryLost(job, "upload");
    return;
  }

  await waitForSiteTab(tab.id, adapter);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const contentScriptProbe = await waitForContentScript(tab.id, adapter);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  recordContentScriptContract(job, contentScriptProbe);
  const prepared = await sendToTab(tab.id, { type: "yoetz_prepare_job", job });
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
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
  job.status = "selecting_model";
  job.model_selection_attempt = Number(job.model_selection_attempt ?? 0) + 1;
  job.updated_at = Date.now();
  await persistJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  await completeModelSelection(job, tab.id, job.model_selection_attempt, {}, continuationEpoch);
}

async function completeModelSelection(job, tabId, attempt, options = {}, continuationEpoch = job?.continuation_epoch) {
  const adapter = adapterForJob(job);
  if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
    return;
  }
  let modelSelection;
  try {
    if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
      return;
    }
    modelSelection = await sendToTab(tabId, {
      type: "yoetz_configure_model",
      job,
      reset: options.reset === true
    });
  } catch (error) {
    if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
      return;
    }
    if (
      options.selectionRecoveryRetried
      || !isRecoverableContentScriptError(error)
    ) {
      throw error;
    }
    try {
      if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
        return;
      }
      await recoverContentScriptJob(job, error, { source: "model_selection" });
    } catch (recoveryError) {
      if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
        return;
      }
      throw recoveryError;
    }
    forgetSettledSuccessfulRecovery(job.job_id);
    if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
      return;
    }
    const retryAttempt = Number(job.model_selection_attempt ?? 0) + 1;
    job.model_selection_attempt = retryAttempt;
    job.updated_at = Date.now();
    await persistJob(job);
    if (staleSelectionAttempt(job, retryAttempt, continuationEpoch)) {
      return;
    }
    return completeModelSelection(job, tabId, retryAttempt, {
      ...options,
      reset: true,
      selectionRecoveryRetried: true
    }, continuationEpoch);
  }
  if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
    return;
  }
  if (adapter.recipe === "chatgpt") {
    job.surface_evidence_seen = Boolean(
      job.surface_evidence_seen || modelSelection.surface_evidence_seen
    );
  }
  job.model_used = modelSelection.model_used ?? null;
  job.model_selection_status = modelSelection.status ?? "unavailable";
  job.warnings = [
    ...(Array.isArray(modelSelection.warnings) ? modelSelection.warnings : []),
    ...(modelSelection.warning ? [modelSelection.warning] : [])
  ];
  if (!postNative(progress(job, "model_selection", modelSelection))) {
    if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
      return;
    }
    await recordTerminalDeliveryLost(job, "model_selection");
    return;
  }
  if (!adapter.isAcceptableModelSelection(modelSelection)) {
    const diagnostics = modelSelectionFailureDiagnostics(modelSelection);
    const diagnosticSummary = formatModelSelectionFailureDiagnostics(diagnostics);
    await failJob(job, "model_selection_failed", [
      `Requested ${adapter.displayName} model was not selected: ${modelSelection.status ?? "unknown"}`,
      modelSelection.failure_reason ? `reason: ${modelSelection.failure_reason}` : null,
      diagnosticSummary ? `diagnostics: ${diagnosticSummary}` : null
    ].filter(Boolean).join(". "), {
      phase: "model_selection",
      side_effect_started: false,
      requested_model: job.model,
      model_strategy: job.model_strategy,
      model_used: job.model_used,
      model_selection_status: job.model_selection_status,
      failure_reason: modelSelection.failure_reason ?? null,
      model_selection: modelSelection,
      ...(Object.keys(diagnostics).length > 0 ? { model_selection_diagnostics: diagnostics } : {})
    });
    return;
  }

  await maybeGroupTab(tabId, job);
  if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
    return;
  }
  job.status = "waiting_for_file";
  job.updated_at = Date.now();
  await persistJob(job);
  if (staleSelectionAttempt(job, attempt, continuationEpoch)) {
    return;
  }
  if (!postNative(progress(job, "ready_for_file", { tab_id: tabId, message: `${adapter.displayName} tab is ready for bundle upload` }))) {
    await recordTerminalDeliveryLost(job, "upload");
  }
}

function staleSelectionAttempt(job, attempt, continuationEpoch = job?.continuation_epoch) {
  return Number(job.model_selection_attempt) !== attempt
    || !jobContinuationIsLive(job, continuationEpoch);
}

function modelSelectionFailureDiagnostics(selection) {
  const diagnostics = {};
  for (const key of [
    "failure_reason",
    "hydration_signal",
    "picker_shape",
    "surface_trust",
    "surface_descendants",
    "advanced_rows",
    "checkbox_probe",
    "family_menu_probe",
    "effort_control",
    "effort_move_method",
    "picker_close_method",
    "picker_close_verification",
    "post_close_failure_reason",
    "post_close_disabled_reason",
    "post_close_family_status",
    "post_close_effort_status",
    "post_close_picker_shape",
    "post_close_closed_pill_text",
    "family_status",
    "effort_status",
    "picker_family_status",
    "picker_effort_status",
    "closed_pill_family_status",
    "closed_pill_effort_status",
    "closed_pill_text",
    "pill_text",
    "family_label",
    "surface_elapsed_ms",
    "surface_attempts",
    "surface_verification_attempts",
    "surface_state",
    "surface_observed_values",
    "modelVerified",
    "maxVerified",
    "modelChip"
  ]) {
    if (Object.prototype.hasOwnProperty.call(selection ?? {}, key)) {
      diagnostics[key] = selection[key];
    }
  }
  if (Array.isArray(selection?.options)) {
    diagnostics.options = selection.options.slice(0, 50);
  }
  return diagnostics;
}

function formatModelSelectionFailureDiagnostics(diagnostics) {
  return Object.entries(diagnostics)
    .map(([key, value]) => `${key}=${JSON.stringify(value)}`)
    .join(", ");
}

async function acceptFileChunk(message) {
  const job = requireJob(message.job_id);
  assertMessageOwnsJob(message, job);
  assertJobConnectionCurrent(job);
  const continuationEpoch = Number(job.continuation_epoch ?? 0);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
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
      if (!jobContinuationIsLive(job, continuationEpoch)) {
        return;
      }
    } else {
      job.status = "receiving_file";
    }
    return;
  }

  const file = chunks.takeFile(job.job_id);
  if (job.recipe === "claude") {
    job.attachment_trace = { final_chunk_ack_at_ms: Date.now() };
  }
  job.status = "file_received";
  job.updated_at = Date.now();
  await persistJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  assertJobConnectionCurrent(job);
  await runJobWithFile(job, file, continuationEpoch);
}

async function runJobWithFile(job, file, continuationEpoch = job?.continuation_epoch) {
  if (!jobContinuationIsLive(job, continuationEpoch)) return;
  assertJobConnectionCurrent(job);
  const adapter = adapterForJob(job);
  const uploadProbe = await requireContentScriptCapability(job.tab_id, adapter, {
    phase: "upload",
    side_effect_started: false
  });
  if (!jobContinuationIsLive(job, continuationEpoch)) return;
  recordContentScriptContract(job, uploadProbe);
  job.status = "uploading_file";
  job.updated_at = Date.now();
  await persistJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) return;
  const uploadResult = await sendToTab(job.tab_id, {
    type: "yoetz_upload_file",
    job,
    file: {
      filename: file.filename,
      mime_type: file.mimeType,
      bytes_base64: uint8ArrayToBase64(file.bytes)
    }
  });
  if (!jobContinuationIsLive(job, continuationEpoch)) return;
  assertJobConnectionCurrent(job);
  if (!postNative(progress(job, "file_uploaded", {
    filename: file.filename,
    bytes: file.bytes.byteLength,
    ...(uploadResult?.upload_commit_signal
      ? { upload_commit_signal: uploadResult.upload_commit_signal }
      : {}),
    message: `bundle uploaded (${file.bytes.byteLength} bytes); sending prompt`
  }))) {
    await recordTerminalDeliveryLost(job, "upload");
    return;
  }

  const prompt = job.prompt ?? "";
  if (prompt) {
    job.response_baseline = await sendToTab(job.tab_id, {
      type: "yoetz_extract_response",
      job,
      blocking_context: "pre_send_baseline"
    });
    if (!jobContinuationIsLive(job, continuationEpoch)) return;
    assertJobConnectionCurrent(job);
    job.status = "sending_prompt";
    job.updated_at = Date.now();
    await persistJob(job);
    if (!jobContinuationIsLive(job, continuationEpoch)) return;
    const sendProbe = await requireContentScriptCapability(job.tab_id, adapter, {
      phase: "send",
      side_effect_started: true,
      send_committed: false
    });
    if (!jobContinuationIsLive(job, continuationEpoch)) return;
    recordContentScriptContract(job, sendProbe);
    if (!jobContinuationIsLive(job, continuationEpoch)) return;
    const sendResult = await sendToTab(job.tab_id, { type: "yoetz_send_prompt", job, prompt });
    if (!jobContinuationIsLive(job, continuationEpoch)) {
      return;
    }
    if (sendResult?.sent === true) {
      job.send_committed = true;
      job.updated_at = Date.now();
      await persistJob(job);
      if (!jobContinuationIsLive(job, continuationEpoch)) return;
    }
    if (job.recipe === "chatgpt") {
      const finalModelSelection = sendResult?.final_model_selection;
      const expectedModelSelectionStatus = job.model_strategy === "current" ? "current" : "selected";
      const expectedRequestedModel = job.model_strategy === "current" ? "current" : job.model;
      const receiptMissing = !finalModelSelection
        || typeof finalModelSelection !== "object"
        || Array.isArray(finalModelSelection);
      const receiptError = receiptMissing
        ? null
        : validateChatgptFinalModelSelectionReceipt(finalModelSelection, job);
      if (receiptMissing || receiptError) {
        if (!jobContinuationIsLive(job, continuationEpoch)) return;
        await failJob(
          job,
          receiptMissing ? "send_proof_missing" : "send_proof_invalid",
          `ChatGPT send returned without a valid click-bound final model selection receipt for strategy ${job.model_strategy ?? "select"} and model ${expectedRequestedModel}${receiptError ? `: ${receiptError}` : ""}. The prompt may already be submitted; inspect the owned tab before rerunning.`,
          {
            phase: "post_completion",
            side_effect_started: true,
            send_committed: true,
            model_strategy: job.model_strategy,
            final_model_selection: finalModelSelection ?? null
          }
        );
        return;
      }
      job.final_model_selection = finalModelSelection;
      job.surface_evidence_seen = Boolean(
        job.surface_evidence_seen || finalModelSelection.surface_evidence_seen === true
      );
      job.model_used = finalModelSelection.model_used ?? job.model_used;
      job.model_selection_status = finalModelSelection.status ?? job.model_selection_status;
    }
    job.submitted_url = sendResult?.url ?? null;
    job.submitted_conversation_id = sendResult?.conversation_id ?? null;
    job.submitted_user_count = Number.isFinite(Number(sendResult?.submitted_user_count))
      ? Number(sendResult.submitted_user_count)
      : null;
    job.submitted_assistant_count = Number.isFinite(Number(sendResult?.submitted_assistant_count))
      ? Number(sendResult.submitted_assistant_count)
      : null;
    assertSubmittedConversationCurrent(job, sendResult);
    assertJobConnectionCurrent(job);
    if (!jobContinuationIsLive(job, continuationEpoch)) return;
    const inspectCommand = inspectCommandForJob(job);
    if (!postNative(progress(job, "prompt_sent", {
      timeout_ms: responseWaitTimeoutMs(job),
      inspect_command: inspectCommand,
      yoetz_url: adapterForJob(job).jobUrl(job.run_id),
      submitted_url: job.submitted_url,
      conversation_id: conversationIdForJob(job),
      conversation_url: conversationUrlForJob(job, conversationIdForJob(job)),
      message: `prompt sent; waiting for ${adapterForJob(job).displayName} response (timeout ${formatDurationForMessage(responseWaitTimeoutMs(job))}); inspect with: ${inspectCommand}`
    }))) {
      if (!jobContinuationIsLive(job, continuationEpoch)) return;
      await recordTerminalDeliveryLost(job, "send");
      return;
    }
  } else {
    if (!jobContinuationIsLive(job, continuationEpoch)) return;
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
  if (!jobContinuationIsLive(job, continuationEpoch)) return;
  const extraction = await waitForResponse(job, continuationEpoch);
  if (!jobContinuationIsLive(job, continuationEpoch)) return;
  assertJobConnectionCurrent(job);
  if (!extraction) return;
  await completeJobWithExtraction(job, extraction, continuationEpoch);
}

async function completeJobWithExtraction(job, extraction, continuationEpoch = job?.continuation_epoch) {
  if (await cancellationFence(job) || !jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const conversationId = conversationIdForJob(job, extraction);
  const adapter = adapterForJob(job);
  const finalityAnchor = adapter.recipe === "chatgpt"
    ? (extraction.method === "backend_api" ? "backend_api" : "dom_only")
    : null;
  const completeEnvelope = makeEnvelope("job_complete", {
    job_id: job.job_id,
    run_id: job.run_id,
    workspace_id: job.workspace_id,
    capability_token: job.capability_token,
    payload: {
      tab_id: job.tab_id,
      // The authoritative answer. This is the ONLY surface that carries is_final=true; every
      // job_progress event is is_final=false. A consumer must treat job_complete.response as the
      // response and never a progress event's interim/partial text.
      is_final: true,
      response: extraction.text,
      extraction_method: extraction.method,
      completion_reason: extraction.completion_reason,
      finality_anchor: finalityAnchor,
      stable_for_ms: extraction.stable_for_ms,
      assistant_turn_count: extraction.assistant_turn_count ?? extraction.assistant_count ?? 0,
      copy_button_count: extraction.copy_button_count ?? 0,
      conversation_id: conversationId,
      conversation_url: conversationUrlForJob(job, conversationId),
      model_strategy: job.model_strategy ?? "select",
      // The picker-proven label (e.g. "GPT-5.6 Sol Pro") is the authoritative
      // model identity yoetz already verified before send. The backend
      // data-message-model-slug (e.g. "gpt-5.6-sol-wm") drifts with ChatGPT's
      // internal naming, so it must not overwrite a proven label. Keep it as
      // observability (model_slug) and as the fallback when there was no
      // picker proof (current/kept_current/unavailable).
      model_used: ["selected", "current"].includes(job.model_selection_status) && job.model_used
        ? job.model_used
        : (extraction.model_slug ?? job.model_used ?? null),
      model_slug: extraction.model_slug ?? null,
      model_selection_status: job.model_selection_status ?? "unavailable",
      final_model_selection: job.final_model_selection ?? null,
      warnings: completionWarnings({
        jobWarnings: job.warnings,
        extraction,
        emptyResponseWarning: adapter.completion.emptyResponseWarning,
        extractionWarnings: adapter.completion.extractionWarnings?.(extraction),
        finalityAnchor,
        domOnlyFinalityWarning: CHATGPT_DOM_ONLY_FINALITY_WARNING
      })
    }
  });
  if (await cancellationFence(job) || !jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  stampTerminalSequence(job, completeEnvelope);
  const completeBytes = nativeEnvelopeByteLength(completeEnvelope);
  if (completeBytes > MAX_NATIVE_OUTBOUND_BYTES) {
    const adapter = adapterForJob(job);
    const inspectCommand = inspectCommandForJob(job);
    await failJob(
      job,
      "response_too_large",
      `${adapter.displayName} response is too large to deliver through chrome-extension-native (${completeBytes} bytes > ${MAX_NATIVE_OUTBOUND_BYTES}); inspect the owned tab with: ${inspectCommand}`,
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
  if (completeBytes > MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES) {
    const inspectCommand = inspectCommandForJob(job);
    await failJob(
      job,
      "response_too_large",
      `${adapter.displayName} response is too large to retain safely for chrome-extension-native replay (${completeBytes} bytes > ${MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES} bytes); inspect the owned tab with: ${inspectCommand}`,
      {
        phase: "wait_response",
        side_effect_started: true,
        completion_reason: extraction.completion_reason,
        extraction_method: extraction.method,
        response_length: extraction.text?.length ?? 0,
        native_message_bytes: completeBytes,
        max_persisted_terminal_envelope_bytes: MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES,
        inspect_command: inspectCommand
      }
    );
    return;
  }
  job.status = "complete";
  forgetContentScriptRecovery(job.job_id);
  rememberTerminalJob(job.job_id);
  await postTerminalJob(job, completeEnvelope, { status: "complete", phase: "wait_response" });
}

async function closeOwnedTabOnComplete(job) {
  if (!job.close_tab_on_complete || !job.tab_id || job.tab_disposition) {
    return;
  }
  const ownership = await verifyTabOwnership(job);
  if (!ownership.owned) {
    job.tab_disposition = "kept_ownership_unverified";
    job.tab_ownership_verified = false;
    job.tab_ownership_error = ownership.error ?? ownership.reason;
    job.updated_at = Date.now();
    await persistTerminalJobBestEffort(job);
    postNative(progress(job, "tab_close_skipped", {
      tab_id: job.tab_id,
      tab_disposition: job.tab_disposition,
      ownership_verified: false,
      ownership_error: job.tab_ownership_error,
      message: "kept the tab because durable job ownership could not be verified"
    }));
    return;
  }
  job.tab_ownership_verified = true;
  let phase = "tab_closed";
  let detail = { tab_id: job.tab_id, ownership_verified: true };
  try {
    await chrome.tabs.remove(job.tab_id);
    job.tab_disposition = "closed";
  } catch (error) {
    job.tab_disposition = "close_failed";
    phase = "tab_close_failed";
    detail = {
      tab_id: job.tab_id,
      ownership_verified: true,
      error: String(error?.message ?? error)
    };
  }
  await persistTerminalJobBestEffort(job);
  // The native host currently releases the per-job client after job_complete,
  // so this progress event is intentionally unrouted until host routing grows
  // an explicit post-terminal channel. The persisted shard is authoritative.
  postNative(progress(job, phase, detail));
}

function conversationIdForJob(job, extraction = null) {
  return job?.submitted_conversation_id ?? extraction?.conversation_id ?? job?.conversation_id ?? null;
}

function expectedConversationIdForJob(job) {
  return String(
    job?.expected_conversation_id
      ?? job?.submitted_conversation_id
      ?? job?.conversation_id
      ?? ""
  ).trim() || null;
}

function conversationUrlForJob(job, conversationId) {
  return adapterForJob(job).conversationUrl(conversationId);
}

async function resumeWaitingResponseJob(job) {
  try {
    const adapter = adapterForJob(job);
    await waitForSiteTab(job.tab_id, adapter);
    if (job.content_script_suspended_at) {
      await recoverContentScriptJob(job, new Error("owned tab is parked in bfcache after worker restore"), {
        source: "worker_restore"
      });
    } else {
      const contentScriptProbe = await waitForContentScript(job.tab_id, adapter, {
        phase: "wait_response",
        side_effect_started: true,
        send_committed: true
      });
      recordContentScriptContract(job, contentScriptProbe);
      const rebound = await sendToTab(job.tab_id, { type: "yoetz_bind_job", job });
      await persistJob(job);
      postNative(progress(job, "content_script_recovered", {
        restored: true,
        url: rebound?.url ?? null,
        title: rebound?.title ?? null
      }));
    }
  } catch (error) {
    await handlePollerError(job, error);
    return;
  }
  await continueWaitingResponseJob(job);
}

async function continueWaitingResponseJob(job) {
  const continuationEpoch = Number(job.continuation_epoch ?? 0);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const lease = acquirePollerLease(job);
  if (lease == null) {
    return;
  }
  try {
    const extraction = await waitForResponse(job, continuationEpoch);
    if (!jobContinuationIsLive(job, continuationEpoch)) {
      return;
    }
    if (!holdsPollerLease(job, lease)) {
      return;
    }
    assertJobConnectionCurrent(job);
    if (!extraction) return;
    await completeJobWithExtraction(job, extraction, continuationEpoch);
  } catch (error) {
    if (!holdsPollerLease(job, lease)) {
      return;
    }
    releasePollerLease(job, lease);
    await handlePollerError(job, error);
  } finally {
    releasePollerLease(job, lease);
  }
}

function validateChatgptFinalModelSelectionReceipt(receipt, job) {
  const strategy = job?.model_strategy === "current" ? "current" : "select";
  const expectedStatus = strategy === "current" ? "current" : "selected";
  const expectedModel = strategy === "current" ? "current" : job?.model;
  if (receipt.click_bound !== true) return "click_bound is not true";
  if (receipt.status !== expectedStatus) return `status=${JSON.stringify(receipt.status)}`;
  if (receipt.requested_model !== expectedModel) return `requested_model=${JSON.stringify(receipt.requested_model)}`;
  if (strategy === "select") {
    for (const [key, expected] of [
      ["model_used", "GPT-5.6 Sol Pro"],
      ["family_status", "verified"],
      ["effort_status", "verified"],
      ["picker_family_status", "verified"],
      ["picker_effort_status", "verified"],
      ["click_bound_closed_pill_effort_status", "verified"]
    ]) {
      if (receipt[key] !== expected) return `${key}=${JSON.stringify(receipt[key])}`;
    }
    // ChatGPT can render only the selected effort tier in the closed pill.
    // The picker proof still binds the family when that observation is skipped.
    const clickBoundFamilyProof = receipt.click_bound_closed_pill_family_status === "verified"
      || (receipt.click_bound_closed_pill_family_status === "skipped"
        && receipt.family_status === "verified"
        && receipt.picker_family_status === "verified"
        && receipt.closed_pill_family_status !== "unverified"
        && receipt.post_close_family_status !== "unverified");
    if (!clickBoundFamilyProof) {
      return `click_bound_closed_pill_family_status=${JSON.stringify(receipt.click_bound_closed_pill_family_status)}`;
    }
    if (!new Set(["menu", "slider", "personal"]).has(receipt.picker_shape)) {
      return `picker_shape=${JSON.stringify(receipt.picker_shape)}`;
    }
    const close = receipt.picker_close_verification;
    if (!close || close.picker_surface_closed !== true
      || close.model_trigger_closed !== true
      || close.family_trigger_closed !== true
      || close.closed_pill_pro !== true) {
      return "picker_close_verification is incomplete";
    }
  } else {
    if (typeof receipt.model_used !== "string" || !receipt.model_used.trim()) {
      return "Current model_used is empty";
    }
    if (receipt.family_status !== "skipped" || receipt.effort_status !== "skipped") {
      return "Current family/effort proof is not skipped";
    }
    if (typeof receipt.click_bound_closed_pill_text !== "string"
        || !receipt.click_bound_closed_pill_text.trim()
        || receipt.model_used.replace(/\s+/g, " ").trim()
          !== receipt.click_bound_closed_pill_text.replace(/\s+/g, " ").trim()) {
      return "Current model_used does not match the click-time closed pill";
    }
  }
  if (typeof receipt.click_bound_closed_pill_text !== "string"
      || !receipt.click_bound_closed_pill_text.trim()) {
    return "click-time closed pill text is empty";
  }
  const observed = Array.isArray(receipt.surface_observed_values)
    ? receipt.surface_observed_values
    : [];
  const chatState = receipt.surface_chat_state;
  const workState = receipt.surface_work_state;
  if (receipt.surface_proof_kind === "explicit_chat_work_radios") {
    if (receipt.surface_evidence_seen !== true
        || receipt.surface_visible_toggle_count !== 2
        || chatState?.aria_checked !== "true"
        || workState?.aria_checked !== "false"
        || !observed.includes("chatgpt")
        || !observed.includes("work")) {
      return "explicit Chat/Work surface proof is incomplete";
    }
    if (receipt.surface_composer_aria !== null) {
      return "explicit surface proof must not claim implicit composer proof";
    }
  } else if (receipt.surface_proof_kind === "implicit_chat_composer_aria") {
    if (receipt.surface_evidence_seen !== false
        || receipt.surface_visible_toggle_count !== 0
        || observed.length !== 0
        || receipt.surface_composer_aria !== "Chat with ChatGPT"
        || chatState !== null
        || workState !== null) {
      return "implicit Chat composer proof is incomplete";
    }
  } else {
    return `surface_proof_kind=${JSON.stringify(receipt.surface_proof_kind)}`;
  }
  return null;
}

function acquirePollerLease(job) {
  if (job.poller_lease != null) {
    return null;
  }
  const lease = Number(job.poller_lease_seq ?? 0) + 1;
  job.poller_lease_seq = lease;
  job.poller_lease = lease;
  return lease;
}

function holdsPollerLease(job, lease) {
  return job.poller_lease === lease;
}

function releasePollerLease(job, lease) {
  if (job.poller_lease === lease) {
    job.poller_lease = null;
  }
}

function jobCanResumePolling(job) {
  return Boolean(
    job
    && jobs.has(job.job_id)
    && !TERMINAL_STATUSES.has(job.status)
    && job.status === "waiting_response"
    && !cancellationIsPending(job)
  );
}

async function handlePollerError(job, error) {
  const code = error?.code ?? "extension_error";
  if (code === "connection_generation_changed") {
    if (isNativeReconnectResumableState(job)) {
      await pauseForNativeReconnect(job);
    } else if (jobs.has(job.job_id) && !TERMINAL_STATUSES.has(job.status)) {
      const phase = phaseForStatus(job.status) ?? "upload";
      await failJob(
        job,
        "bridge_interrupted",
        adapterForJob(job).displayName
          + " job was interrupted by a native bridge restart during "
          + phase
          + "; the provider operation was not retried automatically. Inspect the owned tab before rerunning.",
        {
          phase,
          side_effect_started: Boolean(job.tab_id || job.send_committed),
          send_committed: Boolean(job.send_committed),
          reconnect_reason: String(error?.message ?? error),
          inspect_command: job.run_id ? inspectCommandForJob(job) : undefined
        }
      );
    }
    return;
  }
  const recovery = contentScriptRecoveries.get(job.job_id);
  if (recovery) {
    try {
      await recovery;
    } catch {
      return;
    }
    if (!jobCanResumePolling(job)) {
      return;
    }
    await continueWaitingResponseJob(job);
    return;
  }
  if (!jobs.has(job.job_id) || TERMINAL_STATUSES.has(job.status)) {
    return;
  }
  const detail = errorContextForJob(job, error);
  await failJob(job, code, jobErrorMessage(job, error, code, detail), detail);
}

async function pauseForNativeReconnect(job) {
  if (!isNativeReconnectResumableState(job)) {
    return;
  }
  job.native_reconnect_pending = true;
  job.native_disconnected_at = job.native_disconnected_at ?? Date.now();
  job.updated_at = Date.now();
  await persistJobBestEffort(job);
  scheduleNativeReconnectResume(job);
}

function isNativeReconnectResumableState(job) {
  return Boolean(
    job
    && jobs.has(job.job_id)
    && !TERMINAL_STATUSES.has(job.status)
    && ["waiting_for_file", "waiting_response"].includes(job.status)
    && !cancellationIsPending(job)
  );
}

function isTerminalPendingJob(job) {
  return Boolean(
    job
    && TERMINAL_STATUSES.has(job.status)
    && !job.terminal_delivered_at
    && (
      job.terminal_envelope
      || job.terminal_persistence_failed === true
      || job.status === "terminal_delivery_lost"
      || job.delivery_lost_phase
    )
  );
}

function isReconnectRestorableJob(job) {
  return isNativeReconnectResumableState(job) || isTerminalPendingJob(job);
}

function scheduleNativeReconnectResume(job) {
  if (!jobCanResumePolling(job)
      || job.native_reconnect_pending !== true
      || job.connection_generation !== connectionGeneration
      || !nativePort
      || job.poller_lease != null) {
    return;
  }
  job.native_reconnect_pending = false;
  job.native_disconnected_at = null;
  void persistJobBestEffort(job);
  void continueWaitingResponseJob(job);
}

async function cancelJob(message) {
  const job = requireJob(message.job_id);
  assertMessageOwnsJob(message, job);
  // Once the cancellation intent is durable, a reconnect or worker restore
  // must be allowed to finish it even though the request's connection is old.
  if (!job.cancel_pending) {
    assertJobConnectionCurrent(job);
  }
  if (!job.cancel_pending && !TERMINAL_STATUSES.has(job.status)) {
    // Set the in-memory fence before the first await. A poller that is already
    // past its extraction await must not be able to claim a later terminal.
    job.continuation_epoch = Number(job.continuation_epoch ?? 0) + 1;
    job.cancel_pending = {
      request_id: message.request_id,
      phase: "requested",
      requested_at: Date.now()
    };
    job.cancel_pending_durable = false;
    job.cancel_requested = true;
    job.cancelled = true;
    job.updated_at = Date.now();
  }
  return serializeCancellation(job, () => cancelJobInternal(message, job));
}

function serializeCancellation(job, operation) {
  const existing = cancellationOperations.get(job.job_id);
  if (existing) {
    return existing;
  }
  const pending = Promise.resolve().then(operation);
  const tracked = pending.finally(() => {
    if (cancellationOperations.get(job.job_id) === tracked) {
      cancellationOperations.delete(job.job_id);
    }
  });
  cancellationOperations.set(job.job_id, tracked);
  return tracked;
}

function cancellationIsPending(job) {
  return Boolean(job?.cancel_pending || job?.cancelled || job?.cancel_retry_pending);
}

function jobContinuationIsLive(job, continuationEpoch = job?.continuation_epoch) {
  return Boolean(
    job
    && jobs.get(job.job_id) === job
    && Number(job.continuation_epoch ?? 0) === Number(continuationEpoch ?? 0)
    && !cancellationIsPending(job)
    && !TERMINAL_STATUSES.has(job.status)
  );
}

async function cancellationFence(job) {
  if (!cancellationIsPending(job)) {
    return false;
  }
  const operation = cancellationOperations.get(job.job_id);
  if (operation) {
    try {
      await operation;
    } catch {
      // The cancellation operation reports its own durable failure. If it
      // clears the fence, the caller may continue with its original action.
    }
  } else if (job.cancel_pending) {
    await finishPendingCancellation(job);
  }
  return cancellationIsPending(job) || TERMINAL_STATUSES.has(job.status);
}

async function cancelJobInternal(message, job) {
  if (TERMINAL_STATUSES.has(job.status)) {
    if (job.terminal_envelope && !job.terminal_delivered_at) {
      await postTerminalJob(job, job.terminal_envelope, {
        status: job.status,
        phase: phaseForStatus(job.status) ?? "wait_response"
      });
    }
    return;
  }
  if (job.cancel_pending && job.cancel_pending_durable === true) {
    await finishPendingCancellationInternal(job);
    return;
  }
  if (!await persistCancelPending(job)) {
    // Do not tear down the provider tab while the cancellation intent is not
    // durable. The live job remains available for a reconnect retry.
    job.cancel_pending_persistence_failed = true;
    job.cancel_pending = null;
    job.cancel_pending_durable = false;
    job.cancel_retry_pending = true;
    job.cancel_requested = true;
    job.cancelled = false;
    job.updated_at = Date.now();
    await persistJobBestEffort(job);
    postNative(progress(job, "cancel_pending_persistence_failed", {
      code: "cancel_pending_persistence_failed",
      message: "cancellation was not persisted; the owned tab was kept open and cancellation will not be retried automatically until the bridge reconnects"
    }));
    return;
  }
  job.cancel_pending_durable = true;
  job.cancel_retry_pending = false;
  job.cancel_pending_persistence_failed = false;
  await finishPendingCancellationInternal(job);
}

async function finishPendingCancellation(job) {
  if (!job?.cancel_pending || TERMINAL_STATUSES.has(job.status)) {
    return;
  }
  return serializeCancellation(job, () => finishPendingCancellationInternal(job));
}

async function finishPendingCancellationInternal(job) {
  if (!job?.cancel_pending || TERMINAL_STATUSES.has(job.status)) {
    return;
  }
  forgetContentScriptRecovery(job.job_id);
  job.updated_at = Date.now();

  // Verify the durable window.name marker and provider origin before sending a
  // stop command or removing the tab. A stale job record must never destroy a
  // tab that a later run or the user now owns.
  let ownership = await verifyTabOwnership(job);
  let stopClicked = false;
  // stopConfirmed === false means we could not confirm generation halted (timed
  // out still generating, or the content script was unreachable). The CLI uses
  // it to warn the user the run may still be live server-side. null = unknown
  // (no tab to ask).
  let stopConfirmed = job.tab_id ? false : null;
  if (job.tab_id && ownership.owned) {
    try {
      const stopResult = await sendToTab(job.tab_id, { type: "yoetz_cancel_send", job });
      stopClicked = Boolean(stopResult?.stopped);
      stopConfirmed = Boolean(stopResult?.confirmed_idle);
    } catch {
      // Tab may already be gone / content script unreachable; cancel proceeds.
      // Leave stopConfirmed false so the CLI can warn it may still be running.
    }
  }
  // True when we asked a tab to stop but could not confirm it went idle.
  let cancelMayStillBeRunning = job.tab_id
    ? !ownership.owned || stopConfirmed === false
    : false;

  // Close the tab so generation cannot continue in the background. V1 chooses
  // hard removal over chrome.tabGroups.update({ collapsed: true }) into a
  // "yoetz-cancelled" group — removal is the simpler contract (no group cleanup
  // to manage, no risk of a collapsed-but-still-streaming tab consuming quota).
  // If a future revision wants to preserve the tab for forensics, route that
  // here through the tabGroups API instead of chrome.tabs.remove. This runs only
  // after the awaited cancelSend above resolves, so we never destroy the page
  // mid-abort.
  let tabDisposition = job.tab_id
    ? (ownership.owned ? "close_failed" : "kept_ownership_unverified")
    : "closed";
  if (job.tab_id && ownership.owned && chrome.tabs?.remove) {
    // stopToTab is awaited above, so ownership can change while the provider
    // aborts. Re-probe immediately before removal; a stale positive probe must
    // never authorize destroying a tab that was reused by another run.
    ownership = await verifyTabOwnership(job);
    if (!ownership.owned) {
      cancelMayStillBeRunning = true;
      tabDisposition = "kept_ownership_unverified";
    } else {
      try {
        await chrome.tabs.remove(job.tab_id);
        tabDisposition = "closed";
      } catch {
        // Tab already closed by the user, or removal racing with navigation.
      }
    }
  }
  if (!job.cancel_pending || TERMINAL_STATUSES.has(job.status)) {
    return;
  }

  postNative(progress(job, "cancelled", {
    tab_id: job.tab_id,
    tab_disposition: tabDisposition,
    stop_clicked: stopClicked,
    stop_confirmed: stopConfirmed,
    generation_idle: stopConfirmed,
    may_still_be_running: cancelMayStillBeRunning,
    ownership_verified: ownership.owned,
    ...(ownership.owned
      ? {}
      : { ownership_error: ownership.error ?? ownership.reason })
  }));
  const cancelEnvelope = makeEnvelope("job_cancel", {
    request_id: job.cancel_pending.request_id ?? job.request_id,
    job_id: job.job_id,
    run_id: job.run_id,
    workspace_id: job.workspace_id,
    capability_token: job.capability_token,
    payload: {
      cancelled: true,
      tab_disposition: tabDisposition,
      stop_clicked: stopClicked,
      stop_confirmed: stopConfirmed,
      generation_idle: stopConfirmed,
      may_still_be_running: cancelMayStillBeRunning,
      ownership_verified: ownership.owned,
      ...(ownership.owned
        ? {}
        : { ownership_error: ownership.error ?? ownership.reason })
    }
  });
  job.status = "cancelled";
  job.tab_disposition = tabDisposition;
  job.tab_ownership_verified = ownership.owned;
  if (!ownership.owned) {
    job.tab_ownership_error = ownership.error ?? ownership.reason;
  }
  job.may_still_be_running = cancelMayStillBeRunning;
  job.cancel_pending.phase = "teardown_complete";
  job.terminal_envelope = cancelEnvelope;
  job.terminal_delivered_at = null;
  const posted = await postTerminalJob(job, cancelEnvelope, { status: "cancelled", phase: "cancel" });
  if (posted) {
    await removeCancelPending(job.job_id);
  }
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
  if (message.payload?.intent === "job_reconnect") {
    const targetedJob = message?.job_id ? jobs.get(message.job_id) : null;
    if (!targetedJob) {
      // An explicitly targeted request must never degrade into global recovery
      // when its durable owner is gone or has already been acknowledged.
      postNative(makeEnvelope("reconnect", {
        request_id: message.request_id,
        job_id: message.job_id,
        run_id: message.run_id,
        workspace_id: message.workspace_id,
        payload: {
          restored_jobs: [],
          restored_runs: []
        }
      }));
      return;
    }
    assertMessageOwnsJob(message, targetedJob);
    await handleTargetedReconnect(message, targetedJob);
    return;
  }
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
  if (message.payload?.intent === "doctor_auth_probe") {
    await handleDoctorAuthProbe(message);
    return;
  }
  if (message.payload?.intent === "bridge_check") {
    await postTerminalMessage(message, makeEnvelope("job_complete", {
      request_id: message.request_id,
      job_id: message.job_id,
      run_id: message.run_id,
      workspace_id: message.workspace_id,
      payload: {
        status: "ok"
      }
    }), { status: "complete", phase: "profile" });
    return;
  }
  const waitingResponseJobs = [];
  for (const job of jobs.values()) {
    if (TERMINAL_STATUSES.has(job.status)) {
      continue;
    }
    if (job.cancel_pending || job.cancelled) {
      await finishPendingCancellation(job);
      continue;
    }
    if (job.cancel_retry_pending) {
      continue;
    }
    if (!["waiting_for_file", "waiting_response"].includes(job.status)) {
      await failJob(
        job,
        "bridge_interrupted",
        adapterForJob(job).displayName
          + " job was interrupted by a native bridge restart during "
          + (phaseForStatus(job.status) ?? "upload")
          + "; the provider operation was not retried automatically. Inspect the owned tab before rerunning.",
        {
          phase: phaseForStatus(job.status) ?? "upload",
          side_effect_started: Boolean(job.tab_id || job.send_committed),
          send_committed: Boolean(job.send_committed),
          reconnect_reason: "native bridge reconnect",
          inspect_command: job.run_id ? inspectCommandForJob(job) : undefined
        }
      );
      continue;
    }
    job.connection_generation = connectionGeneration;
    if (job.status === "waiting_response") {
      job.native_reconnect_pending = true;
      waitingResponseJobs.push(job);
    }
    job.updated_at = Date.now();
    await persistJob(job);
  }
  await recoverJobs(message);
  for (const job of waitingResponseJobs) {
    scheduleNativeReconnectResume(job);
  }
}

async function handleTargetedReconnect(message, job) {
  if (job.cancel_pending || job.cancelled) {
    await finishPendingCancellation(job);
    return;
  }
  if (job.cancel_retry_pending) {
    // The prior cancel could not reach durable storage. A targeted reconnect
    // has a live owner route, so retry the intent before exposing the job again.
    job.cancel_pending = {
      request_id: message.request_id ?? job.request_id,
      phase: "requested",
      requested_at: Date.now()
    };
    job.cancel_pending_durable = false;
    job.cancel_retry_pending = false;
    job.cancel_pending_persistence_failed = false;
    job.cancel_requested = true;
    job.cancelled = true;
    job.updated_at = Date.now();
    await serializeCancellation(job, () => cancelJobInternal({
      ...message,
      type: "job_cancel",
      request_id: job.cancel_pending.request_id
    }, job));
    return;
  }
  if (isTerminalPendingJob(job)) {
    await retryPendingTerminalJobs(job.job_id);
  } else if (TERMINAL_STATUSES.has(job.status)) {
    return;
  } else if (!isNativeReconnectResumableState(job)) {
    await failJob(
      job,
      "bridge_interrupted",
      `${adapterForJob(job).displayName} job was interrupted by a targeted native bridge reconnect during ${phaseForStatus(job.status) ?? "upload"}; the provider operation was not retried automatically. Inspect the owned tab before rerunning.`,
      {
        phase: phaseForStatus(job.status) ?? "upload",
        side_effect_started: Boolean(job.tab_id || job.send_committed),
        send_committed: Boolean(job.send_committed),
        reconnect_reason: "targeted native bridge reconnect",
        inspect_command: job.run_id ? inspectCommandForJob(job) : undefined
      }
    );
    return;
  } else {
    job.connection_generation = connectionGeneration;
    if (job.status === "waiting_response") {
      job.native_reconnect_pending = true;
    }
    job.updated_at = Date.now();
    await persistJob(job);
    if (job.status === "waiting_response") {
      scheduleNativeReconnectResume(job);
    } else {
      postNative(progress(job, "ready_for_file", {
        tab_id: job.tab_id,
        restored: true,
        message: `${adapterForJob(job).displayName} tab is ready for bundle upload`
      }));
    }
  }

  const current = jobs.get(job.job_id) ?? job;
  const restorable = isReconnectRestorableJob(current)
    && current.run_id
    && current.workspace_id === message.workspace_id;
  postNative(makeEnvelope("reconnect", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    payload: {
      restored_jobs: restorable ? [current.job_id] : [],
      restored_runs: restorable
        ? [{
          job_id: current.job_id,
          run_id: current.run_id,
          workspace_id: current.workspace_id
        }]
        : []
    }
  }));
}

async function handleDoctorAuthProbe(message) {
  const adapter = siteAdapterForRecipe(message.payload?.recipe);
  await postTerminalMessage(message, makeEnvelope("job_complete", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    payload: await probeSiteAuthentication(adapter)
  }), { status: "complete", phase: "profile" });
}

async function probeSiteAuthentication(adapter) {
  const tabs = await chrome.tabs.query({ url: adapter.tabQueryPattern });
  const ownedTabCounts = await countOwnedTabs(tabs, adapter);
  const selected = selectSiteAuthProbeTab(tabs, adapter);
  if (!selected) {
    return {
      status: adapter.auth.noTabStatus,
      authenticated: false,
      message: `No ${adapter.displayName} tab is open in this Chrome profile; open ${adapter.homeUrl} and rerun doctor`,
      inspected_tabs: 0,
      ...ownedTabCounts
    };
  }
  try {
    const probe = await sendToTab(selected.tab.id, { type: "yoetz_auth_probe", recipe: adapter.recipe });
    return {
      ...probe,
      tab_id: selected.tab.id,
      tab_url: selected.tab.url ?? null,
      tab_title: selected.tab.title ?? null,
      selection: selected.selection,
      inspected_tabs: selected.total,
      ...ownedTabCounts
    };
  } catch (error) {
    return {
      status: "content_script_unavailable",
      authenticated: false,
      message: `Yoetz content script is not ready in selected ${adapter.displayName} tab: ${String(error?.message ?? error)}`,
      tab_id: selected.tab.id,
      tab_url: selected.tab.url ?? null,
      tab_title: selected.tab.title ?? null,
      selection: selected.selection,
      inspected_tabs: selected.total,
      ...ownedTabCounts
    };
  }
}

async function countOwnedTabs(tabs, adapter) {
  const stored = await chrome.storage.session.get(null);
  const storedJobs = Object.entries(stored)
    .filter(([key, job]) =>
      key.startsWith(JOBS_KEY_PREFIX)
      && Number.isFinite(job?.tab_id)
    )
    .map(([, job]) => job);
  const shardedTabIds = new Set(
    storedJobs
      .filter((job) => job.tab_disposition !== "closed")
      .map((job) => job.tab_id)
  );
  const ownedTabIds = new Set(
    (tabs ?? [])
      .filter((tab) =>
        isYoetzOwnedTab(tab, adapter)
        || shardedTabIds.has(tab?.id)
      )
      .map((tab) => tab.id)
  );
  const completeTabIds = new Set(
    storedJobs
      .filter((job) =>
        job.status === "complete"
        && job.close_tab_on_complete === true
        && job.tab_disposition !== "closed"
      )
      .map((job) => job.tab_id)
  );
  return {
    yoetz_owned_tabs_open: ownedTabIds.size,
    yoetz_owned_complete_tabs_open: [...ownedTabIds]
      .filter((tabId) => completeTabIds.has(tabId))
      .length
  };
}

function isYoetzOwnedTab(tab, adapter) {
  return Boolean(tab?.id)
    && adapter.isAllowedTabUrl(tab.url)
    && new URL(tab.url).searchParams.has("_yoetz");
}

function selectSiteAuthProbeTab(tabs, adapter) {
  const candidates = (tabs ?? [])
    .filter((tab) => tab?.id && adapter.isAllowedTabUrl(tab.url))
    .map((tab) => ({
      tab,
      yoetzOwned: isYoetzOwnedTab(tab, adapter)
    }));
  if (candidates.length === 0) {
    return null;
  }
  const candidate =
    candidates.find((item) => item.tab.active && !item.yoetzOwned)
    ?? candidates.find((item) => !item.yoetzOwned)
    ?? candidates.find((item) => item.tab.active)
    ?? candidates[0];
  return {
    tab: candidate.tab,
    selection: adapter.auth.selection(candidate),
    total: candidates.length
  };
}

async function handleInspectRun(message) {
  const adapter = siteAdapterForRecipe(message.payload?.recipe);
  const runId = String(message.payload?.run_id ?? "").trim();
  if (!runId) {
    await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), "missing_run_id", "inspect_run requires payload.run_id", {
        request_id: message.request_id,
        phase: "profile",
        side_effect_started: false
      }),
      { status: "failed", phase: "profile" }
    );
    return;
  }
  // inspect_run normally uses a fresh control job_id. Resolve that request to
  // one durable provider job before reading any tab. A run id alone is not an
  // ownership credential because two jobs can share a caller-supplied run id.
  const targetedJob = message?.job_id ? jobs.get(message.job_id) : null;
  if (targetedJob) {
    assertMessageOwnsJob(message, targetedJob);
    if (runId !== targetedJob.run_id) {
      throw commandError(
        "run_mismatch",
        `inspect target run ${runId} does not match active job ${targetedJob.job_id}`,
        {
          phase: "profile",
          side_effect_started: false,
          expected_run_id: targetedJob.run_id,
          received_run_id: runId
        }
      );
    }
  }
  const liveInspectCandidates = targetedJob
    ? [targetedJob]
    : Array.from(jobs.values()).filter((job) => (
      job?.job_id
      && job.run_id === runId
      && job.workspace_id === message.workspace_id
      && (job.recipe ?? adapter.recipe) === adapter.recipe
    ));
  const acknowledgedInspectCandidates = targetedJob
    ? []
    : await loadAcknowledgedInspectableJobs(runId, message.workspace_id, adapter.recipe);
  const inspectCandidates = Array.from(new Map(
    [...liveInspectCandidates, ...acknowledgedInspectCandidates]
      .map((job) => [job.job_id, job])
  ).values());
  if (inspectCandidates.length !== 1) {
    const code = inspectCandidates.length === 0 ? "run_not_found" : "run_ambiguous";
    await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), code, inspectCandidates.length === 0
        ? `no durable ${adapter.displayName} job found for run ${runId}`
        : `more than one durable ${adapter.displayName} job owns run ${runId}`, {
          request_id: message.request_id,
          phase: "profile",
          side_effect_started: false
        }),
      { status: "failed", phase: "profile" }
    );
    return;
  }
  const inspectJob = inspectCandidates[0];
  if (!inspectJob.tab_id || !inspectJob.ownership_nonce) {
    await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), "ownership_unverified", `durable ${adapter.displayName} job ${inspectJob.job_id} has no inspectable owned tab`, {
        request_id: message.request_id,
        phase: "profile",
        side_effect_started: false
      }),
      { status: "failed", phase: "profile" }
    );
    return;
  }
  const tabs = await chrome.tabs.query({ url: adapter.tabQueryPattern });
  const matches = [];
  const errors = [];
  for (const tab of tabs) {
    if (!tab?.id) {
      continue;
    }
    try {
      const inspection = sanitizeInspection(await sendToTab(tab.id, {
        type: "yoetz_inspect_page",
        job_id: inspectJob.job_id,
        run_id: runId,
        workspace_id: message.workspace_id,
        ownership_nonce: inspectJob.ownership_nonce,
        recipe: adapter.recipe
      }));
      const responseInProgress = Boolean(inspection?.extraction?.is_generating);
      matches.push({
        tab_id: tab.id,
        url: tab.url ?? inspection?.url ?? null,
        title: tab.title ?? inspection?.title ?? null,
        // Non-final marker for an inspect read while the site still reports generation.
        response_in_progress: responseInProgress,
        note: responseInProgress
          ? "response still generating; extraction.text is a partial/interim assistant turn, not the final answer"
          : undefined,
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
    await postTerminalMessage(
      message,
      errorEnvelope(messageJob(message), "run_not_found", `no Yoetz ${adapter.displayName} tab found for run ${runId}`, {
        request_id: message.request_id,
        run_id: runId,
        inspected_tabs: errors,
        phase: "profile",
        side_effect_started: false
      }),
      { status: "failed", phase: "profile" }
    );
    return;
  }
  await postTerminalMessage(message, makeEnvelope("job_complete", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: runId,
    workspace_id: message.workspace_id,
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
  }), { status: "complete", phase: "profile" });
}

async function loadAcknowledgedInspectableJobs(runId, workspaceId, recipe) {
  if (!chrome.storage.local?.get) {
    return [];
  }
  const localStored = (await chrome.storage.local.get(null)) ?? {};
  return Object.entries(localStored)
    .filter(([key]) => key.startsWith(TERMINAL_ACK_KEY_PREFIX))
    .map(([, tombstone]) => inspectableJobFromAckTombstone(tombstone, runId, workspaceId, recipe))
    .filter(Boolean);
}

function inspectableJobFromAckTombstone(tombstone, runId, workspaceId, recipe) {
  const acknowledgedAt = Number(tombstone?.acknowledged_at);
  if (
    !tombstone?.job_id
    || tombstone.run_id !== runId
    || (tombstone.workspace_id ?? null) !== (workspaceId ?? null)
    || (tombstone.recipe ?? recipe) !== recipe
    || !Number.isInteger(tombstone.tab_id)
    || typeof tombstone.ownership_nonce !== "string"
    || tombstone.ownership_nonce.length === 0
    || !Number.isSafeInteger(acknowledgedAt)
    || Date.now() - acknowledgedAt > JOB_TTL_MS
  ) {
    return null;
  }
  return {
    job_id: tombstone.job_id,
    run_id: tombstone.run_id,
    workspace_id: tombstone.workspace_id ?? null,
    recipe,
    status: tombstone.status ?? terminalStatusForEnvelope({ type: tombstone.terminal_type }),
    tab_id: tombstone.tab_id,
    ownership_nonce: tombstone.ownership_nonce,
    conversation_id: tombstone.conversation_id ?? null,
    expected_conversation_id: tombstone.expected_conversation_id ?? null,
    submitted_conversation_id: tombstone.submitted_conversation_id ?? null,
    terminal_delivered_at: acknowledgedAt,
    inspect_only: true
  };
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
  await postTerminalMessage(message, makeEnvelope("job_complete", {
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
  }), { status: "complete", phase: "profile" });
}

function messageJob(message) {
  return {
    job_id: message?.job_id,
    run_id: message?.run_id,
    workspace_id: message?.workspace_id,
    capability_token: message?.capability_token,
    request_id: message?.request_id,
    recipe: message?.payload?.recipe ?? null
  };
}

async function recoverJobs(message) {
  await reconcileAcknowledgedTerminalTombstones();
  await restoreJobsFromStorage({ emitLostState: true });
  for (const job of jobs.values()) {
    if (job.cancel_pending && !TERMINAL_STATUSES.has(job.status)) {
      await finishPendingCancellation(job);
    }
  }
  await retryPendingTerminalJobs();
  for (const job of jobs.values()) {
    if (job.status !== "waiting_for_file"
        || job.terminal_delivered_at
        || cancellationIsPending(job)) {
      continue;
    }
    const adapter = adapterForJob(job);
    postNative(progress(job, "ready_for_file", {
      tab_id: job.tab_id,
      restored: true,
      message: `${adapter.displayName} tab is ready for bundle upload`
    }));
  }
  postNative(makeEnvelope("reconnect", {
    request_id: message.request_id,
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    payload: {
      restored_jobs: Array.from(jobs.values())
        .filter((job) => isReconnectRestorableJob(job) && reconnectIdentityMatches(message, job))
        .map((job) => job.job_id),
      restored_runs: Array.from(jobs.values())
        .filter((job) => (
          job.job_id
          && job.run_id
          && isReconnectRestorableJob(job)
          && reconnectIdentityMatches(message, job)
        ))
        .map((job) => ({
          job_id: job.job_id,
          run_id: job.run_id,
          workspace_id: job.workspace_id
        }))
    }
  }));
}

function reconnectIdentityMatches(message, job) {
  return message?.workspace_id == null || job?.workspace_id === message.workspace_id;
}

async function restoreJobsFromStorage({ emitLostState = false } = {}) {
  const stored = (await chrome.storage.session.get(null)) ?? {};
  const localStored = chrome.storage.local
    ? ((await chrome.storage.local.get(null)) ?? {})
    : {};
  const acknowledgedTombstones = Object.entries(localStored)
    .filter(([key, value]) => (
      key.startsWith(TERMINAL_ACK_KEY_PREFIX)
      && value?.job_id
      && value?.terminal_type
      && value?.sequence !== undefined
    ))
    .map(([, value]) => value);
  const isAcknowledged = (job) => acknowledgedTombstones.some((tombstone) => (
    tombstone.job_id === job.job_id
    && (tombstone.run_id ?? null) === (job.run_id ?? null)
    && (tombstone.workspace_id ?? null) === (job.workspace_id ?? null)
  ));
  const restoredByJobId = new Map();
  const addRestored = (job, priority = 1) => {
    if (!job?.job_id || isAcknowledged(job)) {
      return;
    }
    const existing = restoredByJobId.get(job.job_id);
    if (!existing || priority > existing.priority) {
      restoredByJobId.set(job.job_id, { job, priority });
    }
  };

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
      addRestored(job, 1);
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
    addRestored(value, 1);
  }
  for (const [key, value] of Object.entries(localStored)) {
    if (key.startsWith(CANCEL_PENDING_KEY_PREFIX)) {
      addRestored(value, 2);
    }
    if (key.startsWith(TERMINAL_OUTBOX_KEY_PREFIX)) {
      addRestored(value, 3);
    }
  }

  for (const { job } of restoredByJobId.values()) {
    if (!job?.job_id) {
      continue;
    }
    if (TERMINAL_STATUSES.has(job.status)) {
      if (isExpiredTerminalJob(job)) {
        await removeJobShard(job.job_id);
        continue;
      }
      rememberTerminalJob(job.job_id);
      // Keep every unacknowledged terminal in memory until its matching ACK.
      // Jobs without an envelope are repaired by retryPendingTerminalJobs.
      const needsTerminalRepair = job.status === "terminal_delivery_lost"
        || job.terminal_persistence_failed === true
        || Boolean(job.delivery_lost_phase);
      if (!job.terminal_delivered_at
          && (job.terminal_envelope || needsTerminalRepair)
          && !jobs.has(job.job_id)) {
        if (job.terminal_envelope) {
          stampTerminalSequence(job, job.terminal_envelope);
        }
        jobs.set(job.job_id, job);
      }
      continue;
    }
    if (job.cancel_pending || job.cancelled || job.cancel_retry_pending) {
      if (job.cancel_pending) {
        jobs.set(job.job_id, job);
        await finishPendingCancellation(job);
      } else if (job.cancel_retry_pending) {
        jobs.set(job.job_id, job);
      }
      continue;
    }
    if (Date.now() - (job.updated_at ?? job.started_at ?? 0) > JOB_TTL_MS) {
      continue;
    }
    if (jobs.has(job.job_id)) {
      continue;
    }
    if (canResumeJobAfterWorkerRestart(job)) {
      const adapter = adapterForJob(job);
      job.connection_generation = connectionGeneration;
      job.updated_at = Date.now();
      jobs.set(job.job_id, job);
      try {
        const contentScriptProbe = await waitForContentScript(job.tab_id, adapter, {
          phase: "upload",
          side_effect_started: false,
          send_committed: false
        });
        recordContentScriptContract(job, contentScriptProbe);
        await sendToTab(job.tab_id, { type: "yoetz_bind_job", job });
      } catch (error) {
        await handlePollerError(job, error);
        continue;
      }
      await persistJob(job);
      postNative(progress(job, "ready_for_file", {
        tab_id: job.tab_id,
        restored: true,
        message: `${adapterForJob(job).displayName} tab is ready for bundle upload`
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
        message: `restored ${adapterForJob(job).displayName} response wait after service-worker restart`
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

// An ACK is a durable commit point, but cleanup can be interrupted by a worker
// restart. Reconcile the ACK ledger before resuming live jobs so an acknowledged
// terminal cannot leave its outbox, cancellation intent, or owned tab behind.
async function reconcileAcknowledgedTerminalTombstones() {
  if (!chrome.storage.local?.get) {
    return;
  }
  const localStored = (await chrome.storage.local.get(null)) ?? {};
  const sessionStored = (await chrome.storage.session.get(null)) ?? {};
  for (const [key, tombstone] of Object.entries(localStored)) {
    if (!key.startsWith(TERMINAL_ACK_KEY_PREFIX)
        || !tombstone?.job_id
        || !tombstone.terminal_type
        || tombstone.cleanup_pending === "done") {
      continue;
    }
    const outbox = localStored[terminalOutboxStorageKey(tombstone.job_id)];
    const cancellation = localStored[cancelPendingStorageKey(tombstone.job_id)];
    const sessionJob = sessionStored[jobsStorageKey(tombstone.job_id)];
    const source = [outbox, cancellation, sessionJob]
      .find((value) => value && typeof value === "object") ?? {};
    const job = {
      ...source,
      job_id: tombstone.job_id,
      run_id: tombstone.run_id ?? source.run_id ?? null,
      workspace_id: tombstone.workspace_id ?? source.workspace_id ?? null,
      recipe: tombstone.recipe ?? source.recipe ?? null,
      tab_id: tombstone.tab_id ?? source.tab_id ?? null,
      close_tab_on_complete: tombstone.close_tab_on_complete === true
        || source.close_tab_on_complete === true,
      ownership_nonce: tombstone.ownership_nonce ?? source.ownership_nonce ?? null,
      conversation_id: tombstone.conversation_id ?? source.conversation_id ?? null,
      expected_conversation_id: tombstone.expected_conversation_id
        ?? source.expected_conversation_id
        ?? null,
      submitted_conversation_id: tombstone.submitted_conversation_id
        ?? source.submitted_conversation_id
        ?? null,
      status: tombstone.status
        ?? terminalStatusForEnvelope({ type: tombstone.terminal_type }),
      terminal_type: tombstone.terminal_type,
      terminal_sequence: tombstone.sequence,
      terminal_delivered_at: tombstone.acknowledged_at ?? Date.now(),
      updated_at: Date.now()
    };
    const shouldRetryTabClose = tombstone.cleanup_pending === "tab_close"
      || (!tombstone.cleanup_pending
        && job.close_tab_on_complete
        && job.tab_id
        && job.status === "complete");
    if (shouldRetryTabClose) {
      await closeOwnedTabOnComplete(job);
    }
    if (!await persistTerminalAckCleanup(job.job_id, "records")) {
      continue;
    }
    const outboxRemoved = await removeTerminalOutbox(job.job_id);
    const cancellationRemoved = await removeCancelPending(job.job_id);
    const { terminal_envelope: _terminalEnvelope, ...sessionCleanup } = job;
    const sessionPersisted = await persistJobBestEffort(sessionCleanup);
    if (!outboxRemoved || !cancellationRemoved || !sessionPersisted) {
      continue;
    }
    if (await persistTerminalAckCleanup(job.job_id, "done")) {
      jobs.delete(job.job_id);
      chunks.discard(job.job_id);
    }
  }
}

function canResumeJobAfterWorkerRestart(job) {
  // The tab is prepared and no file chunks have been accepted yet. There is no
  // in-memory ChunkAssembler state to reconstruct, so the native process can
  // continue by sending the first chunk after reconnect.
  return job.status === "waiting_for_file"
    && Boolean(job.tab_id)
    && !cancellationIsPending(job);
}

function canResumeWaitingResponseAfterWorkerRestart(job) {
  // The prompt has already been accepted by the site and the only remaining
  // mutable state is the DOM polling loop. Rebind the content script to the
  // persisted owned tab and continue structural-finality polling.
  return job.status === "waiting_response"
    && Boolean(job.tab_id)
    && !cancellationIsPending(job);
}

function normalizeJob(message, adapter) {
  const payload = message.payload ?? {};
  const conversation = adapter.normalizeConversationId(payload.conversation_id);
  return {
    job_id: message.job_id,
    run_id: message.run_id,
    workspace_id: message.workspace_id,
    ownership_nonce: cryptoRandomId(),
    capability_token: message.capability_token,
    request_id: message.request_id,
    recipe: adapter.recipe,
    prompt: payload.prompt ?? "",
    model: payload.model_strategy === "current" ? "current" : adapter.defaultModel,
    model_strategy: payload.model_strategy ?? "select",
    wait_timeout_ms: payload.wait_timeout_ms ?? DEFAULT_WAIT_TIMEOUT_MS,
    wait_interval_ms: payload.wait_interval_ms ?? 30000,
    upload_timeout_ms: payload.upload_timeout_ms ?? 120000,
    attachment_stall_timeout_ms: payload.attachment_stall_timeout_ms ?? 0,
    send_timeout_ms: payload.send_timeout_ms ?? 120000,
    close_tab_on_complete: payload.close_tab_on_complete === true,
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
    continuation_epoch: 0,
    status: "starting"
  };
}

function adapterForJob(job) {
  return siteAdapterForRecipe(job?.recipe);
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
  for (const key of [
    "state",
    "provider_message",
    "provider_dom",
    "requested_model",
    "model_selection_status",
    "model_selection_failure_reason",
    "model_selection_error_code",
    "send_committed",
    "required_content_script_capability",
    "required_content_script_capabilities",
    "content_script_instance_id",
    "expected_content_script_instance_id",
    "content_script_build",
    "expected_content_script_build",
    "expected_content_script_recipe",
    "content_script_recipe"
  ]) {
    if (error?.[key] !== undefined) {
      detail[key] = error[key];
    }
  }
  if (job.tab_id != null) {
    detail.tab_id = job.tab_id;
  }
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
  const attachmentTrace = sanitizeAttachmentTrace(error?.attachment_trace);
  if (attachmentTrace) {
    detail.attachment_trace = attachmentTrace;
  }
  return detail;
}

function sanitizeAttachmentTrace(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  const trace = {};
  for (const key of ATTACHMENT_TRACE_TIMESTAMP_KEYS) {
    const timestamp = value[key];
    if (Number.isSafeInteger(timestamp) && timestamp >= 0) {
      trace[key] = timestamp;
    }
  }
  for (const key of ["soft_timeout_pending_legs", "hard_timeout_pending_legs"]) {
    if (Array.isArray(value[key])) {
      trace[key] = value[key].filter((leg) => ATTACHMENT_TRACE_PENDING_LEGS.has(leg)).slice(0, 4);
    }
  }
  return Object.keys(trace).length > 0 ? trace : null;
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
        profile_id: identity.profile_id || null,
        recipes: [...ADVERTISED_RECIPES],
        capabilities: [...ADVERTISED_CAPABILITIES]
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
        profile_id: null,
        recipes: [...ADVERTISED_RECIPES],
        capabilities: [...ADVERTISED_CAPABILITIES]
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

async function waitForSiteTab(tabId, adapter) {
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const tab = await chrome.tabs.get(tabId);
    if (tab.status === "complete" && adapter.isAllowedTabUrl(tab.url)) {
      return;
    }
    await sleep(500);
  }
  throw new Error(`${adapter.displayName} tab ${tabId} did not load`);
}

async function createJobTab(url, adapter) {
  const policy = adapter.tabActivation ?? {};
  const activateOnCreate = policy.activateOnCreate === true;
  return chrome.tabs.create({ url, active: activateOnCreate });
}

async function discardCreatedJobTab(tab) {
  if (!tab?.id || !chrome.tabs?.remove) {
    return;
  }
  try {
    await chrome.tabs.remove(tab.id);
  } catch {
    // Cancellation already removed it, or the browser closed it concurrently.
  }
}

async function waitForContentScript(tabId, adapter, detail = {
  phase: "upload",
  side_effect_started: false,
  send_committed: false
}) {
  let lastError = null;
  for (let attempt = 0; attempt < CONTENT_SCRIPT_RECONNECT_ATTEMPTS; attempt += 1) {
    try {
      return await requireContentScriptCapability(tabId, adapter, detail);
    } catch (error) {
      if ([
        "content_script_build_mismatch",
        "content_script_capability_missing",
        "content_script_recipe_mismatch",
        "content_script_instance_missing"
      ].includes(error?.code)) {
        throw error;
      }
      lastError = error;
      await sleep(CONTENT_SCRIPT_RECONNECT_DELAY_MS);
    }
  }
  if (lastError) {
    throw new Error(`Yoetz content script did not become ready in ${adapter.displayName} tab ${tabId}`);
  }
  throw new Error(`Yoetz content script did not become ready in ${adapter.displayName} tab ${tabId}`);
}

function requiredContentScriptCapabilitiesForRecipe(recipe) {
  return [
    NATIVE_JOB_COMMANDS_CAPABILITY,
    ...(recipe === "chatgpt"
      ? [CHATGPT_CLICK_BOUND_SEND_RECEIPT_CAPABILITY]
      : [])
  ];
}

function requiredContentScriptCapabilities(adapter) {
  return requiredContentScriptCapabilitiesForRecipe(adapter?.recipe);
}

async function requireContentScriptCapability(tabId, adapter, detail = {}) {
  const probe = await sendToTab(tabId, { type: "yoetz_probe", recipe: adapter.recipe });
  const required = requiredContentScriptCapabilities(adapter);
  const capabilities = Array.isArray(probe?.capabilities) ? probe.capabilities : [];
  const instanceId = String(probe?.content_script_instance_id ?? "").trim();
  if (!instanceId) {
    throw commandError(
      "content_script_instance_missing",
      `${adapter.displayName} content script did not advertise an injection identity; refusing provider-visible work until the managed extension is reloaded`,
      {
        ...detail,
        content_script_instance_id: null,
        expected_content_script_instance_id: "non-empty",
        tab_id: tabId
      }
    );
  }
  if (probe?.recipe !== adapter.recipe) {
    throw commandError(
      "content_script_recipe_mismatch",
      `${adapter.displayName} tab reported recipe ${JSON.stringify(probe?.recipe ?? null)} instead of ${JSON.stringify(adapter.recipe)}; refusing provider-visible work until the managed extension is reloaded`,
      {
        ...detail,
        expected_content_script_recipe: adapter.recipe,
        content_script_recipe: probe?.recipe ?? null,
        content_script_instance_id: instanceId,
        required_content_script_capabilities: required,
        tab_id: tabId
      }
    );
  }
  const expectedBuild = CONTENT_SCRIPT_BUILD;
  const observedBuild = String(probe?.content_script_build ?? "").trim();
  if (!observedBuild || observedBuild !== expectedBuild) {
    throw commandError(
      "content_script_build_mismatch",
      `${adapter.displayName} content script build ${JSON.stringify(observedBuild || null)} does not match the extension content contract ${JSON.stringify(expectedBuild)}; refusing provider-visible work until the managed extension is reloaded`,
      {
        ...detail,
        content_script_build: observedBuild || null,
        expected_content_script_build: expectedBuild,
        content_script_instance_id: instanceId,
        tab_id: tabId
      }
    );
  }
  const missing = required.find((capability) => !capabilities.includes(capability));
  if (!missing) {
    return probe;
  }
  throw commandError(
    "content_script_capability_missing",
    `${adapter.displayName} content script does not advertise required capability ${missing}; refusing provider-visible work until the managed extension is reloaded`,
    {
      ...detail,
      required_content_script_capability: missing,
      required_content_script_capabilities: required,
      content_script_build: probe?.content_script_build ?? null,
      content_script_instance_id: instanceId,
      tab_id: tabId
    }
  );
}

function recordContentScriptContract(job, probe) {
  if (!job.recipe && probe?.recipe) {
    job.recipe = probe.recipe;
  }
  if (job.model_strategy == null) {
    job.model_strategy = "select";
  }
  if (job.model == null) {
    job.model = siteAdapterForRecipe(job.recipe).defaultModel;
  }
  job.content_script_instance_id = String(probe?.content_script_instance_id ?? "").trim();
  job.content_script_build = String(probe?.content_script_build ?? "").trim();
  job.content_script_recipe = probe?.recipe ?? null;
}

async function sendToTab(tabId, message) {
  const response = await chrome.tabs.sendMessage(tabId, secureContentScriptMessage(message));
  if (!response?.ok) {
    throw tabCommandError(response);
  }
  return response.payload;
}

async function verifyTabOwnership(job) {
  if (!job?.tab_id) {
    return { owned: true, reason: "no_tab" };
  }
  const adapter = adapterForJob(job);
  let response;
  try {
    response = await chrome.tabs.sendMessage(job.tab_id, {
      type: "yoetz_verify_job_ownership",
      job
    });
  } catch (error) {
    return {
      owned: false,
      reason: "ownership_probe_failed",
      error: String(error?.message ?? error)
    };
  }
  if (!response?.ok) {
    return {
      owned: false,
      reason: response?.code ?? "ownership_probe_rejected",
      error: response?.error ?? "content script did not confirm durable tab ownership"
    };
  }
  const result = response.payload;
  const expectedOrigin = new URL(adapter.homeUrl).origin;
  const expectedConversationId = expectedConversationIdForJob(job);
  const observedConversationId = result?.url
    ? adapter.conversationIdFromUrl(result.url)
    : null;
  const conversationMatches = !expectedConversationId
    || observedConversationId === expectedConversationId
    || Boolean(adapter.isExpectedConversationIdAssignment?.(
      job,
      expectedConversationId,
      observedConversationId
    ));
  if (
    result?.owned !== true
    || result.job_id !== job.job_id
    || result.run_id !== job.run_id
    || (job.workspace_id != null && result.workspace_id !== job.workspace_id)
    || (job.ownership_nonce != null && result.ownership_nonce !== job.ownership_nonce)
    || result.origin !== expectedOrigin
    || !conversationMatches
  ) {
    return {
      owned: false,
      reason: "ownership_probe_mismatch",
      error: "content script returned ownership evidence for a different job, run, workspace, nonce, conversation, or origin",
      expected_origin: expectedOrigin,
      expected_workspace_id: job.workspace_id ?? null,
      expected_ownership_nonce: job.ownership_nonce ?? null,
      expected_conversation_id: expectedConversationId,
      observed_conversation_id: observedConversationId,
      observed: result ?? null
    };
  }
  return result;
}

function secureContentScriptMessage(message) {
  if (!SECURE_CONTENT_SCRIPT_COMMANDS.has(message?.type)) {
    return message;
  }
  const job = message?.job;
  const instanceId = String(job?.content_script_instance_id ?? "").trim();
  const build = String(job?.content_script_build ?? "").trim();
  if (!instanceId || !build || !job?.recipe) {
    throw commandError(
      "content_script_contract_missing",
      `cannot send ${message.type} without a bound content-script contract`,
      {
        phase: contentScriptCommandPhase(message),
        side_effect_started: contentScriptCommandHasSideEffect(message),
        content_script_instance_id: instanceId || null,
        expected_content_script_build: build || CONTENT_SCRIPT_BUILD,
        expected_content_script_recipe: job?.recipe ?? null,
        tab_id: null
      }
    );
  }
  return {
    type: "yoetz_secure_command",
    command: message.type,
    content_script_contract: {
      content_script_instance_id: instanceId,
      content_script_build: build,
      content_script_recipe: job.recipe,
      required_content_script_capabilities: requiredContentScriptCapabilitiesForRecipe(job.recipe)
    },
    payload: message
  };
}

function contentScriptCommandPhase(message) {
  if (message?.type === "yoetz_configure_model") {
    return "model_selection";
  }
  if (message?.type === "yoetz_send_prompt") {
    return "send";
  }
  if (message?.type === "yoetz_extract_response" || message?.type === "yoetz_fetch_conversation") {
    return "wait_response";
  }
  if (message?.type === "yoetz_cancel_send") {
    return "send";
  }
  if (message?.type === "yoetz_bind_job" && message?.job?.status === "waiting_response") {
    return "wait_response";
  }
  return "upload";
}

function contentScriptCommandHasSideEffect(message) {
  return [
    "yoetz_send_prompt",
    "yoetz_extract_response",
    "yoetz_fetch_conversation",
    "yoetz_cancel_send"
  ].includes(message?.type)
    || (message?.type === "yoetz_bind_job" && message?.job?.status === "waiting_response");
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
  if (response?.attachment_trace !== undefined) {
    error.attachment_trace = response.attachment_trace;
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
    "required_content_script_capability",
    "required_content_script_capabilities",
    "content_script_instance_id",
    "expected_content_script_instance_id",
    "content_script_build",
    "expected_content_script_build",
    "expected_content_script_recipe",
    "content_script_recipe",
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

async function waitForResponse(job, continuationEpoch = job?.continuation_epoch) {
  const completion = adapterForJob(job).completion;
  const startedAt = Number(job.response_wait_started_at) || Date.now();
  job.response_wait_started_at = startedAt;
  const interval = Math.max(500, Math.min(Number(job.wait_interval_ms) || 30000, 30000));
  const finalAffordanceIdleMs = responseStableIdleThresholdMs(interval);
  // The post-affordance confirm window is clamped to the idle floor so it can only
  // shorten the wait for a settled response, never extend it past the late-hydration
  // ceiling (and so test envs that drive MIN_STABLE_IDLE_MS below the confirm default
  // still complete promptly).
  const affordanceConfirmMs = Math.min(MIN_AFFORDANCE_CONFIRM_MS, finalAffordanceIdleMs);
  const finalStructuralConfirmMs = completion.finalAffordanceRequiresStableIdle
    ? finalAffordanceIdleMs
    : affordanceConfirmMs;
  let best = { method: "none", text: "", is_generating: true };
  let last = { method: "none", text: "", is_generating: true };
  let finalAffordanceCandidate = null;
  let bestFinalAffordanceCandidate = null;
  let finalAffordanceCandidateSinceMs = 0;
  let unscopedCopyCandidate = null;
  let bestUnscopedCopyCandidate = null;
  let unscopedCopyCandidateSinceMs = 0;
  let renderRefreshCandidate = null;
  let renderRefreshCandidateSinceMs = 0;
  let extractionFailureSinceMs = 0;
  let finalityStallSignature = null;
  let finalityStallCandidateSinceMs = 0;
  let lastResponseProgressAt = 0;
  let lastResponseProgressGenerating = null;
  let lastResponseProgressInterimTurn = false;
  let lastWaitingProgressAt = startedAt;
  let reportedAwaitingFinalAffordance = false;
  const timeoutMs = responseWaitTimeoutMs(job);
  while (Date.now() - startedAt <= timeoutMs) {
    if (!jobContinuationIsLive(job, continuationEpoch)) {
      return null;
    }
    assertJobConnectionCurrent(job);
    if (cancellationIsPending(job)) {
      return null;
    }
    const extraction = await extractResponseForJob(job);
    if (!jobContinuationIsLive(job, continuationEpoch)) {
      return null;
    }
    assertJobConnectionCurrent(job);
    if (reconcileJobConversationCurrent(job, extraction)) {
      await persistJob(job);
      if (!jobContinuationIsLive(job, continuationEpoch)) {
        return null;
      }
    }
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
    const currentFinalityStallSignature = isClaudeFinalityConflict(job, extraction)
      ? responseFinalityStallSignature(extraction)
      : null;
    if (currentFinalityStallSignature === null) {
      finalityStallSignature = null;
      finalityStallCandidateSinceMs = 0;
    } else if (currentFinalityStallSignature !== finalityStallSignature) {
      finalityStallSignature = currentFinalityStallSignature;
      finalityStallCandidateSinceMs = Date.now();
    } else if (!finalityStallCandidateSinceMs) {
      finalityStallCandidateSinceMs = Date.now();
    }
    const finalityStalledForMs = finalityStallCandidateSinceMs
      ? Date.now() - finalityStallCandidateSinceMs
      : 0;
    if (currentFinalityStallSignature !== null
        && finalityStalledForMs >= RESPONSE_FINALITY_STALL_MS) {
      const inspectCommand = inspectCommandForJob(job);
      const adapter = adapterForJob(job);
      await failJob(
        job,
        "response_finality_stalled",
        `${adapter.displayName} response content remained unchanged for ${formatDurationForMessage(finalityStalledForMs)} without positive finality proof. The owned ${adapter.displayName} tab is left open; inspect it before rerunning with: ${inspectCommand}. Do not rerun until inspection because the prompt was already submitted.`,
        {
          phase: "wait_response",
          side_effect_started: true,
          completion_reason: "non_streaming_turn_with_persistent_stop",
          send_committed: true,
          stable_for_ms: finalityStalledForMs,
          stall_timeout_ms: RESPONSE_FINALITY_STALL_MS,
          extraction_method: extraction.method,
          response_length: extraction.text.length,
          assistant_count: extraction.assistant_count ?? 0,
          assistant_identity: extraction.assistant_identity,
          turn_index: extraction.turn_index ?? -1,
          copy_button_count: extraction.copy_button_count ?? 0,
          has_copy_button: Boolean(extraction.has_copy_button),
          inspect_command: inspectCommand,
          diagnostics: diagnosticPayload(extraction.diagnostics)
        }
      );
      return null;
    }
    if (postSend && extraction?.text && extraction.text.length >= best.text.length) {
      best = extraction;
    }
    if (postSend && extraction?.text) {
      const responseProgressState = interimTurnState(job, extraction);
      const meaningfulProgressTransition = (
        responseProgressState.interimAssistantTurn && !lastResponseProgressInterimTurn
      ) || (
        lastResponseProgressGenerating === true && !responseProgressState.generating
      );
      if (!lastResponseProgressAt
          || meaningfulProgressTransition
          || Date.now() - lastResponseProgressAt >= interval) {
        postResponseProgress(job, extraction);
        lastResponseProgressAt = Date.now();
        assertJobConnectionCurrent(job);
      }
      lastResponseProgressGenerating = responseProgressState.generating;
      lastResponseProgressInterimTurn = responseProgressState.interimAssistantTurn;
    }
    const extractionIdle = !extraction?.is_generating;
    const backendApiPending = Boolean(extraction?.backend_api_pending);
    const scopedDomExtractionCandidate = Boolean(
      postSend
      && extractionIdle
      && extraction?.method !== "page_text_fallback"
    );
    // Once the ChatGPT conversation API has answered but says the active
    // lineage has no completed end_turn yet, DOM affordances are not proof of
    // finality. Reasoning captions can temporarily render as copyable assistant
    // content while stop/Answer-now controls disappear. Keep the DOM sample for
    // render-refresh diagnostics, but require the backend positive anchor before
    // allowing it to complete the job.
    const scopedExtractionCandidate = scopedDomExtractionCandidate && !backendApiPending;
    const backendApiFinal = Boolean(scopedExtractionCandidate && completion.isFreshBackendApiExtraction(extraction));
    const finalAffordance = Boolean(scopedExtractionCandidate && completion.hasFinalAssistantAffordance(extraction));
    const finalStructuralResponse = finalAffordance || backendApiFinal;
    // Broad page text is diagnostic only; final controls without scoped answer
    // text means extraction failed, not that page chrome is safe to return.
    const finalAffordanceWithoutScopedText = Boolean(
      postSendAssistantActivity
      && extraction?.method === "page_text_fallback"
      && !extraction?.is_generating
      && !backendApiPending
      && completion.hasFinalAssistantAffordance(extraction)
    );
    const stableIdleUnscopedCopy = Boolean(
      scopedExtractionCandidate
      && !finalStructuralResponse
      && completion.hasStableIdleUnscopedCopyAffordance(
        job,
        extraction,
        MIN_UNSCOPED_COPY_STABLE_TEXT_CHARS
      )
    );
    const renderRefreshCandidateEligible = completion.isRenderFreezeRefreshCandidate(
      job,
      extraction,
      scopedDomExtractionCandidate,
      finalStructuralResponse,
      {
        conversationId: conversationIdForJob(job, extraction),
        shortResponseMaxChars: RENDER_FREEZE_SHORT_RESPONSE_MAX_CHARS,
        maxRefreshAttempts: MAX_RENDER_REFRESH_ATTEMPTS
      }
    );
    let stableForMs = 0;
    let unscopedCopyStableForMs = 0;
    let renderRefreshStableForMs = 0;
    if (backendApiFinal) {
      return completion.completedExtraction(extraction, "backend_api", 0);
    }
    if (finalStructuralResponse) {
      // Once the adapter exposes its final structural signal, scope and turn
      // checks have already ruled out pre-send content. From here we track the best
      // scoped candidate by text growth so late page chrome cannot replace a
      // completed response, and transient generating blips cannot forget it.
      const bestSelection = completion.selectFinalAffordanceCandidate(bestFinalAffordanceCandidate, extraction);
      bestFinalAffordanceCandidate = bestSelection.candidate;
      const candidateSelection = completion.selectFinalAffordanceCandidate(
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
      if (stableForMs >= finalStructuralConfirmMs) {
        const completionReason = finalAffordanceCandidate?.has_copy_button
          ? "copy_button"
          : "stable_idle";
        return completion.completedExtraction(finalAffordanceCandidate, completionReason, stableForMs);
      }
      unscopedCopyCandidate = null;
      bestUnscopedCopyCandidate = null;
      unscopedCopyCandidateSinceMs = 0;
    } else if (extraction?.is_generating || backendApiPending) {
      finalAffordanceCandidate = null;
      finalAffordanceCandidateSinceMs = 0;
      unscopedCopyCandidate = null;
      unscopedCopyCandidateSinceMs = 0;
    } else if (!postSendAssistantActivity) {
      finalAffordanceCandidate = null;
      bestFinalAffordanceCandidate = null;
      finalAffordanceCandidateSinceMs = 0;
      unscopedCopyCandidate = null;
      bestUnscopedCopyCandidate = null;
      unscopedCopyCandidateSinceMs = 0;
      renderRefreshCandidate = null;
      renderRefreshCandidateSinceMs = 0;
    }
    if (stableIdleUnscopedCopy) {
      const bestSelection = completion.selectFinalAffordanceCandidate(bestUnscopedCopyCandidate, extraction);
      bestUnscopedCopyCandidate = bestSelection.candidate;
      const candidateSelection = completion.selectFinalAffordanceCandidate(
        unscopedCopyCandidate ?? bestUnscopedCopyCandidate,
        extraction
      );
      if (!unscopedCopyCandidate || candidateSelection.candidate !== unscopedCopyCandidate) {
        if (!unscopedCopyCandidate || candidateSelection.resetTimer) {
          unscopedCopyCandidateSinceMs = Date.now();
        }
        unscopedCopyCandidate = candidateSelection.candidate;
      } else if (!unscopedCopyCandidateSinceMs) {
        unscopedCopyCandidateSinceMs = Date.now();
      }
      unscopedCopyStableForMs = Date.now() - unscopedCopyCandidateSinceMs;
      if (unscopedCopyStableForMs >= finalAffordanceIdleMs) {
        return completion.completedExtraction(
          unscopedCopyCandidate,
          "stable_idle_unscoped_copy_button",
          unscopedCopyStableForMs
        );
      }
    } else if (!finalStructuralResponse) {
      unscopedCopyCandidate = null;
      unscopedCopyCandidateSinceMs = 0;
      if (!postSendAssistantActivity) {
        bestUnscopedCopyCandidate = null;
      }
    }
    if (renderRefreshCandidateEligible) {
      if (!completion.sameRenderRefreshCandidate(renderRefreshCandidate, extraction)) {
        renderRefreshCandidate = extraction;
        renderRefreshCandidateSinceMs = Date.now();
      } else if (!renderRefreshCandidateSinceMs) {
        renderRefreshCandidateSinceMs = Date.now();
      }
      renderRefreshStableForMs = Date.now() - renderRefreshCandidateSinceMs;
      if (renderRefreshStableForMs >= MIN_RENDER_FREEZE_IDLE_MS
          && completion.canRefreshFrozenRender(job, MAX_RENDER_REFRESH_ATTEMPTS)) {
        await refreshFrozenRender(job, extraction, renderRefreshStableForMs, continuationEpoch);
        if (!jobContinuationIsLive(job, continuationEpoch)) {
          return null;
        }
        renderRefreshCandidate = null;
        renderRefreshCandidateSinceMs = 0;
        finalAffordanceCandidate = null;
        finalAffordanceCandidateSinceMs = 0;
        unscopedCopyCandidate = null;
        unscopedCopyCandidateSinceMs = 0;
        continue;
      }
    } else {
      renderRefreshCandidate = null;
      renderRefreshCandidateSinceMs = 0;
    }
    const awaitingFinalAffordance = Boolean(scopedDomExtractionCandidate && !finalStructuralResponse);
    if (finalAffordanceWithoutScopedText) {
      if (!extractionFailureSinceMs) {
        extractionFailureSinceMs = Date.now();
      }
      const extractionFailureStableForMs = Date.now() - extractionFailureSinceMs;
      if (extractionFailureStableForMs >= finalAffordanceIdleMs) {
        await failJob(
          job,
          "response_extraction_failed",
          completion.finalAffordanceExtractionFailureMessage(
            job,
            extraction,
            extractionFailureStableForMs,
            inspectCommandForJob(job)
          ),
          {
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
          }
        );
        return null;
      }
    } else {
      extractionFailureSinceMs = 0;
    }
    const nextDelay = job.backend_api_confirmation
      ? Math.min(interval, Math.max(50, BACKEND_API_CONFIRMATION_MS))
      : finalAffordance
      // Poll fast while confirming a latched final affordance so the short confirm
      // window is sampled across several ticks rather than overshot by a coarse poll.
      ? Math.min(interval, AFFORDANCE_CONFIRM_POLL_MS)
      : finalAffordanceWithoutScopedText
        ? Math.min(interval, Math.max(finalAffordanceIdleMs, 500))
      : postSendAssistantActivity
        ? Math.min(interval, POST_SEND_ASSISTANT_ACTIVITY_POLL_MS)
        : interval;
    const nowMs = Date.now();
    const elapsedMs = nowMs - startedAt;
    const enteredAwaitingFinalAffordance =
      awaitingFinalAffordance && !reportedAwaitingFinalAffordance;
    if (enteredAwaitingFinalAffordance
        || nowMs - lastWaitingProgressAt >= WAITING_RESPONSE_PROGRESS_INTERVAL_MS) {
      const waitingDetail = {
        elapsed_ms: elapsedMs,
        timeout_ms: timeoutMs,
        next_poll_ms: nextDelay,
        stable_for_ms: Math.max(stableForMs, unscopedCopyStableForMs, renderRefreshStableForMs),
        final_affordance: finalAffordance,
        backend_api_final: backendApiFinal,
        stable_idle_unscoped_copy_candidate: stableIdleUnscopedCopy,
        extraction_failure_candidate: finalAffordanceWithoutScopedText,
        render_refresh_candidate: renderRefreshCandidateEligible,
        backend_api_pending: backendApiPending
      };
      if (awaitingFinalAffordance) {
        waitingDetail.awaiting_final_affordance = true;
        waitingDetail.inspect_command = inspectCommandForJob(job);
      }
      postWaitingResponseProgress(job, extraction, waitingDetail);
      reportedAwaitingFinalAffordance ||= enteredAwaitingFinalAffordance;
      lastWaitingProgressAt = nowMs;
    }
    await sleep(nextDelay);
  }
  const inspectCommand = inspectCommandForJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return null;
  }
  const adapter = adapterForJob(job);
  const timeoutSummary = `${adapter.displayName} response did not reach stable completion before timeout (baseline_assistant_count=${job.response_baseline?.assistant_count ?? 0}, best_method=${best.method}, best_text_chars=${best.text?.length ?? 0}, best_assistant_count=${best.assistant_count ?? 0}, best_turn_index=${best.turn_index ?? -1}, best_copy_button_count=${best.copy_button_count ?? 0}, best_is_generating=${Boolean(best.is_generating)}, last_method=${last.method}, last_text_chars=${last.text?.length ?? 0}, last_assistant_count=${last.assistant_count ?? 0}, last_turn_index=${last.turn_index ?? -1}, last_copy_button_count=${last.copy_button_count ?? 0}, last_is_generating=${Boolean(last.is_generating)}, last_diagnostics=${diagnosticSummary(last.diagnostics)}). The owned ${adapter.displayName} tab is left open; if it finishes later, recover with: ${inspectCommand}`;
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

// How many assistant turns have appeared since the prompt was sent. The post-send baseline is
// captured before send; the first turn beyond it is the response's first turn, and any turn beyond
// THAT (while still generating) means the earlier turns were interim Pro status posts.
function assistantTurnsSinceSend(job, extraction) {
  const baseline = Number(job.response_baseline?.assistant_count ?? 0);
  const current = Number(extraction?.assistant_count ?? 0);
  return Math.max(0, current - baseline);
}

// Single source of truth for "is this progress an interim Pro turn, not the answer?".
// A second-or-later assistant turn appearing while generation is still active proves the earlier
// turn(s) were interim status posts ("I'll review...", "I've narrowed..."), not the final answer.
function interimTurnState(job, extraction) {
  const generating = Boolean(extraction?.is_generating || extraction?.backend_api_pending);
  const turnsSinceSend = assistantTurnsSinceSend(job, extraction);
  return { generating, turnsSinceSend, interimAssistantTurn: generating && turnsSinceSend > 1 };
}

function postResponseProgress(job, extraction) {
  const text = String(extraction?.text ?? "");
  if (!text || text === job.last_response_progress_text) {
    return;
  }
  const previous = String(job.last_response_progress_text ?? "");
  const delta = text.startsWith(previous) ? text.slice(previous.length) : text;
  job.last_response_progress_text = text;
  const { generating, turnsSinceSend, interimAssistantTurn } = interimTurnState(job, extraction);
  postNative(progress(job, "response_observed", {
    message: interimAssistantTurn
      ? `interim assistant turn observed (${text.length} chars, turn ${turnsSinceSend} since send, still generating — not the final response)`
      : `response observed (${text.length} chars${generating ? ", still generating" : ""})`,
    response_in_progress: generating,
    interim_assistant_turn: interimAssistantTurn,
    assistant_turns_since_send: turnsSinceSend,
    response_delta: delta,
    response_length: text.length,
    response_tail: text.slice(-500),
    extraction_method: extraction.method,
    is_generating: generating,
    backend_api_pending: Boolean(extraction.backend_api_pending),
    assistant_count: extraction.assistant_count ?? 0,
    turn_index: extraction.turn_index ?? -1,
    copy_button_count: extraction.copy_button_count ?? 0,
    has_copy_button: Boolean(extraction.has_copy_button)
  }));
}

function postWaitingResponseProgress(job, extraction, detail = {}) {
  const adapter = adapterForJob(job);
  const elapsedMs = Number(detail.elapsed_ms ?? 0);
  const timeoutMs = Number(detail.timeout_ms ?? responseWaitTimeoutMs(job));
  const finalityStatus = detail.awaiting_final_affordance ? ", waiting for final assistant controls" : "";
  const scopedCopyStatus = extraction?.has_copy_button ? ", scoped_copy_button=true" : ", scoped_copy_button=false";
  const { generating, turnsSinceSend, interimAssistantTurn } = interimTurnState(job, extraction);
  const interimStatus = interimAssistantTurn ? `, interim assistant turn ${turnsSinceSend} (response not final)` : "";
  postNative(progress(job, "waiting_response", {
    ...detail,
    inspect_command: detail.inspect_command ?? inspectCommandForJob(job),
    message: `waiting for ${adapter.displayName} response (${formatDurationForMessage(elapsedMs)} elapsed of ${formatDurationForMessage(timeoutMs)} timeout; method=${extraction?.method ?? "none"}, assistant_count=${extraction?.assistant_count ?? 0}, copy_buttons=${extraction?.copy_button_count ?? 0}${scopedCopyStatus}${generating ? ", generating" : ""}${interimStatus}${finalityStatus})`,
    extraction_method: extraction?.method ?? "none",
    response_in_progress: generating,
    interim_assistant_turn: interimAssistantTurn,
    assistant_turns_since_send: turnsSinceSend,
    is_generating: generating,
    backend_api_pending: Boolean(extraction?.backend_api_pending),
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
  return `yoetz browser extension inspect ${adapterForJob(job).inspectScope} --run-id ${job.run_id}${selector}`;
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
    finality: diagnostics.finality ?? {},
    assistant_turn_snippets: (diagnostics.assistant_turn_snippets ?? []).slice(-3),
    article_snippets: (diagnostics.article_snippets ?? []).slice(-3),
    markdown_snippets: (diagnostics.markdown_snippets ?? []).slice(-3),
    stop_control_snippets: (diagnostics.stop_control_snippets ?? []).slice(0, 3)
  };
}

async function extractResponseForJob(job) {
  const domExtraction = await extractDomResponseForJob(job);
  const backendExtraction = await maybeBackendApiExtractionForJob(job, domExtraction);
  return backendExtraction ?? domExtraction;
}

async function extractDomResponseForJob(job) {
  try {
    const extraction = await sendToTab(job.tab_id, { type: "yoetz_extract_response", job });
    forgetSettledSuccessfulRecovery(job.job_id);
    return extraction;
  } catch (error) {
    if (
      !isRecoverableContentScriptError(error)
      || Number(job.content_script_recovery_incidents ?? 0) >= MAX_CONTENT_SCRIPT_RECOVERY_INCIDENTS
    ) {
      throw error;
    }
    await recoverContentScriptJob(job, error);
    return sendToTab(job.tab_id, { type: "yoetz_extract_response", job });
  }
}

async function maybeBackendApiExtractionForJob(job, domExtraction) {
  const completion = adapterForJob(job).completion;
  if (!completion.supportsBackendApiFallback || job?.backend_api_disabled) {
    return null;
  }
  const conversationId = conversationIdForJob(job, domExtraction);
  if (!domExtraction || domExtraction.manual_handoff || !conversationId) {
    return null;
  }
  // DOM generation state is a useful rendering hint, not an authority boundary.
  // A settled reasoning-recap widget can be misclassified as still generating
  // after a render refresh. Keep polling the active-lineage backend anchor so it
  // can prove completion (or keep us pending) in either DOM state.
  const now = Date.now();
  const lastFetchAt = Number(job.backend_api_last_fetch_at ?? 0);
  const confirmation = job.backend_api_confirmation;
  const confirmationDue = Boolean(
    confirmation
    && now - Number(confirmation.observed_at ?? 0) >= BACKEND_API_CONFIRMATION_MS
  );
  if ((confirmation && !confirmationDue)
      || (!confirmation && now - lastFetchAt < BACKEND_API_FETCH_COOLDOWN_MS)) {
    return job.backend_api_pending
      ? backendApiPendingExtraction(domExtraction, null)
      : null;
  }
  job.backend_api_last_fetch_at = now;
  job.updated_at = now;
  await persistJob(job);
  try {
    const backendExtraction = await sendToTab(job.tab_id, {
      type: "yoetz_fetch_conversation",
      job,
      conversation_id: conversationId
    });
    const normalized = normalizeBackendApiExtraction(backendExtraction, domExtraction, conversationId);
    const fresh = completion.isFreshBackendApiExtraction(normalized);
    job.backend_api_consecutive_failures = 0;
    if (!fresh) {
      job.backend_api_confirmation = null;
      job.backend_api_pending = true;
      job.updated_at = Date.now();
      await persistJob(job);
      return backendApiPendingExtraction(domExtraction, normalized);
    }
    const nodeId = String(normalized.node_id ?? "").trim();
    if (!nodeId) {
      job.backend_api_confirmation = null;
      job.backend_api_pending = true;
      job.updated_at = Date.now();
      await persistJob(job);
      return backendApiPendingExtraction(domExtraction, {
        ...normalized,
        backend_api_detail: "fresh backend answer omitted node_id required for confirmation"
      });
    }
    if (!confirmation || String(confirmation.node_id ?? "") !== nodeId) {
      job.backend_api_confirmation = { node_id: nodeId, observed_at: Date.now() };
      job.backend_api_pending = true;
      job.updated_at = Date.now();
      await persistJob(job);
      return backendApiPendingExtraction(domExtraction, {
        ...normalized,
        backend_api_detail: `awaiting confirmation of backend answer node ${nodeId}`
      });
    }
    job.backend_api_confirmation = null;
    job.backend_api_pending = false;
    job.updated_at = Date.now();
    await persistJob(job);
    return normalized;
  } catch (error) {
    if (!completion.isBackendApiFallbackError(error)) {
      throw error;
    }
    if (job.backend_api_pending) {
      // Do not silently downgrade to DOM finality after the backend has already
      // proved that the active lineage is unfinished. Transient fetch failures
      // keep the DOM barred and retry on the normal cooldown; only a sustained
      // loss of the positive anchor terminates the job.
      job.backend_api_consecutive_failures = Math.max(
        0,
        Number(job.backend_api_consecutive_failures ?? 0) || 0
      ) + 1;
      job.backend_api_confirmation = null;
      job.updated_at = Date.now();
      if (job.backend_api_consecutive_failures < MAX_BACKEND_API_CONSECUTIVE_FAILURES) {
        await persistJob(job);
        return backendApiPendingExtraction(domExtraction, null);
      }
      job.backend_api_disabled = true;
      job.backend_api_disabled_reason = error?.code ?? String(error?.message ?? error);
      await persistJob(job);
      throw error;
    }
    job.backend_api_disabled = true;
    job.backend_api_disabled_reason = error?.code ?? String(error?.message ?? error);
    job.backend_api_pending = false;
    job.backend_api_confirmation = null;
    job.updated_at = Date.now();
    await persistJob(job);
    return null;
  }
}

function backendApiPendingExtraction(domExtraction, backendExtraction) {
  return {
    ...domExtraction,
    backend_api_pending: true,
    backend_api_detail: backendExtraction?.backend_api_detail ?? null
  };
}

function normalizeBackendApiExtraction(backendExtraction, domExtraction, conversationId) {
  const nodeFresh = Boolean(backendExtraction?.node_fresh);
  const text = nodeFresh ? String(backendExtraction?.text ?? "") : "";
  return {
    ...backendExtraction,
    method: "backend_api",
    text,
    is_generating: !nodeFresh || Boolean(backendExtraction?.is_generating),
    node_fresh: nodeFresh,
    conversation_id: conversationId,
    assistant_count: backendExtraction?.assistant_count ?? domExtraction?.assistant_count ?? 0,
    user_count: backendExtraction?.user_count ?? domExtraction?.user_count ?? 0,
    preceding_user_count: backendExtraction?.preceding_user_count ?? domExtraction?.preceding_user_count,
    turn_index: backendExtraction?.turn_index ?? domExtraction?.turn_index ?? -1,
    copy_button_count: backendExtraction?.copy_button_count ?? domExtraction?.copy_button_count ?? 0,
    has_copy_button: Boolean(backendExtraction?.has_copy_button),
    diagnostics: backendExtraction?.diagnostics ?? domExtraction?.diagnostics
  };
}

async function recoverContentScriptJob(job, error, options = {}) {
  const existing = contentScriptRecoveries.get(job.job_id);
  if (existing && existing.settled === "pending") {
    return existing;
  }
  if (existing && existing.settled !== "fulfilled") {
    return existing;
  }
  if (existing && existing.settled === "fulfilled" && !options.restoredFromBfcache) {
    return existing;
  }
  return trackContentScriptRecovery(job.job_id, recoverContentScriptJobOnce(job, error, options));
}

function trackContentScriptRecovery(jobId, promise) {
  const tracked = promise.then(
    (value) => {
      tracked.settled = "fulfilled";
      return value;
    },
    (error) => {
      tracked.settled = "rejected";
      throw error;
    }
  );
  tracked.settled = "pending";
  contentScriptRecoveries.set(jobId, tracked);
  return tracked;
}

function forgetSettledSuccessfulRecovery(jobId) {
  const recovery = contentScriptRecoveries.get(jobId);
  if (recovery?.settled === "fulfilled") {
    contentScriptRecoveries.delete(jobId);
  }
}

function forgetContentScriptRecovery(jobId) {
  if (!jobId) {
    return;
  }
  contentScriptRecoveries.delete(jobId);
  const gate = suspensionGates.get(jobId);
  if (gate) {
    gate.reject(commandError("extension_error", "job terminated while parked for pageshow", {
      phase: "wait_response",
      side_effect_started: true
    }));
  }
}

async function recoverContentScriptJobOnce(job, error, options = {}) {
  if (job.content_script_suspended_at && !options.restoredFromBfcache) {
    await parkForPageshow(job);
    if (!jobs.has(job.job_id)
        || TERMINAL_STATUSES.has(job.status)
        || cancellationIsPending(job)) {
      return;
    }
  }
  job.content_script_recovery_incidents = Number(job.content_script_recovery_incidents ?? 0) + 1;
  job.content_script_recovery_in_progress = true;
  job.content_script_suspended_at = null;
  job.updated_at = Date.now();
  await persistJob(job);
  postNative(progress(job, "content_script_recovering", {
    reason: String(error?.message ?? error),
    source: options.source ?? "command_error",
    restored_from_bfcache: Boolean(options.restoredFromBfcache)
  }));
  const contentScriptProbe = await waitForContentScript(job.tab_id, adapterForJob(job), {
    phase: phaseForStatus(job.status) ?? "wait_response",
    side_effect_started: Boolean(job.status !== "selecting_model" && job.status !== "opening_tab"),
    send_committed: Boolean(job.send_committed)
  });
  recordContentScriptContract(job, contentScriptProbe);
  const rebound = await sendToTab(job.tab_id, { type: "yoetz_bind_job", job });
  job.content_script_recovery_in_progress = false;
  job.updated_at = Date.now();
  await persistJob(job);
  postNative(progress(job, "content_script_recovered", {
    url: rebound?.url ?? null,
    title: rebound?.title ?? null,
    source: options.source ?? "command_error",
    restored_from_bfcache: Boolean(options.restoredFromBfcache)
  }));
}

function remainingResponseDeadlineMs(job) {
  const startedAt = Number(job.response_wait_started_at) || Date.now();
  return Math.max(0, startedAt + responseWaitTimeoutMs(job) - Date.now());
}

function parkedDeadlineError(job) {
  return commandError(
    "response_timeout",
    "owned tab stayed in bfcache past the response deadline",
    {
      phase: "wait_response",
      side_effect_started: true,
      inspect_command: inspectCommandForJob(job)
    }
  );
}

async function failParkedJobAtDeadline(job) {
  if (!jobs.has(job.job_id) || TERMINAL_STATUSES.has(job.status)) {
    return;
  }
  const inspectCommand = inspectCommandForJob(job);
  const adapter = adapterForJob(job);
  await failJob(
    job,
    "response_timeout",
    `${adapter.displayName} response wait expired while the owned tab stayed in bfcache with no persisted pageshow. The owned tab is left open; inspect it before rerunning with: ${inspectCommand}`,
    {
      phase: "wait_response",
      side_effect_started: true,
      completion_reason: "timeout",
      send_committed: true,
      inspect_command: inspectCommand
    }
  );
}

async function parkForPageshow(job) {
  const remainingMs = remainingResponseDeadlineMs(job);
  postNative(progress(job, "parked_for_pageshow", {
    inspect_command: inspectCommandForJob(job),
    remaining_ms: remainingMs,
    tab_id: job.tab_id,
    message: "owned background tab is in bfcache; parked until persisted pageshow or response deadline"
  }));
  if (remainingMs <= 0) {
    await failParkedJobAtDeadline(job);
    throw parkedDeadlineError(job);
  }
  await waitForSuspensionGate(job, remainingMs);
}

function waitForSuspensionGate(job, remainingMs) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      const current = suspensionGates.get(job.job_id);
      if (current?.timer === timer) {
        suspensionGates.delete(job.job_id);
      }
      failParkedJobAtDeadline(job).then(
        () => reject(parkedDeadlineError(job)),
        reject
      );
    }, remainingMs);
    suspensionGates.set(job.job_id, {
      resolve: () => {
        clearTimeout(timer);
        suspensionGates.delete(job.job_id);
        resolve();
      },
      reject: (error) => {
        clearTimeout(timer);
        suspensionGates.delete(job.job_id);
        reject(error);
      },
      timer
    });
  });
}

function openSuspensionGate(job) {
  const gate = suspensionGates.get(job.job_id);
  if (!gate) {
    return false;
  }
  gate.resolve();
  return true;
}

function isRecoverableContentScriptError(error) {
  if (error?.code === "content_script_contract_mismatch") {
    return true;
  }
  const message = String(error?.message ?? error);
  return /Could not establish connection|Receiving end does not exist|Extension context invalidated|message (?:port|channel)(?: is)? closed|A listener indicated an asynchronous response.*channel closed|is not active in this tab/i.test(message);
}

function isPostSendExtraction(job, extraction) {
  if (!extraction || extraction.method === "page_text_fallback") {
    return false;
  }
  if (adapterForJob(job).completion.isFreshBackendApiExtraction(extraction)) {
    return true;
  }
  return isPostSendAssistantActivity(job, extraction);
}

function responseFinalityStallSignature(extraction) {
  return JSON.stringify([
    extraction.text,
    extraction.assistant_count,
    extraction.turn_index,
    extraction.method,
    extraction.assistant_identity
  ]);
}

function isClaudeFinalityConflict(job, extraction) {
  return adapterForJob(job).recipe === "claude"
    && isPostSendExtraction(job, extraction)
    && extraction.method === "assistant_dom"
    && Boolean(extraction.text)
    && typeof extraction.assistant_identity === "string"
    && Boolean(extraction.assistant_identity.trim())
    && extraction.is_generating === true
    && extraction.diagnostics?.finality?.last_turn_streaming === "false"
    && Number(extraction.diagnostics?.counts?.stop_controls ?? 0) > 0;
}

function isPostSendAssistantActivity(job, extraction, allowUnknownTurnIndex = false) {
  if (!extraction) {
    return false;
  }
  if (adapterForJob(job).completion.isFreshBackendApiExtraction(extraction)) {
    return true;
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

async function refreshFrozenRender(job, extraction, stableForMs, continuationEpoch = job?.continuation_epoch) {
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const conversationId = conversationIdForJob(job, extraction);
  const adapter = adapterForJob(job);
  const url = adapter.conversationJobUrl(conversationId, job.run_id);
  job.render_refresh_attempts = Number(job.render_refresh_attempts ?? 0) + 1;
  job.updated_at = Date.now();
  await persistJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  postNative(progress(job, "render_refreshing", {
    tab_id: job.tab_id,
    conversation_id: conversationId,
    conversation_url: url,
    attempt: job.render_refresh_attempts,
    max_attempts: MAX_RENDER_REFRESH_ATTEMPTS,
    stable_for_ms: stableForMs,
    response_length: extraction?.text?.length ?? 0,
    extraction_method: extraction?.method ?? "none",
    message: `refreshing owned ${adapter.displayName} conversation render after idle short response stayed frozen for ${formatDurationForMessage(stableForMs)}`
  }));
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  await chrome.tabs.update(job.tab_id, { url, active: false });
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  await waitForSiteTab(job.tab_id, adapter);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const contentScriptProbe = await waitForContentScript(job.tab_id, adapter, {
    phase: "wait_response",
    side_effect_started: true,
    send_committed: true
  });
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  recordContentScriptContract(job, contentScriptProbe);
  await persistJob(job);
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  const rebound = await sendToTab(job.tab_id, { type: "yoetz_bind_job", job });
  if (!jobContinuationIsLive(job, continuationEpoch)) {
    return;
  }
  postNative(progress(job, "render_refreshed", {
    tab_id: job.tab_id,
    conversation_id: conversationId,
    conversation_url: url,
    attempt: job.render_refresh_attempts,
    url: rebound?.url ?? null,
    title: rebound?.title ?? null,
    message: `owned ${adapter.displayName} conversation render refreshed; continuing final-response polling`
  }));
}

function responseStableIdleThresholdMs(intervalMs) {
  const interval = Math.max(0, Number(intervalMs) || 0);
  return Math.min(
    MAX_FINAL_AFFORDANCE_IDLE_MS,
    Math.max(MIN_STABLE_IDLE_MS, interval * STABLE_IDLE_INTERVAL_MULTIPLIER)
  );
}

function nativeEnvelopeByteLength(message) {
  const json = JSON.stringify(message);
  if (typeof TextEncoder !== "undefined") {
    return new TextEncoder().encode(json).byteLength;
  }
  return json.length;
}

function enforceMessageCapability(message) {
  if (!message?.job_id || message.type === "terminal_ack") {
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

function assertMessageOwnsJob(message, job) {
  const messageRunId = message?.run_id ?? null;
  const jobRunId = job?.run_id ?? null;
  const messageWorkspaceId = message?.workspace_id ?? null;
  const jobWorkspaceId = job?.workspace_id ?? null;
  if (
    message?.job_id !== job?.job_id
    || messageRunId !== jobRunId
    || messageWorkspaceId !== jobWorkspaceId
  ) {
    throw commandError(
      "run_mismatch",
      "message identity does not match active job " + (job?.job_id ?? "unknown"),
      {
        phase: "profile",
        side_effect_started: false,
        expected_job_id: job?.job_id ?? null,
        expected_run_id: jobRunId,
        received_run_id: messageRunId,
        expected_workspace_id: jobWorkspaceId,
        received_workspace_id: messageWorkspaceId
      }
    );
  }
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

function assertSubmittedConversationCurrent(job, sendResult) {
  const expectedConversationId = job.expected_conversation_id ?? job.conversation_id;
  if (!expectedConversationId) {
    return;
  }
  const currentConversationId = sendResult?.conversation_id ?? null;
  if (currentConversationId === expectedConversationId) {
    return;
  }
  throw commandError(
    "conversation_changed",
    `job ${job.job_id} sent in ${adapterForJob(job).displayName} conversation ${currentConversationId ?? "(none)"} instead of ${expectedConversationId}`,
    {
      phase: "send",
      side_effect_started: true,
      requested_conversation_id: expectedConversationId,
      current_conversation_id: currentConversationId,
      current_url: job.submitted_url ?? null
    }
  );
}

function reconcileJobConversationCurrent(job, extraction) {
  const expectedConversationId = job.expected_conversation_id ?? job.submitted_conversation_id ?? job.conversation_id;
  if (!expectedConversationId) {
    return false;
  }
  const currentConversationId = extraction?.conversation_id ?? null;
  if (expectedConversationId === currentConversationId) {
    return false;
  }
  const currentUrl = extraction?.url ?? null;
  if (adapterForJob(job).isExpectedConversationIdAssignment?.(
    job,
    expectedConversationId,
    currentConversationId
  )) {
    job.submitted_conversation_id = currentConversationId;
    job.submitted_url = currentUrl;
    job.updated_at = Date.now();
    return true;
  }
  throw commandError(
    "conversation_changed",
    `job ${job.job_id} moved from ${adapterForJob(job).displayName} conversation ${expectedConversationId} to ${currentConversationId ?? "(none)"}`,
    {
      phase: "wait_response",
      side_effect_started: true,
      requested_conversation_id: expectedConversationId,
      current_conversation_id: currentConversationId
    }
  );
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
  for (const [key, value] of Object.entries(detail)) {
    if (!(key in error) && value !== undefined) {
      error[key] = value;
    }
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

const TERMINAL_ENVELOPE_TYPES = new Set(["job_complete", "job_error", "job_cancel"]);

function isTerminalEnvelope(envelope) {
  return TERMINAL_ENVELOPE_TYPES.has(envelope?.type);
}

function terminalStatusForEnvelope(envelope) {
  switch (envelope?.type) {
    case "job_complete":
      return "complete";
    case "job_cancel":
      return "cancelled";
    default:
      return "failed";
  }
}

async function postTerminalMessage(message, envelope, { status, phase } = {}) {
  if (!message?.job_id) {
    return postNative(envelope);
  }
  const existing = jobs.get(message.job_id);
  if (existing?.terminal_envelope && !existing.terminal_delivered_at) {
    return postTerminalJob(existing, existing.terminal_envelope, {
      status: existing.status,
      phase: phaseForStatus(existing.status) ?? phase ?? "profile"
    });
  }
  if (existing && !TERMINAL_STATUSES.has(existing.status)) {
    const rejectionCode = envelope?.payload?.code;
    if (rejectionCode === "capability_mismatch" || rejectionCode === "run_mismatch") {
      // The malformed request cannot be routed on its received identity. Fail
      // the owning job using the durable owner identity so the native client
      // receives a terminal response and the rejection is not replay-poisoned.
      const ownedError = errorEnvelope(
        existing,
        rejectionCode,
        envelope?.payload?.message ?? "request identity does not match the active job",
        {
          request_id: envelope?.request_id ?? message?.request_id,
          phase: phase ?? "profile",
          side_effect_started: false,
          received_run_id: message?.run_id ?? null,
          received_workspace_id: message?.workspace_id ?? null
        }
      );
      return postTerminalJob(existing, ownedError, {
        status: "failed",
        phase: phase ?? "profile"
      });
    }
    // A live job owns this route. Do not replace it with an unrelated terminal
    // response that could later be ACKed as the live job's terminal.
    return postNative(progress(existing, "request_rejected", {
      request_id: envelope?.request_id ?? message?.request_id,
      code: envelope?.payload?.code ?? "request_rejected",
      message: envelope?.payload?.message ?? "request rejected while the job is active"
    }));
  }
  if (existing?.terminal_delivered_at) {
    return false;
  }
  const job = {
    ...messageJob(message),
    status: status ?? terminalStatusForEnvelope(envelope),
    started_at: Date.now(),
    updated_at: Date.now(),
    connection_generation: connectionGeneration
  };
  jobs.set(job.job_id, job);
  return postTerminalJob(job, envelope, {
    status: job.status,
    phase: phase ?? phaseForStatus(job.status) ?? "profile"
  });
}

async function postTerminalJob(job, envelope, { status, phase } = {}) {
  if (!isTerminalEnvelope(envelope)) {
    return postNative(envelope);
  }
  if (!job?.job_id) {
    return postNative(envelope);
  }
  if (cancellationIsPending(job) && envelope.type !== "job_cancel") {
    return false;
  }
  if (job.terminal_delivered_at) {
    return false;
  }
  if (job.terminal_envelope && job.terminal_envelope !== envelope) {
    envelope = job.terminal_envelope;
  }
  envelope = boundedTerminalEnvelope(job, envelope, phase ?? phaseForStatus(status) ?? "upload");
  const envelopeStatus = terminalStatusForEnvelope(envelope);
  job.status = status && !(status === "complete" && envelopeStatus !== "complete")
    ? status
    : envelopeStatus;
  job.terminal_type = envelope.type;
  job.terminal_envelope = envelope;
  stampTerminalSequence(job, envelope);
  job.terminal_delivered_at = null;
  job.updated_at = Date.now();
  rememberTerminalJob(job.job_id);
  jobs.set(job.job_id, job);
  const persisted = await persistTerminalJobBestEffort(job);
  if (!persisted) {
    job.terminal_persistence_failed = true;
    await recordTerminalDeliveryLost(job, phase ?? phaseForStatus(job.status) ?? "upload");
    return false;
  }
  if (!postNative(envelope)) {
    await recordTerminalDeliveryLost(job, phase ?? phaseForStatus(job.status) ?? "upload");
    return false;
  }
  job.terminal_last_post_at = Date.now();
  job.terminal_last_post_generation = connectionGeneration;
  job.terminal_persistence_failed = false;
  return true;
}

async function failJob(job, code, message, detail = {}) {
  // Cancellation owns the terminal outcome once its in-memory fence is set.
  // Do not mutate status or replace the cancellation envelope from a poller
  // error that resumes after the cancellation request.
  if (job && cancellationIsPending(job)) {
    return;
  }
  if (job && TERMINAL_STATUSES.has(job.status)) {
    return;
  }
  const { terminal_status: terminalStatus, ...payloadDetail } = detail;
  if (job?.tab_id) {
    // "kept" means Yoetz did not close the tab on this failure path. It does
    // not assert that the user has not already closed the tab independently.
    payloadDetail.tab_disposition = "kept";
  }
  if (!job) {
    await postTerminalMessage(null, errorEnvelope(job, code, message, payloadDetail));
    return;
  }
  job.status = terminalStatus ?? "failed";
  forgetContentScriptRecovery(job.job_id);
  rememberTerminalJob(job.job_id);
  job.updated_at = Date.now();
  chunks.discard(job.job_id);
  job.terminal_envelope = errorEnvelope(job, code, message, payloadDetail);
  await postTerminalJob(job, job.terminal_envelope, {
    status: job.status,
    phase: payloadDetail.phase ?? phaseForStatus(job.status) ?? "upload"
  });
}

function boundedTerminalEnvelope(job, envelope, phase) {
  stampTerminalSequence(job, envelope);
  const bytes = nativeEnvelopeByteLength(envelope);
  if (bytes <= MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES) {
    return envelope;
  }
  const replacement = errorEnvelope(job, "terminal_payload_too_large", "terminal payload exceeded the durable replay limit; inspect the owned tab before rerunning", {
    phase,
    side_effect_started: Boolean(job?.tab_id || job?.send_committed),
    original_terminal_type: envelope?.type ?? null,
    original_terminal_bytes: bytes,
    max_persisted_terminal_envelope_bytes: MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES
  });
  stampTerminalSequence(job, replacement);
  return replacement;
}

async function recordTerminalDeliveryLost(job, phase) {
  if (!job?.job_id) {
    return;
  }
  const deliveryPhase = phase ?? phaseForStatus(job.status) ?? "upload";
  if (!job.terminal_envelope) {
    let inspectCommand = null;
    let displayName = "Yoetz";
    if (job.recipe) {
      try {
        displayName = adapterForJob(job).displayName;
        inspectCommand = job.run_id ? inspectCommandForJob(job) : null;
      } catch {
        // Keep the fallback terminal bounded even when the original recipe is invalid.
      }
    }
    const message = `${displayName} terminal delivery was lost during ${deliveryPhase}${inspectCommand ? `; inspect with: ${inspectCommand}` : ""}`;
    job.terminal_envelope = errorEnvelope(job, "terminal_delivery_lost", message, {
      phase: deliveryPhase,
      side_effect_started: Boolean(job.tab_id || job.send_committed),
      terminal_delivery_lost: true,
      inspect_command: inspectCommand ?? undefined
    });
    stampTerminalSequence(job, job.terminal_envelope);
    job.terminal_delivered_at = null;
  }
  job.terminal_envelope = boundedTerminalEnvelope(job, job.terminal_envelope, deliveryPhase);
  const successfulCompletion = job.status === "complete"
    && job.terminal_envelope?.type === "job_complete";
  if (!successfulCompletion) {
    job.status = "terminal_delivery_lost";
  } else {
    job.terminal_delivery_lost = true;
  }
  forgetContentScriptRecovery(job.job_id);
  job.delivery_lost_phase = deliveryPhase;
  job.updated_at = Date.now();
  rememberTerminalJob(job.job_id);
  jobs.set(job.job_id, job);
  await persistTerminalJobBestEffort(job);
}

async function persistJob(job) {
  if (!job?.job_id) {
    return;
  }
  // Shard by job_id so concurrent jobs no longer fight over a single { jobs: {...} }
  // read-modify-write. Each job owns its own key and only rewrites itself; lost
  // updates from interleaved persists are no longer possible.
  await chrome.storage.session.set({
    [jobsStorageKey(job.job_id)]: TERMINAL_STATUSES.has(job.status)
      ? terminalJobForStorage(job)
      : strippedJobForStorage(job)
  });
}

async function persistJobBestEffort(job) {
  try {
    await persistJob(job);
    return true;
  } catch (error) {
    console.warn(`could not persist job ${job?.job_id ?? "unknown"}: ${String(error?.message ?? error)}`);
    return false;
  }
}

async function persistCancelPending(job) {
  if (!job?.job_id || !chrome.storage.local?.set) {
    return false;
  }
  const value = cancellationJobForStorage(job);
  try {
    await chrome.storage.local.set({ [cancelPendingStorageKey(job.job_id)]: value });
    try {
      await chrome.storage.session?.set?.({ [jobsStorageKey(job.job_id)]: value });
    } catch (error) {
      console.warn(
        "could not mirror cancellation intent for "
          + job.job_id
          + ": "
          + String(error?.message ?? error)
      );
    }
    return true;
  } catch (error) {
    console.warn(`could not persist cancellation intent for ${job.job_id}: ${String(error?.message ?? error)}`);
    return false;
  }
}

async function persistTerminalJobBestEffort(job) {
  const terminalRecord = terminalJobForStorage(job);
  if (job?.terminal_envelope && !terminalRecord.terminal_envelope) {
    console.warn(`could not persist terminal job ${job.job_id ?? "unknown"}: terminal envelope exceeds the persisted size limit`);
    return false;
  }
  if (!globalThis.chrome?.storage?.local?.set) {
    console.warn(`could not persist terminal job ${job?.job_id ?? "unknown"}: chrome.storage.local is unavailable`);
    return false;
  }
  try {
    // The local outbox is the durable source of truth. The session shard is a
    // fast mirror for status/inspection and is intentionally written second.
    await chrome.storage.local.set({
      [terminalOutboxStorageKey(job.job_id)]: terminalRecord
    });
    try {
      await chrome.storage.session?.set?.({
        [jobsStorageKey(job.job_id)]: terminalRecord
      });
    } catch (error) {
      console.warn(
        "could not mirror terminal job "
          + (job?.job_id ?? "unknown")
          + ": "
          + String(error?.message ?? error)
      );
    }
    return true;
  } catch (error) {
    console.warn(`could not persist terminal job ${job?.job_id ?? "unknown"}: ${String(error?.message ?? error)}`);
    return false;
  }
}

async function retryPendingTerminalJobs(jobId = null) {
  if (!nativePort) {
    return;
  }
  const now = Date.now();
  for (const job of jobs.values()) {
    if (jobId && job.job_id !== jobId) {
      continue;
    }
    if (!TERMINAL_STATUSES.has(job.status) || job.terminal_delivered_at) {
      continue;
    }
    if (!job.terminal_envelope
        && (job.status === "terminal_delivery_lost"
          || job.terminal_persistence_failed === true
          || Boolean(job.delivery_lost_phase))) {
      await recordTerminalDeliveryLost(job, phaseForStatus(job.status) ?? "wait_response");
    }
    if (!job.terminal_envelope) {
      continue;
    }
    const sameConnection = job.terminal_last_post_generation === connectionGeneration;
    if (sameConnection
        && Number.isSafeInteger(job.terminal_last_post_at)
        && now - job.terminal_last_post_at < TERMINAL_RETRY_INTERVAL_MS) {
      continue;
    }
    const delivered = await postTerminalJob(job, job.terminal_envelope, {
      status: job.status,
      phase: phaseForStatus(job.status) ?? "wait_response"
    });
    if (!delivered && !nativePort) {
      return;
    }
  }
}

function jobsStorageKey(jobId) {
  return `${JOBS_KEY_PREFIX}${jobId}`;
}

function terminalOutboxStorageKey(jobId) {
  return `${TERMINAL_OUTBOX_KEY_PREFIX}${jobId}`;
}

function terminalAckStorageKey(jobId) {
  return `${TERMINAL_ACK_KEY_PREFIX}${jobId}`;
}

function cancelPendingStorageKey(jobId) {
  return `${CANCEL_PENDING_KEY_PREFIX}${jobId}`;
}

function terminalAgeStamp(job) {
  for (const value of [job?.terminal_at, job?.updated_at, job?.started_at]) {
    if (Number.isSafeInteger(value) && value > 0) {
      return value;
    }
  }
  return null;
}

function isExpiredTerminalJob(job) {
  if (TERMINAL_STATUSES.has(job?.status) && !job?.terminal_delivered_at) {
    return false;
  }
  const stamp = terminalAgeStamp(job);
  return stamp != null && Date.now() - stamp > JOB_TTL_MS;
}

async function removeJobShard(jobId) {
  if (!jobId || !chrome.storage.session.remove) {
    return;
  }
  try {
    await chrome.storage.session.remove(jobsStorageKey(jobId));
  } catch (error) {
    console.warn(`could not remove expired job ${jobId}: ${String(error?.message ?? error)}`);
  }
}

async function removeTerminalOutbox(jobId) {
  if (!jobId || !chrome.storage.local?.remove) {
    return false;
  }
  try {
    await chrome.storage.local.remove(terminalOutboxStorageKey(jobId));
    return true;
  } catch (error) {
    console.warn(`could not remove acknowledged terminal outbox ${jobId}: ${String(error?.message ?? error)}`);
    return false;
  }
}

async function removeCancelPending(jobId) {
  if (!jobId || !chrome.storage.local?.remove) {
    return false;
  }
  try {
    await chrome.storage.local.remove(cancelPendingStorageKey(jobId));
    return true;
  } catch (error) {
    console.warn(`could not remove cancellation intent ${jobId}: ${String(error?.message ?? error)}`);
    return false;
  }
}

function stampTerminalSequence(job, envelope) {
  if (!job || !envelope || typeof envelope !== "object") {
    return envelope;
  }
  if (job.terminal_sequence == null) {
    job.terminal_sequence = 0;
  }
  if (job.terminal_at == null) {
    job.terminal_at = Date.now();
  }
  if (!envelope.payload || typeof envelope.payload !== "object" || Array.isArray(envelope.payload)) {
    envelope.payload = {};
  }
  if (envelope.payload.sequence == null) {
    envelope.payload.sequence = job.terminal_sequence;
  }
  return envelope;
}

function expectedTerminalSequence(job) {
  if (Number.isSafeInteger(job?.terminal_sequence) && job.terminal_sequence >= 0) {
    return job.terminal_sequence;
  }
  const stamped = job?.terminal_envelope?.payload?.sequence;
  if (Number.isSafeInteger(stamped) && stamped >= 0) {
    return stamped;
  }
  return 0;
}

function inboundAckSequence(message) {
  const sequence = message?.payload?.sequence;
  if (sequence == null) {
    return 0;
  }
  if (!Number.isSafeInteger(sequence) || sequence < 0) {
    return null;
  }
  return sequence;
}

async function persistTerminalAck(job, sequence, terminalType) {
  if (!chrome.storage.local?.set || !job?.job_id || !terminalType) {
    return false;
  }
  const tombstone = {
    job_id: job.job_id,
    run_id: job.run_id ?? null,
    workspace_id: job.workspace_id ?? null,
    recipe: job.recipe ?? null,
    tab_id: job.tab_id ?? null,
    close_tab_on_complete: job.close_tab_on_complete === true,
    ownership_nonce: job.ownership_nonce ?? null,
    conversation_id: job.conversation_id ?? null,
    expected_conversation_id: job.expected_conversation_id ?? null,
    submitted_conversation_id: job.submitted_conversation_id ?? null,
    status: job.status ?? terminalStatusForEnvelope({ type: terminalType }),
      terminal_type: terminalType,
    sequence,
    acknowledged_at: Date.now(),
    cleanup_pending: job.close_tab_on_complete === true
      && Boolean(job.tab_id)
      && (job.status === "complete" || terminalType === "job_complete")
      ? "tab_close"
      : "records"
  };
  try {
    await chrome.storage.local.set({ [terminalAckStorageKey(job.job_id)]: tombstone });
    return true;
  } catch (error) {
    console.warn(`could not commit terminal ACK for ${job.job_id}: ${String(error?.message ?? error)}`);
    return false;
  }
}

async function persistTerminalAckCleanup(jobId, phase) {
  if (!chrome.storage.local?.get || !chrome.storage.local?.set || !jobId) {
    return false;
  }
  const key = terminalAckStorageKey(jobId);
  try {
    const stored = (await chrome.storage.local.get(key))?.[key];
    if (!stored || typeof stored !== "object") {
      return false;
    }
    await chrome.storage.local.set({ [key]: { ...stored, cleanup_pending: phase } });
    return true;
  } catch (error) {
    console.warn(`could not advance terminal ACK cleanup for ${jobId}: ${String(error?.message ?? error)}`);
    return false;
  }
}

async function loadJobForTerminalAck(jobId) {
  const live = jobs.get(jobId);
  if (live) {
    return live;
  }
  const key = jobsStorageKey(jobId);
  if (chrome.storage.local) {
    const durable = (await chrome.storage.local.get(terminalOutboxStorageKey(jobId)))?.[terminalOutboxStorageKey(jobId)];
    if (durable && typeof durable === "object") {
      return durable;
    }
  }
  const stored = (await chrome.storage.session.get(key))?.[key];
  if (stored && typeof stored === "object") {
    return stored;
  }
  return null;
}

async function handleTerminalAck(message) {
  const jobId = message?.job_id;
  if (!jobId || typeof jobId !== "string") {
    return;
  }
  const sequence = inboundAckSequence(message);
  if (sequence == null) {
    return;
  }
  const job = await loadJobForTerminalAck(jobId);
  if (!job) {
    return;
  }
  if (expectedTerminalSequence(job) !== sequence) {
    return;
  }
  const acknowledgedType = message?.payload?.terminal_type;
  const expectedType = job.terminal_envelope?.type
    ?? job.terminal_type
    ?? (job.terminal_envelope_too_large ? acknowledgedType : null);
  if (!expectedType || acknowledgedType !== expectedType) {
    return;
  }
  if (job.run_id != null && message.run_id !== job.run_id) {
    return;
  }
  if (job.workspace_id != null && message.workspace_id !== job.workspace_id) {
    return;
  }
  if (job.terminal_delivered_at) {
    return;
  }
  if (!await persistTerminalAck(job, sequence, acknowledgedType)) {
    return;
  }
  job.terminal_delivered_at = Date.now();
  job.updated_at = Date.now();
  rememberTerminalJob(jobId);
  await persistJobBestEffort(job);
  if (job.close_tab_on_complete
      && (job.status === "complete" || job.terminal_envelope?.type === "job_complete")) {
    await closeOwnedTabOnComplete(job);
  }
  await persistTerminalAckCleanup(jobId, "records");
  const outboxRemoved = await removeTerminalOutbox(jobId);
  const cancellationRemoved = await removeCancelPending(jobId);
  delete job.terminal_envelope;
  delete job.terminal_persistence_failed;
  const sessionPersisted = await persistJobBestEffort(job);
  if (outboxRemoved && cancellationRemoved && sessionPersisted
      && await persistTerminalAckCleanup(jobId, "done")) {
    jobs.delete(jobId);
    chunks.discard(jobId);
  }
}

const TERMINAL_STORAGE_FIELDS = Object.freeze([
  "job_id",
  "run_id",
  "workspace_id",
  "capability_token",
  "request_id",
  "recipe",
  "status",
  "tab_id",
  "ownership_nonce",
  "conversation_id",
  "expected_conversation_id",
  "submitted_conversation_id",
  "close_tab_on_complete",
  "tab_disposition",
  "tab_ownership_verified",
  "tab_ownership_error",
  "may_still_be_running",
  "send_committed",
  "started_at",
  "updated_at",
  "terminal_envelope",
  "terminal_type",
  "terminal_envelope_too_large",
  "terminal_sequence",
  "terminal_at",
  "terminal_delivered_at",
  "terminal_persistence_failed",
  "terminal_delivery_lost",
  "delivery_lost_phase",
  "terminal_last_post_at",
  "terminal_last_post_generation",
  "last_response_progress_length",
  "last_response_progress_tail"
]);

const CANCEL_PENDING_STORAGE_FIELDS = Object.freeze([
  "job_id",
  "run_id",
  "workspace_id",
  "capability_token",
  "request_id",
  "recipe",
  "status",
  "tab_id",
  "ownership_nonce",
  "conversation_id",
  "expected_conversation_id",
  "submitted_conversation_id",
  "close_tab_on_complete",
  "send_committed",
  "content_script_instance_id",
  "content_script_build",
  "content_script_recipe",
  "cancel_pending",
  "cancel_requested",
  "cancelled",
  "started_at",
  "updated_at"
]);

function storageRecordFromFields(job, fields) {
  const record = {};
  for (const field of fields) {
    if (job?.[field] !== undefined) {
      record[field] = job[field];
    }
  }
  if (record.terminal_envelope) {
    record.terminal_envelope = JSON.parse(JSON.stringify(record.terminal_envelope));
  }
  return record;
}

function terminalJobForStorage(job) {
  const record = storageRecordFromFields(job, TERMINAL_STORAGE_FIELDS);
  if (typeof job?.last_response_progress_text === "string" && job.last_response_progress_text.length > 0) {
    record.last_response_progress_length = job.last_response_progress_text.length;
    record.last_response_progress_tail = job.last_response_progress_text.slice(-RESPONSE_TEXT_PERSIST_TAIL);
  }
  if (
    record.terminal_envelope
    && nativeEnvelopeByteLength(record.terminal_envelope) > MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES
  ) {
    delete record.terminal_envelope;
    record.terminal_envelope_too_large = true;
  }
  return record;
}

function cancellationJobForStorage(job) {
  return storageRecordFromFields(job, CANCEL_PENDING_STORAGE_FIELDS);
}

// Build a JSON-cloneable, size-bounded view of a job for chrome.storage.session.
// last_response_progress_text on the live job holds the FULL streaming text so
// postResponseProgress can compute deltas against the previous tick — but that
// text can be multi-MB on long Pro responses, and persisting it on every status
// transition (or on failJob's error path) would chew through the 10MB session
// quota and risk masking the real failure with a quota throw. We persist only a
// bounded tail plus the length, which is enough to reconstruct progress context
// after a restart without bloating storage. Poller leases are process-local
// coordination and must never be durable: a restored shard that still carries
// poller_lease would make acquirePollerLease return null and wedge the job.
function strippedJobForStorage(job) {
  const {
    last_response_progress_text: fullText,
    poller_lease: _pollerLease,
    poller_lease_seq: _pollerLeaseSeq,
    terminal_last_post_at: _terminalLastPostAt,
    terminal_last_post_generation: _terminalLastPostGeneration,
    ...rest
  } = job;
  if (
    rest.terminal_envelope
    && nativeEnvelopeByteLength(rest.terminal_envelope) > MAX_PERSISTED_TERMINAL_ENVELOPE_BYTES
  ) {
    delete rest.terminal_envelope;
    rest.terminal_envelope_too_large = true;
  }
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
    if (TERMINAL_STATUSES.has(value.status) && !value.terminal_delivered_at) {
      continue;
    }
    const stamp = TERMINAL_STATUSES.has(value.status)
      ? terminalAgeStamp(value)
      : (value.updated_at ?? value.started_at ?? 0);
    if (stamp != null && stamp < cutoff) {
      expiredKeys.push(key);
    }
  }
  if (expiredKeys.length > 0 && chrome.storage.session.remove) {
    await chrome.storage.session.remove(expiredKeys);
  }
}

function handleNativeDisconnect(port = nativePort, generation = connectionGeneration) {
  if (nativePort !== port || connectionGeneration !== generation) {
    return;
  }
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
  const port = nativePort;
  const generation = connectionGeneration;
  if (!port) {
    return false;
  }
  try {
    port.postMessage(message);
    return true;
  } catch (error) {
    const detail = String(error?.message ?? error);
    if (nativePort !== port || connectionGeneration !== generation) {
      return false;
    }
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
    selecting_model: "model_selection",
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

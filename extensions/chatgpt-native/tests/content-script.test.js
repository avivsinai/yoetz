import assert from "node:assert/strict";
import test from "node:test";
import { uint8ArrayToBase64 } from "../src/chunks.js";

const chatgptBackendModuleUrl = new URL("../src/sites/chatgpt-backend.js", import.meta.url).href;
const chatgptDomModuleUrl = new URL("../src/chatgpt-dom.js", import.meta.url).href;
const helperModule = `import { fetchConversationAnswer } from ${JSON.stringify(chatgptBackendModuleUrl)};
import {
  classifyManualHandoff as classifyRealManualHandoff,
  classifyWaitManualHandoff as classifyRealWaitManualHandoff
} from ${JSON.stringify(chatgptDomModuleUrl)};
export { fetchConversationAnswer };
const hooks = globalThis.__contentScriptTestHooks;

export function ownedWindowName(job) {
  const base = \`yoetz-chatgpt-native:\${job.run_id}:\${job.job_id}\`;
  return job.workspace_id || job.ownership_nonce
    ? \`\${base}|\${encodeURIComponent(job.workspace_id ?? "")}|\${encodeURIComponent(job.ownership_nonce ?? "")}\`
    : base;
}

export function parseOwnedWindowName(value) {
  if (typeof value !== "string" || !value.startsWith("yoetz-chatgpt-native:")) {
    return null;
  }
  const [identity, workspaceId, ownershipNonce] = value
    .slice("yoetz-chatgpt-native:".length)
    .split("|");
  const separator = identity.lastIndexOf(":");
  if (separator <= 0 || separator === identity.length - 1) return null;
  const parsed = {
    run_id: identity.slice(0, separator),
    job_id: identity.slice(separator + 1)
  };
  if (workspaceId) parsed.workspace_id = decodeURIComponent(workspaceId);
  if (ownershipNonce) parsed.ownership_nonce = decodeURIComponent(ownershipNonce);
  return parsed;
}

export function getPageText() {
  return hooks.pageText ?? "";
}

export function findComposer() {
  return hooks.composer ?? null;
}

export function findAuthenticatedComposer() {
  return hooks.authenticatedComposer ?? null;
}

export function manualHandoffContext() {
  return hooks.manualHandoffContext ?? {
    authenticated: Boolean(hooks.authenticatedComposer),
    title: globalThis.document.title,
    text: hooks.pageText ?? ""
  };
}

export function classifyManualHandoff(input) {
  hooks.manualHandoffInputs.push(input);
  return hooks.manualHandoff === undefined
    ? classifyRealManualHandoff(input)
    : hooks.manualHandoff;
}

export function classifyWaitManualHandoff(input) {
  hooks.waitManualHandoffInputs.push(input);
  return hooks.waitManualHandoff === undefined
    ? classifyRealWaitManualHandoff(input)
    : hooks.waitManualHandoff;
}

export function classifyBlockingState() {
  return hooks.blockingState ?? null;
}

export async function ensureFreshChat(_document, job) {
  hooks.ensureFreshChatCalls.push(job);
  if (hooks.failFreshChat) {
    throw new Error(hooks.failFreshChat);
  }
  return { status: "fresh", pathname: globalThis.location.pathname };
}

export async function ensureChatSurface(_document, options) {
  hooks.ensureChatSurfaceCalls.push(options);
  return hooks.ensureChatSurfaceResult ?? { ok: true };
}

export function verifyChatSurface(_document, options) {
  hooks.verifyChatSurfaceCalls.push(options);
  return hooks.verifyChatSurfaceResult ?? { ok: true };
}

export function verifyChatgptModelSelectionBeforeSend(_document, selection) {
  hooks.verifyChatgptModelSelectionCalls.push(selection);
  return hooks.verifyChatgptModelSelectionResult ?? {
    ok: true,
    surface_evidence_seen: selection?.surface_evidence_seen === true,
    surface_state: null,
    surface_observed_values: []
  };
}

export async function ensureConversationLoaded(_document, conversationId, options) {
  hooks.ensureConversationLoadedCalls.push({ conversationId, options });
  const actual = conversationIdFromLocation();
  if (actual !== conversationId) {
    const error = new Error(\`ChatGPT conversation \${conversationId} did not load\`);
    error.code = "conversation_not_loaded";
    error.phase = "upload";
    error.side_effect_started = false;
    throw error;
  }
  if (hooks.failConversationLoaded) {
    const error = new Error(hooks.failConversationLoaded.message);
    for (const [key, value] of Object.entries(hooks.failConversationLoaded)) {
      if (key !== "message") {
        error[key] = value;
      }
    }
    throw error;
  }
  hooks.afterEnsureConversationLoaded?.();
  return { status: "loaded", conversation_id: conversationId, pathname: globalThis.location.pathname };
}

export function markOwnership(_document, job) {
  hooks.markOwnershipCalls.push(job);
}

export async function uploadFile(_document, file, options) {
  hooks.uploadFileCalls.push({ file_name: file.name, size: file.size, options });
  if (hooks.uploadFileError) {
    throw hooks.uploadFileError;
  }
  return hooks.uploadFileResult ?? { upload_commit_signal: "send_enabled" };
}

export function configureModelState(_document, job) {
  hooks.configureModelCalls.push(job);
  hooks.events.push("configure_model");
  return hooks.configureModelResult ?? {
    status: "selected",
    model_used: "GPT-5.6 Sol Pro",
    requested_model: "gpt-5-6-sol-chat-pro",
    family_status: "verified",
    effort_status: "verified",
    surface_evidence_seen: Boolean(hooks.configureModelSurfaceEvidenceSeen)
  };
}

export function sendAcceptanceBaseline() {
  hooks.sendAcceptanceBaselineCalls += 1;
  return { user_count: 1, assistant_count: 2 };
}

export async function insertPrompt(_document, prompt, options) {
  hooks.events.push("insert_prompt");
  hooks.insertPromptCalls.push({ prompt, options });
  hooks.afterInsertPrompt?.();
}

export async function clickSend(_document, options) {
  hooks.clickSendCalls.push(options);
  await options.beforeClick?.();
  options.verifyBeforeClick?.();
  hooks.clickCommittedCalls += 1;
}

export async function waitForSendAccepted() {
  hooks.afterWaitForSendAccepted?.();
  return hooks.sendAccepted ?? { accepted: true };
}

export function extractResponse() {
  return hooks.extraction ?? { method: "assistant_dom_fallback", text: "answer", conversation_id: conversationIdFromLocation() };
}

export function modelSelectionDiagnostics() {
  return hooks.modelSelectionDiagnostics ?? {};
}

function conversationIdFromLocation() {
  const match = String(globalThis.location.pathname ?? "").match(/^\\/c\\/([^/?#]+)$/);
  return match ? decodeURIComponent(match[1]) : null;
}

const dom = {
  ownedWindowName,
  parseOwnedWindowName,
  getPageText,
  findComposer,
  findAuthenticatedComposer,
  manualHandoffContext,
  classifyManualHandoff,
  classifyWaitManualHandoff,
  classifyBlockingState,
  ensureFreshChat,
  ensureChatSurface,
    verifyChatSurface,
    verifyChatgptModelSelectionBeforeSend,
  ensureConversationLoaded,
  markOwnership,
  uploadFile,
  configureModelState,
  sendAcceptanceBaseline,
  insertPrompt,
  clickSend,
  waitForSendAccepted,
  extractResponse,
  modelSelectionDiagnostics
};

export const siteAdapter = {
  recipe: hooks.recipe ?? "chatgpt",
  displayName: "ChatGPT",
  homeUrl: "https://chatgpt.com/",
  dom,
  fetchConversationAnswer,
  conversationIdFromUrl(value) {
    try {
      const match = new URL(value).pathname.match(/^\\/c\\/([^/?#]+)$/);
      return match ? decodeURIComponent(match[1]) : null;
    } catch {
      return null;
    }
  },
  isExpectedConversationIdAssignment() {
    return Boolean(hooks.allowConversationAssignment);
  },
  isAcceptableModelSelection(selection) {
    return selection?.status === "selected"
      && selection?.requested_model === "gpt-5-6-sol-chat-pro"
      && selection?.family_status === "verified"
      && selection?.effort_status === "verified"
      && selection?.model_used === "GPT-5.6 Sol Pro";
  },
  isConversationUrl(value) {
    return Boolean(this.conversationIdFromUrl(value));
  },
  isAllowedTabUrl(value) {
    return String(value ?? "").startsWith("https://chatgpt.com/");
  }
};`;

test("content script resume path skips fresh enforcement and completes on requested conversation", async () => {
  const { send, hooks, restore } = await loadContentScript("resume_happy", "https://chatgpt.com/c/conv-123?_yoetz=run_resume");
  try {
    const job = resumeJob();

    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);
    assert.equal(prepared.payload.manual_handoff, null);
    assert.equal(prepared.payload.fresh_chat, null);
    assert.deepEqual(hooks.ensureFreshChatCalls, []);
    assert.deepEqual(hooks.ensureConversationLoadedCalls.map((call) => call.conversationId), ["conv-123"]);
    assert.equal(globalThis.window.name, "yoetz-chatgpt-native:run_resume:job_resume");

    const uploaded = await send({
      type: "yoetz_upload_file",
      job,
      file: {
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }
    });
    assert.equal(uploaded.ok, true);
    assert.equal(uploaded.payload.upload_commit_signal, "send_enabled");
    assert.equal(hooks.uploadFileCalls.length, 1);

    const configured = await send({ type: "yoetz_configure_model", job });
    assert.equal(configured.ok, true);

    const sent = await send({ type: "yoetz_send_prompt", job, prompt: "continue" });
    assert.equal(sent.ok, true);
    assert.equal(sent.payload.conversation_id, "conv-123");
    assert.equal(hooks.clickSendCalls[0].expectedConversationId, "conv-123");
    assert.equal(hooks.configureModelCalls.length, 2);
    assert.ok(hooks.events.indexOf("configure_model") < hooks.events.indexOf("insert_prompt"));
    assert.equal(hooks.verifyChatgptModelSelectionCalls.length, 1);

    const extracted = await send({ type: "yoetz_extract_response", job });
    assert.equal(extracted.ok, true);
    assert.equal(extracted.payload.conversation_id, "conv-123");
  } finally {
    restore();
  }
});

test("content script probe advertises the generic command contract for each recipe", async () => {
  const chatgpt = await loadContentScript("probe_chatgpt", "https://chatgpt.com/");
  try {
    const response = await chatgpt.send({ type: "yoetz_probe", recipe: "chatgpt" });
    assert.equal(response.ok, true);
    assert.equal(response.payload.recipe, "chatgpt");
    assert.deepEqual(response.payload.capabilities, [
      "native_job_commands_v1",
      "chatgpt_click_bound_send_receipt_v1"
    ]);
  } finally {
    chatgpt.restore();
  }

  const claude = await loadContentScript("probe_claude", "https://claude.ai/new");
  try {
    claude.hooks.recipe = "claude";
    const response = await claude.send({ type: "yoetz_probe", recipe: "claude" });
    assert.equal(response.ok, true);
    assert.equal(response.payload.recipe, "claude");
    assert.deepEqual(response.payload.capabilities, ["native_job_commands_v1"]);
  } finally {
    claude.restore();
  }
});

test("content script accepts the bound command envelope and rejects a stale injection nonce", async () => {
  const { send, hooks, restore } = await loadContentScript("secure_command_contract", "https://chatgpt.com/");
  try {
    const probe = await send({ type: "yoetz_probe", recipe: "chatgpt" });
    const job = {
      job_id: "job_secure",
      run_id: "run_secure",
      recipe: "chatgpt",
      upload_timeout_ms: 1000,
      send_timeout_ms: 1000
    };
    const contract = {
      content_script_instance_id: probe.payload.content_script_instance_id,
      content_script_build: probe.payload.content_script_build,
      content_script_recipe: "chatgpt",
      required_content_script_capabilities: probe.payload.capabilities
    };
    const prepared = await send({
      type: "yoetz_secure_command",
      command: "yoetz_prepare_job",
      content_script_contract: contract,
      payload: { type: "yoetz_prepare_job", job }
    });
    assert.equal(prepared.ok, true);

    const stale = await send({
      type: "yoetz_secure_command",
      command: "yoetz_upload_file",
      content_script_contract: {
        ...contract,
        content_script_instance_id: "cs_stale"
      },
      payload: {
        type: "yoetz_upload_file",
        job,
        file: {
          filename: "bundle.md",
          mime_type: "text/markdown",
          bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
        }
      }
    });
    assert.equal(stale.ok, false);
    assert.equal(stale.code, "content_script_contract_mismatch");
    assert.equal(stale.phase, "upload");
    assert.equal(stale.side_effect_started, false);
    assert.equal(hooks.uploadFileCalls.length, 0);
  } finally {
    restore();
  }
});

test("content script reports restored waiting-for-file bind failures before side effects", async () => {
  const { send, restore } = await loadContentScript("bind_waiting_for_file", "https://chatgpt.com/");
  try {
    const response = await send({
      type: "yoetz_bind_job",
      job: { ...resumeJob(), status: "waiting_for_file" }
    });
    assert.equal(response.ok, false);
    assert.equal(response.code, "ownership_lost");
    assert.equal(response.phase, "upload");
    assert.equal(response.side_effect_started, false);
  } finally {
    restore();
  }
});

test("content script preserves an attachment-stalled trace from the upload adapter", async () => {
  const { send, hooks, restore } = await loadContentScript("attachment_trace", "https://chatgpt.com/c/conv-123?_yoetz=run_resume");
  try {
    const job = resumeJob();
    const error = new Error("Claude attachment stalled");
    error.code = "attachment_stalled";
    error.phase = "upload";
    error.side_effect_started = true;
    error.attachment_trace = {
      final_chunk_ack_at_ms: 100,
      input_resolved_at_ms: 101,
      files_assigned_at_ms: 102,
      change_dispatched_at_ms: 103,
      hard_timeout_at_ms: 420000,
      hard_timeout_pending_legs: ["matching_thumbnail"]
    };
    hooks.uploadFileError = error;

    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);

    const response = await send({
      type: "yoetz_upload_file",
      job,
      file: {
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }
    });

    assert.equal(response.ok, false);
    assert.equal(response.code, "attachment_stalled");
    assert.deepEqual(response.attachment_trace, error.attachment_trace);
  } finally {
    restore();
  }
});

test("content script extends Claude attachment observation only when the stall window is opt in", async () => {
  const { send, hooks, restore } = await loadContentScript("claude_opt_in_stall", "https://chatgpt.com/c/conv-123?_yoetz=run_resume");
  try {
    hooks.recipe = "claude";
    const job = { ...resumeJob(), recipe: "claude" };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);

    const defaultUpload = await send({
      type: "yoetz_upload_file",
      job,
      file: {
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }
    });
    assert.equal(defaultUpload.ok, true);
    assert.equal(hooks.uploadFileCalls[0].options.stallTimeoutMs, undefined);

    const optedInUpload = await send({
      type: "yoetz_upload_file",
      job: { ...job, attachment_stall_timeout_ms: 1500 },
      file: {
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }
    });
    assert.equal(optedInUpload.ok, true);
    assert.equal(hooks.uploadFileCalls[1].options.stallTimeoutMs, 1500);
  } finally {
    restore();
  }
});

test("content script rejects an unavailable site adapter before DOM side effects", async () => {
  const { send, hooks, restore } = await loadContentScript("unsupported_recipe", "https://chatgpt.com/");
  try {
    const response = await send({
      type: "yoetz_prepare_job",
      job: { ...resumeJob(), recipe: "unknown" }
    });

    assert.equal(response.ok, false);
    assert.equal(response.code, "unsupported_recipe");
    assert.equal(response.phase, "profile");
    assert.equal(response.side_effect_started, false);
    assert.deepEqual(hooks.ensureFreshChatCalls, []);
    assert.deepEqual(hooks.ensureConversationLoadedCalls, []);
    assert.deepEqual(hooks.markOwnershipCalls, []);
  } finally {
    restore();
  }
});

test("content script auth probe reports manual handoff without job side effects", async () => {
  const { send, hooks, restore } = await loadContentScript("auth_probe_login", "https://chatgpt.com/auth/login");
  try {
    hooks.manualHandoff = {
      state: "login_required",
      message: "ChatGPT login required in this Chrome profile"
    };
    hooks.pageText = "Log in to ChatGPT";

    const response = await send({ type: "yoetz_auth_probe" });

    assert.equal(response.ok, true);
    assert.equal(response.payload.status, "login_required");
    assert.equal(response.payload.authenticated, false);
    assert.deepEqual(response.payload.manual_handoff, hooks.manualHandoff);
    assert.equal(response.payload.text_chars, "Log in to ChatGPT".length);
    assert.deepEqual(hooks.ensureFreshChatCalls, []);
    assert.deepEqual(hooks.ensureConversationLoadedCalls, []);
    assert.deepEqual(hooks.markOwnershipCalls, []);
  } finally {
    restore();
  }
});

test("content script excludes sidebar challenge text even when authentication is unknown", async () => {
  const { send, hooks, restore } = await loadContentScript(
    "auth_probe_scoped_text",
    "https://chatgpt.com/?_yoetz=run_current"
  );
  try {
    hooks.pageText = "New chat\nPre-execution security check\nAsk ChatGPT";
    hooks.manualHandoffContext = {
      authenticated: false,
      title: "",
      text: "Ask ChatGPT"
    };
    globalThis.document.title = "Pre-execution security check";
    const job = {
      job_id: "job_current",
      run_id: "run_current",
      upload_timeout_ms: 1000,
      send_timeout_ms: 1000
    };

    const prepared = await send({ type: "yoetz_prepare_job", job });
    const auth = await send({ type: "yoetz_auth_probe" });
    const extracted = await send({ type: "yoetz_extract_response", job });

    assert.equal(prepared.ok, true);
    assert.equal(prepared.payload.manual_handoff, null);
    assert.equal(prepared.payload.window_name, "yoetz-chatgpt-native:run_current:job_current");
    assert.equal(auth.ok, true);
    assert.equal(auth.payload.status, "authentication_unknown");
    assert.equal(auth.payload.authenticated, false);
    assert.equal(auth.payload.manual_handoff, null);
    assert.equal(extracted.ok, true);
    assert.equal(extracted.payload.manual_handoff, null);
    assert.deepEqual(hooks.markOwnershipCalls, [job]);
    assert.deepEqual(hooks.manualHandoffInputs.map((input) => input.text), ["Ask ChatGPT", "Ask ChatGPT"]);
    assert.equal(hooks.waitManualHandoffInputs[0].title, "");
  } finally {
    restore();
  }
});

test("content script does not treat a generic visible editor as authenticated ChatGPT", async () => {
  const { send, hooks, restore } = await loadContentScript(
    "auth_probe_generic_editor",
    "https://chatgpt.com/?_yoetz=run_challenge"
  );
  try {
    hooks.composer = {};
    hooks.pageText = "Security check";
    hooks.manualHandoffContext = {
      authenticated: false,
      title: "",
      text: "Security check"
    };
    const job = {
      job_id: "job_challenge",
      run_id: "run_challenge",
      upload_timeout_ms: 1000,
      send_timeout_ms: 1000
    };

    const prepared = await send({ type: "yoetz_prepare_job", job });
    const auth = await send({ type: "yoetz_auth_probe" });

    assert.equal(prepared.ok, true);
    assert.equal(prepared.payload.manual_handoff.state, "challenge_required");
    assert.equal(auth.ok, true);
    assert.equal(auth.payload.status, "challenge_required");
    assert.equal(auth.payload.authenticated, false);
    assert.deepEqual(hooks.markOwnershipCalls, []);
    assert.deepEqual(hooks.manualHandoffInputs.map((input) => input.text), ["Security check", "Security check"]);
  } finally {
    restore();
  }
});

test("content script reports unknown authentication without a strict composer or handoff", async () => {
  const { send, hooks, restore } = await loadContentScript(
    "auth_probe_unknown",
    "https://chatgpt.com/"
  );
  try {
    hooks.composer = {};
    hooks.pageText = "Welcome";
    hooks.manualHandoffContext = {
      authenticated: false,
      title: "",
      text: "Welcome"
    };

    const auth = await send({ type: "yoetz_auth_probe" });

    assert.equal(auth.ok, true);
    assert.equal(auth.payload.status, "authentication_unknown");
    assert.equal(auth.payload.authenticated, false);
    assert.equal(auth.payload.manual_handoff, null);
    assert.match(auth.payload.message, /composer is not visible/);
    assert.equal(Object.hasOwn(hooks.manualHandoffInputs[0], "authenticated"), false);
  } finally {
    restore();
  }
});

test("content script inspect labels model diagnostics as current chip state", async () => {
  const { send, hooks, restore } = await loadContentScript(
    "inspect_current_model_chip",
    "https://chatgpt.com/c/conv-123?_yoetz=run-inspect"
  );
  try {
    globalThis.window.name = "yoetz-chatgpt-native:run-inspect:job-inspect|workspace_test|nonce-inspect";
    hooks.modelSelectionDiagnostics = {
      modelChip: "GPT-5.6 Sol Pro",
      modelVerified: true
    };

    const response = await send({
      type: "yoetz_inspect_page",
      job_id: "job-inspect",
      run_id: "run-inspect",
      workspace_id: "workspace_test",
      ownership_nonce: "nonce-inspect",
      recipe: "chatgpt"
    });

    assert.equal(response.ok, true);
    assert.deepEqual(response.payload.current_model_chip_state, hooks.modelSelectionDiagnostics);
    assert.equal(response.payload.model_selection, undefined);
  } finally {
    restore();
  }
});

test("content script inspect rejects a conversation-only match without durable ownership", async () => {
  const { send, restore } = await loadContentScript(
    "inspect_unowned_conversation",
    "https://chatgpt.com/c/conv-123"
  );
  try {
    const response = await send({
      type: "yoetz_inspect_page",
      run_id: "conv-123",
      workspace_id: "workspace_test",
      recipe: "chatgpt"
    });

    assert.equal(response.ok, false);
    assert.equal(response.code, "run_mismatch");
  } finally {
    restore();
  }
});

test("content script resume prepare rejects a different conversation before send", async () => {
  const { send, restore } = await loadContentScript("resume_mismatch", "https://chatgpt.com/c/other?_yoetz=run_resume");
  try {
    const response = await send({ type: "yoetz_prepare_job", job: resumeJob() });

    assert.equal(response.ok, false);
    assert.equal(response.code, "conversation_not_loaded");
    assert.equal(response.phase, "upload");
    assert.equal(response.side_effect_started, false);
  } finally {
    restore();
  }
});

test("content script resume prepare preserves conversation unavailable details", async () => {
  const currentUrl = "https://chatgpt.com/c/conv-123?_yoetz=run_resume";
  const { send, hooks, restore } = await loadContentScript("resume_unavailable", currentUrl);
  try {
    hooks.failConversationLoaded = {
      message: "ChatGPT conversation conv-123 is unavailable",
      code: "conversation_unavailable",
      phase: "upload",
      side_effect_started: false,
      requested_conversation_id: "conv-123",
      current_url: currentUrl
    };

    const response = await send({ type: "yoetz_prepare_job", job: resumeJob() });

    assert.equal(response.ok, false);
    assert.equal(response.code, "conversation_unavailable");
    assert.equal(response.phase, "upload");
    assert.equal(response.side_effect_started, false);
    assert.equal(response.requested_conversation_id, "conv-123");
    assert.equal(response.current_url, currentUrl);
  } finally {
    restore();
  }
});

test("content script resume prepare passes job load timing into conversation loading", async () => {
  const { send, hooks, restore } = await loadContentScript("resume_timing", "https://chatgpt.com/c/conv-123?_yoetz=run_resume");
  try {
    const job = {
      ...resumeJob(),
      upload_timeout_ms: 4321,
      upload_interval_ms: 123
    };

    const prepared = await send({ type: "yoetz_prepare_job", job });

    assert.equal(prepared.ok, true);
    assert.equal(hooks.ensureConversationLoadedCalls.length, 1);
    assert.equal(hooks.ensureConversationLoadedCalls[0].conversationId, "conv-123");
    assert.equal(hooks.ensureConversationLoadedCalls[0].options.timeoutMs, 4321);
    assert.equal(hooks.ensureConversationLoadedCalls[0].options.intervalMs, 123);
  } finally {
    restore();
  }
});

test("content script resume prepare rejects an unowned resume URL marker", async () => {
  const { send, hooks, restore } = await loadContentScript("resume_wrong_marker", "https://chatgpt.com/c/conv-123?_yoetz=other_run");
  try {
    const response = await send({ type: "yoetz_prepare_job", job: resumeJob() });

    assert.equal(response.ok, false);
    assert.equal(response.code, "run_mismatch");
    assert.equal(response.phase, "upload");
    assert.equal(response.side_effect_started, false);
    assert.deepEqual(hooks.ensureConversationLoadedCalls, []);
    assert.deepEqual(hooks.markOwnershipCalls, []);
  } finally {
    restore();
  }
});

test("content script resume prepare rejects marker drift during conversation loading before ownership mark", async () => {
  const { send, hooks, restore, location } = await loadContentScript("resume_marker_drift_during_load", "https://chatgpt.com/c/conv-123?_yoetz=run_resume");
  try {
    hooks.afterEnsureConversationLoaded = () => {
      location.href = "https://chatgpt.com/c/conv-123?_yoetz=other_run";
    };

    const response = await send({ type: "yoetz_prepare_job", job: resumeJob() });

    assert.equal(response.ok, false);
    assert.equal(response.code, "run_mismatch");
    assert.equal(response.phase, "upload");
    assert.equal(response.side_effect_started, false);
    assert.equal(hooks.ensureConversationLoadedCalls.length, 1);
    assert.deepEqual(hooks.markOwnershipCalls, []);
  } finally {
    restore();
  }
});

test("content script resume follow-on commands reject conversation drift", async () => {
  const { send, restore, location } = await loadContentScript("resume_drift", "https://chatgpt.com/c/conv-123?_yoetz=run_resume");
  try {
    const job = resumeJob();
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);

    location.href = "https://chatgpt.com/c/other?_yoetz=run_resume";
    location.pathname = "/c/other";
    const response = await send({
      type: "yoetz_upload_file",
      job,
      file: {
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }
    });

    assert.equal(response.ok, false);
    assert.equal(response.code, "conversation_changed");
    assert.equal(response.phase, "upload");
    assert.equal(response.side_effect_started, false);
  } finally {
    restore();
  }
});

test("content script resume send rechecks conversation drift after prompt insertion before clicking send", async () => {
  const { send, hooks, restore, location } = await loadContentScript("resume_send_drift", "https://chatgpt.com/c/conv-123?_yoetz=run_resume");
  try {
    const job = resumeJob();
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);
    hooks.afterInsertPrompt = () => {
      location.href = "https://chatgpt.com/c/other?_yoetz=run_resume";
      location.pathname = "/c/other";
    };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "continue" });

    assert.equal(response.ok, false);
    assert.equal(response.code, "conversation_changed");
    assert.equal(response.phase, "send");
    assert.equal(response.side_effect_started, true);
    assert.equal(hooks.insertPromptCalls.length, 1);
    assert.equal(hooks.clickSendCalls.length, 0);
  } finally {
    restore();
  }
});

test("content script marks a fresh-job conversation drift after upload as send-side effect", async () => {
  const { send, hooks, restore, location } = await loadContentScript("fresh_send_drift", "https://chatgpt.com/?_yoetz=run_fresh_send");
  try {
    const job = {
      job_id: "job_fresh_send",
      run_id: "run_fresh_send",
      recipe: "chatgpt",
      upload_timeout_ms: 1000,
      send_timeout_ms: 1000
    };
    assert.equal((await send({ type: "yoetz_prepare_job", job })).ok, true);
    assert.equal((await send({
      type: "yoetz_upload_file",
      job,
      file: {
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }
    })).ok, true);
    hooks.afterInsertPrompt = () => {
      location.href = "https://chatgpt.com/c/other?_yoetz=run_fresh_send";
      location.pathname = "/c/other";
    };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "continue" });

    assert.equal(response.ok, false);
    assert.equal(response.code, "fresh_chat_lost");
    assert.equal(response.phase, "send");
    assert.equal(response.side_effect_started, true);
    assert.equal(hooks.clickSendCalls.length, 0);
  } finally {
    restore();
  }
});

test("content script rechecks the Chat surface immediately before sending", async () => {
  const { send, hooks, restore } = await loadContentScript("send_surface_guard", "https://chatgpt.com/?_yoetz=run_surface_guard");
  try {
    const job = { job_id: "job_surface_guard", run_id: "run_surface_guard", send_timeout_ms: 1000 };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);
    hooks.verifyChatgptModelSelectionResult = {
      ok: false,
      failure_reason: "chat_surface_selection_mismatch",
      surface_state: { aria_checked: "false", data_state: "unchecked" },
      surface_observed_values: ["chatgpt", "work"]
    };
    hooks.verifyChatSurfaceResult = {
      ok: false,
      failure_reason: "chat_surface_selection_mismatch",
      state: { aria_checked: "false", data_state: "unchecked" },
      observed_values: ["chatgpt", "work"]
    };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "review" });

    assert.equal(response.ok, false);
    assert.equal(response.code, "model_selection_not_verified_before_send");
    assert.equal(response.phase, "send");
    assert.equal(response.side_effect_started, true);
    assert.equal(response.send_committed, false);
    assert.equal(response.surface_failure_reason, "chat_surface_selection_mismatch");
    assert.deepEqual(response.surface_observed_values, ["chatgpt", "work"]);
    assert.equal(hooks.ensureChatSurfaceCalls.length, 0);
    assert.equal(hooks.verifyChatSurfaceCalls.length, 0);
    assert.equal(hooks.verifyChatgptModelSelectionCalls.length, 1);
    assert.equal(hooks.clickSendCalls.length, 1);
    assert.equal(hooks.clickCommittedCalls, 0);
  } finally {
    restore();
  }
});

test("content script rejects a synchronous Chat surface drift after the async guard", async () => {
  const { send, hooks, restore } = await loadContentScript("send_surface_sync_guard", "https://chatgpt.com/?_yoetz=run_surface_sync_guard");
  try {
    const job = { job_id: "job_surface_sync_guard", run_id: "run_surface_sync_guard", send_timeout_ms: 1000 };
    assert.equal((await send({ type: "yoetz_prepare_job", job })).ok, true);
    hooks.verifyChatgptModelSelectionResult = {
      ok: false,
      failure_reason: "chat_surface_selection_mismatch",
      state: { aria_checked: "false", data_state: "off" },
      observed_values: ["chatgpt", "work"]
    };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "review" });

    assert.equal(response.ok, false);
    assert.equal(response.code, "model_selection_not_verified_before_send");
    assert.match(response.error, /changed or was incomplete immediately before send/);
    assert.equal(hooks.ensureChatSurfaceCalls.length, 0);
    assert.equal(hooks.verifyChatSurfaceCalls.length, 0);
    assert.equal(hooks.verifyChatgptModelSelectionCalls.length, 1);
    assert.equal(hooks.clickCommittedCalls, 0);
  } finally {
    restore();
  }
});

test("content script carries explicit Chat surface evidence through the final send guards", async () => {
  const { send, hooks, restore } = await loadContentScript("send_surface_evidence", "https://chatgpt.com/?_yoetz=run_surface_evidence");
  try {
    const job = {
      job_id: "job_surface_evidence",
      run_id: "run_surface_evidence",
      send_timeout_ms: 1000,
      surface_evidence_seen: true
    };
    assert.equal((await send({ type: "yoetz_prepare_job", job })).ok, true);
    hooks.verifyChatgptModelSelectionResult = { ok: true, surface_evidence_seen: true };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "review" });

    assert.equal(response.ok, true, JSON.stringify(response));
    assert.equal(hooks.ensureChatSurfaceCalls.length, 0);
    assert.equal(hooks.verifyChatgptModelSelectionCalls.length, 1);
    assert.equal(hooks.verifyChatgptModelSelectionCalls[0].surface_evidence_seen, true);
    assert.equal(hooks.clickCommittedCalls, 1);
    assert.equal(response.payload.final_model_selection.click_bound, true);
    assert.equal(response.payload.final_model_selection.surface_evidence_seen, true);
  } finally {
    restore();
  }
});

test("content script fences a committed send when acceptance is unknown", async () => {
  const { send, hooks, restore } = await loadContentScript("send_acceptance_unknown", "https://chatgpt.com/?_yoetz=run_acceptance_unknown");
  try {
    const job = { job_id: "job_acceptance_unknown", run_id: "run_acceptance_unknown", send_timeout_ms: 1000 };
    assert.equal((await send({ type: "yoetz_prepare_job", job })).ok, true);
    hooks.afterWaitForSendAccepted = () => {
      throw new Error("send acceptance was not observed");
    };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "review" });

    assert.equal(response.ok, false);
    assert.equal(response.code, "send_acceptance_unknown");
    assert.equal(response.phase, "send");
    assert.equal(response.side_effect_started, true);
    assert.equal(response.send_committed, true);
    assert.equal(hooks.clickCommittedCalls, 1);
  } finally {
    restore();
  }
});

test("content script rejects final ChatGPT model drift before clicking send", async () => {
  const { send, hooks, restore } = await loadContentScript("send_model_guard", "https://chatgpt.com/?_yoetz=run_model_guard");
  try {
    const job = { job_id: "job_model_guard", run_id: "run_model_guard", send_timeout_ms: 1000 };
    assert.equal((await send({ type: "yoetz_prepare_job", job })).ok, true);
    hooks.configureModelResult = {
      status: "selected",
      model_used: "GPT-5.6 Sol Expert",
      requested_model: "gpt-5-6-sol-chat-pro",
      family_status: "verified",
      effort_status: "verified"
    };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "review" });

    assert.equal(response.ok, false);
    assert.equal(response.code, "model_selection_not_verified_before_send");
    assert.equal(response.phase, "send");
    assert.equal(response.side_effect_started, true);
    assert.equal(response.send_committed, false);
    assert.equal(response.model_selection_status, "selected");
    assert.equal(response.model_selection_failure_reason, null);
    assert.equal(hooks.insertPromptCalls.length, 1);
    assert.equal(hooks.clickCommittedCalls, 0);
  } finally {
    restore();
  }
});

test("content script fails closed on Claude credits before marking prepare complete", async () => {
  const { send, hooks, restore } = await loadContentScript("claude_credits_prepare", "https://claude.ai/new?_yoetz=run_credits");
  try {
    hooks.recipe = "claude";
    hooks.blockingState = usageCreditsState();
    const response = await send({
      type: "yoetz_prepare_job",
      job: { job_id: "job_credits", run_id: "run_credits", recipe: "claude" }
    });

    assert.equal(response.ok, false);
    assert.equal(response.code, "usage_credits_exhausted");
    assert.equal(response.state, "usage_credits_exhausted");
    assert.equal(response.requested_model, "fable-5-max");
    assert.equal(response.phase, "upload");
    assert.equal(response.side_effect_started, false);
    assert.equal(response.send_committed, false);
    assert.deepEqual(hooks.markOwnershipCalls, []);
  } finally {
    restore();
  }
});

test("content script rechecks Claude credits after prompt insertion without clicking send", async () => {
  const { send, hooks, restore } = await loadContentScript("claude_credits_presend", "https://claude.ai/new?_yoetz=run_credits");
  try {
    hooks.recipe = "claude";
    const job = { job_id: "job_credits", run_id: "run_credits", recipe: "claude" };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);
    hooks.afterInsertPrompt = () => {
      hooks.blockingState = usageCreditsState();
    };

    const response = await send({ type: "yoetz_send_prompt", job, prompt: "review" });

    assert.equal(response.ok, false);
    assert.equal(response.code, "usage_credits_exhausted");
    assert.equal(response.phase, "send");
    assert.equal(response.side_effect_started, true);
    assert.equal(response.send_committed, false);
    assert.equal(response.provider_message, usageCreditsState().provider_message);
    assert.deepEqual(response.provider_dom, usageCreditsState().provider_dom);
    assert.equal(hooks.clickSendCalls.length, 0);
  } finally {
    restore();
  }
});

test("content script classifies a credit banner during baseline extraction as pre-send", async () => {
  const { send, hooks, restore } = await loadContentScript("claude_credits_baseline", "https://claude.ai/new?_yoetz=run_credits");
  try {
    hooks.recipe = "claude";
    const job = { job_id: "job_credits", run_id: "run_credits", recipe: "claude" };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);
    hooks.blockingState = usageCreditsState();

    const response = await send({
      type: "yoetz_extract_response",
      job,
      blocking_context: "pre_send_baseline"
    });

    assert.equal(response.ok, false);
    assert.equal(response.code, "usage_credits_exhausted");
    assert.equal(response.phase, "send");
    assert.equal(response.side_effect_started, true);
    assert.equal(response.send_committed, false);
  } finally {
    restore();
  }
});

test("content script fresh path still requires a fresh page after prepare", async () => {
  const { send, hooks, restore, location } = await loadContentScript("fresh_guard", "https://chatgpt.com/?_yoetz=run_fresh");
  try {
    const job = {
      job_id: "job_fresh",
      run_id: "run_fresh",
      upload_timeout_ms: 1000
    };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);
    assert.equal(hooks.ensureFreshChatCalls.length, 1);
    assert.deepEqual(hooks.ensureConversationLoadedCalls, []);

    location.href = "https://chatgpt.com/c/late?_yoetz=run_fresh";
    location.pathname = "/c/late";
    const response = await send({
      type: "yoetz_upload_file",
      job,
      file: {
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }
    });

    assert.equal(response.ok, false);
    assert.equal(response.code, "fresh_chat_lost");
  } finally {
    restore();
  }
});

test("content script accepts ChatGPT replacing a submitted WEB conversation id on the owned tab", async () => {
  const { send, hooks, restore, location } = await loadContentScript("fresh_web_assignment", "https://chatgpt.com/?_yoetz=run_fresh_assignment");
  try {
    hooks.allowConversationAssignment = true;
    const job = {
      job_id: "job_fresh_assignment",
      run_id: "run_fresh_assignment",
      upload_timeout_ms: 1000,
      send_timeout_ms: 1000
    };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);

    globalThis.__contentScriptTestHooks.afterWaitForSendAccepted = () => {
      location.href = "https://chatgpt.com/c/WEB:ca5209ac-2836-440d-b674-ffc54ee5dd2d?_yoetz=run_fresh_assignment";
      location.pathname = "/c/WEB:ca5209ac-2836-440d-b674-ffc54ee5dd2d";
    };
    const sent = await send({ type: "yoetz_send_prompt", job, prompt: "review" });
    assert.equal(sent.ok, true);
    assert.equal(sent.payload.conversation_id, "WEB:ca5209ac-2836-440d-b674-ffc54ee5dd2d");

    const waitingJob = {
      ...job,
      submitted_conversation_id: sent.payload.conversation_id
    };
    location.href = "https://chatgpt.com/c/6a5f60dc-8174-8329-949a-1f282d1dccbd";
    location.pathname = "/c/6a5f60dc-8174-8329-949a-1f282d1dccbd";
    const rebound = await send({ type: "yoetz_bind_job", job: waitingJob });
    assert.equal(rebound.ok, true, JSON.stringify(rebound));
    const extracted = await send({ type: "yoetz_extract_response", job: waitingJob });

    assert.equal(extracted.ok, true, JSON.stringify(extracted));
    assert.equal(extracted.payload.conversation_id, "6a5f60dc-8174-8329-949a-1f282d1dccbd");
  } finally {
    restore();
  }
});

test("content script requires the surviving window.name marker for conversation assignment", async () => {
  const { send, hooks, restore, location } = await loadContentScript(
    "fresh_web_assignment_wrong_owner",
    "https://chatgpt.com/?_yoetz=run_fresh_assignment"
  );
  try {
    hooks.allowConversationAssignment = true;
    const job = {
      job_id: "job_fresh_assignment",
      run_id: "run_fresh_assignment",
      upload_timeout_ms: 1000,
      send_timeout_ms: 1000
    };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);

    const waitingJob = {
      ...job,
      submitted_conversation_id: "WEB:ca5209ac-2836-440d-b674-ffc54ee5dd2d"
    };
    location.href = "https://chatgpt.com/c/6a5f60dc-8174-8329-949a-1f282d1dccbd";
    location.pathname = "/c/6a5f60dc-8174-8329-949a-1f282d1dccbd";
    globalThis.window.name = "yoetz-chatgpt-native:other_run:other_job";
    const extracted = await send({ type: "yoetz_extract_response", job: waitingJob });

    assert.equal(extracted.ok, false);
    assert.match(extracted.error, /ownership marker mismatch/);
  } finally {
    restore();
  }
});

test("content script verifies durable tab ownership without requiring in-memory active state", async () => {
  const { send, restore, location } = await loadContentScript(
    "durable_ownership_probe",
    "https://chatgpt.com/?_yoetz=run_durable_ownership"
  );
  try {
    const job = {
      job_id: "job_durable_ownership",
      run_id: "run_durable_ownership",
      workspace_id: "workspace_test",
      ownership_nonce: "nonce_durable_ownership"
    };
    const prepared = await send({ type: "yoetz_prepare_job", job });
    assert.equal(prepared.ok, true);
    const verified = await send({ type: "yoetz_verify_job_ownership", job });
    assert.equal(verified.ok, true, JSON.stringify(verified));
    assert.equal(verified.payload.owned, true);
    assert.equal(verified.payload.job_id, job.job_id);
    assert.equal(verified.payload.run_id, job.run_id);
    assert.equal(verified.payload.workspace_id, job.workspace_id);
    assert.equal(verified.payload.ownership_nonce, job.ownership_nonce);
    assert.equal(verified.payload.origin, "https://chatgpt.com");

    location.href = "https://chatgpt.com/?_yoetz=run_other";
    const rejected = await send({ type: "yoetz_verify_job_ownership", job });
    assert.equal(rejected.ok, false);
    assert.equal(rejected.code, "ownership_unverified");
  } finally {
    restore();
  }
});

test("content script reports only persisted pagehide and pageshow for active jobs", async () => {
  const { send, dispatchLifecycle, runtimeMessages, restore } = await loadContentScript(
    "persisted_lifecycle",
    "https://chatgpt.com/?_yoetz=run_lifecycle"
  );
  try {
    const job = {
      job_id: "job_lifecycle",
      run_id: "run_lifecycle",
      upload_timeout_ms: 1000,
      send_timeout_ms: 1000
    };
    assert.equal((await send({ type: "yoetz_prepare_job", job })).ok, true);

    dispatchLifecycle("pagehide", false);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(runtimeMessages.length, 0);

    dispatchLifecycle("pagehide", true);
    dispatchLifecycle("pageshow", true);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(runtimeMessages.map((message) => message.event), ["pagehide", "pageshow"]);
    assert.deepEqual(runtimeMessages[0].job_ids, ["job_lifecycle"]);
    assert.deepEqual(runtimeMessages[1].job_ids, ["job_lifecycle"]);
  } finally {
    restore();
  }
});

async function loadContentScript(label, href) {
  const originalChrome = globalThis.chrome;
  const originalWindow = globalThis.window;
  const originalDocument = globalThis.document;
  const originalLocation = globalThis.location;
  const location = locationState(href);
  let listener = null;
  const lifecycleListeners = new Map();
  const runtimeMessages = [];
  const hooks = {
    ensureFreshChatCalls: [],
    ensureConversationLoadedCalls: [],
    markOwnershipCalls: [],
    uploadFileCalls: [],
    configureModelCalls: [],
    events: [],
    ensureChatSurfaceCalls: [],
    verifyChatSurfaceCalls: [],
    verifyChatgptModelSelectionCalls: [],
    insertPromptCalls: [],
    clickSendCalls: [],
    clickCommittedCalls: 0,
    sendAcceptanceBaselineCalls: 0,
    manualHandoffInputs: [],
    waitManualHandoffInputs: []
  };
  globalThis.__contentScriptTestHooks = hooks;
  globalThis.window = {
    name: "",
    location,
    addEventListener: (type, callback) => lifecycleListeners.set(type, callback)
  };
  globalThis.document = { title: "ChatGPT", defaultView: globalThis.window };
  globalThis.location = location;
  const helperUrl = `data:text/javascript,${encodeURIComponent(helperModule)}#${label}`;
  globalThis.chrome = {
    runtime: {
      getURL: () => helperUrl,
      getManifest: () => ({ version: "test" }),
      sendMessage: async (message) => {
        runtimeMessages.push(message);
        return { ok: true };
      },
      onMessage: {
        addListener: (fn) => {
          listener = fn;
        }
      }
    }
  };

  await import(`../src/content-script.js?test=${label}-${Date.now()}`);
  assert.equal(typeof listener, "function");

  return {
    hooks,
    location,
    runtimeMessages,
    dispatchLifecycle: (type, persisted) => lifecycleListeners.get(type)?.({ persisted }),
    send: (message) => new Promise((resolve) => listener(message, {}, resolve)),
    restore: () => {
      globalThis.chrome = originalChrome;
      globalThis.window = originalWindow;
      globalThis.document = originalDocument;
      globalThis.location = originalLocation;
      delete globalThis.__contentScriptTestHooks;
    }
  };
}

function locationState(href) {
  const url = new URL(href);
  return {
    href: url.href,
    pathname: url.pathname
  };
}

function resumeJob() {
  return {
    job_id: "job_resume",
    run_id: "run_resume",
    conversation_id: "conv-123",
    expected_conversation_id: "conv-123",
    upload_timeout_ms: 1000,
    send_timeout_ms: 1000
  };
}

function usageCreditsState() {
  return {
    state: "usage_credits_exhausted",
    code: "usage_credits_exhausted",
    requested_model: "fable-5-max",
    provider_message: "Your org is out of usage credits for the month. We let your admin know. Switch models to continue chatting.",
    provider_dom: {
      container: { found: true, tag: "div", role: "alert" },
      switch_models_control: { found: false }
    },
    message: "Claude cannot run Fable 5 Max because this organization is out of monthly usage credits. Yoetz did not switch models."
  };
}

// ---- T1 backend-api read (yoetz_fetch_conversation) ----

function fetchJob(preSendAssistantCount = 0) {
  return {
    job_id: "job_fetch",
    run_id: "run_fetch",
    conversation_id: "conv-123",
    expected_conversation_id: "conv-123",
    response_baseline: { assistant_count: preSendAssistantCount },
    submitted_assistant_count: preSendAssistantCount,
    upload_timeout_ms: 1000,
    send_timeout_ms: 1000
  };
}

function asstTextNode(id, parent, text, opts = {}) {
  return {
    id,
    parent,
    children: [],
    message: {
      id,
      author: { role: "assistant" },
      content: { content_type: "text", parts: [text] },
      end_turn: opts.end_turn ?? true,
      recipient: opts.recipient ?? "all",
      status: "finished_successfully"
    }
  };
}

// Install a mocked same-origin fetch for /api/auth/session and /backend-api/conversation/<id>.
// conv = { current_node, mapping } or null to 404; status overrides the conversation GET status.
function installBackendFetch({ token = "tok-123", conv = null, conversationStatus = 200, sessionStatus = 200 } = {}) {
  const original = globalThis.fetch;
  globalThis.fetch = async (url) => {
    const u = String(url);
    if (u.startsWith("/api/auth/session")) {
      return { ok: sessionStatus >= 200 && sessionStatus < 300, status: sessionStatus, json: async () => ({ accessToken: token }) };
    }
    if (u.startsWith("/backend-api/conversation/")) {
      return {
        ok: conversationStatus >= 200 && conversationStatus < 300,
        status: conversationStatus,
        json: async () => conv ?? {}
      };
    }
    throw new Error(`unexpected fetch ${u}`);
  };
  return () => { globalThis.fetch = original; };
}

async function prepareFetchJob(send, hooks, job) {
  const prepared = await send({ type: "yoetz_prepare_job", job });
  assert.equal(prepared.ok, true, `prepare failed: ${JSON.stringify(prepared)}`);
}

test("backend-api read returns the fresh final answer from the conversation mapping", async () => {
  const { send, hooks, restore } = await loadContentScript("backend_happy", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const FINAL = "No P0 found. I found two P1 proof-integrity issues and several P2 residual risks across the bundle.";
  const restoreFetch = installBackendFetch({ conv: {
    current_node: "a_final",
    mapping: {
      root: { id: "root", parent: null, children: ["u1"], message: { author: { role: "system" }, content: { content_type: "text", parts: [""] } } },
      u1: { id: "u1", parent: "root", children: ["a_interim"], message: { author: { role: "user" }, content: { content_type: "text", parts: ["review this"] }, end_turn: null } },
      a_interim: asstTextNode("a_interim", "u1", "I'll review the bundled diff as the source of truth"),
      a_final: asstTextNode("a_final", "a_interim", FINAL)
    }
  }});
  try {
    const job = fetchJob(0);
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, true, JSON.stringify(res));
    assert.equal(res.payload.method, "backend_api");
    assert.equal(res.payload.node_fresh, true);
    assert.equal(res.payload.is_generating, false);
    assert.equal(res.payload.text, FINAL);
    assert.equal(res.payload.conversation_id, "conv-123");
    assert.equal(res.payload.assistant_count, 2);
    assert.equal(res.payload.node_id, "a_final");
  } finally {
    restoreFetch();
    restore();
  }
});

test("backend-api freshness uses the pre-send answer baseline, not the post-send DOM turn count", async () => {
  const { send, hooks, restore } = await loadContentScript("backend_post_send_dom_turn", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const restoreFetch = installBackendFetch({ conv: {
    current_node: "a_final",
    mapping: {
      u1: { id: "u1", parent: null, children: ["a_final"], message: { author: { role: "user" }, content: { content_type: "text", parts: ["Return only 7"] }, end_turn: null } },
      a_final: asstTextNode("a_final", "u1", "7")
    }
  }});
  try {
    const job = {
      ...fetchJob(),
      response_baseline: { assistant_count: 0 },
      // Send acceptance can observe the newly-created in-progress DOM turn.
      // That count is not comparable to completed backend answer nodes.
      submitted_assistant_count: 1
    };
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, true, JSON.stringify(res));
    assert.equal(res.payload.node_fresh, true);
    assert.equal(res.payload.text, "7");
    assert.equal(res.payload.assistant_count, 1);
  } finally {
    restoreFetch();
    restore();
  }
});

test("backend-api read does not accept an answer buried below a later current node", async () => {
  const { send, hooks, restore } = await loadContentScript("backend_buried_answer", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const CAPTION = "I'll review the bundle end to end, then report whether the producer and consumer invariants hold.";
  const restoreFetch = installBackendFetch({ conv: {
    // A backend caption can itself look like a completed answer. Once later
    // work exists above it, the caption must not be treated as turn-final.
    current_node: "recap",
    mapping: {
      u1: { id: "u1", parent: null, children: ["a_caption"], message: { author: { role: "user" }, content: { content_type: "text", parts: ["review"] }, end_turn: null } },
      a_caption: asstTextNode("a_caption", "u1", CAPTION),
      tool1: { id: "tool1", parent: "a_caption", children: ["recap"], message: { author: { role: "assistant" }, content: { content_type: "text", parts: ["{search}"] }, end_turn: true, recipient: "file_search.msearch", status: "finished_successfully" } },
      recap: { id: "recap", parent: "tool1", children: [], message: { author: { role: "assistant" }, content: { content_type: "reasoning_recap", parts: ["recapped"] }, end_turn: true, recipient: "all" } }
    }
  }});
  try {
    const job = fetchJob(0);
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, true, JSON.stringify(res));
    assert.equal(res.payload.node_fresh, false);
    assert.equal(res.payload.is_generating, true);
    assert.equal(res.payload.text, "");
    assert.match(res.payload.backend_api_detail, /current_node/i);
  } finally {
    restoreFetch();
    restore();
  }
});

test("backend-api read rejects an end_turn caption while any mapping message is in progress", async () => {
  const { send, hooks, restore } = await loadContentScript("backend_in_progress_caption", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const CAPTION = "I'll compare both mechanisms across failure recovery, takeover safety, and implementation guardrails.";
  const restoreFetch = installBackendFetch({ conv: {
    current_node: "a_caption",
    mapping: {
      u1: { id: "u1", parent: null, children: ["a_caption"], message: { author: { role: "user" }, content: { content_type: "text", parts: ["compare"] }, end_turn: null } },
      a_caption: asstTextNode("a_caption", "u1", CAPTION),
      tool_in_flight: {
        id: "tool_in_flight",
        parent: "a_caption",
        children: [],
        message: {
          author: { role: "tool" },
          content: { content_type: "text", parts: [""] },
          status: "in_progress"
        }
      }
    }
  }});
  try {
    const job = fetchJob(0);
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, true, JSON.stringify(res));
    assert.equal(res.payload.node_fresh, false);
    assert.equal(res.payload.is_generating, true);
    assert.equal(res.payload.text, "");
    assert.match(res.payload.backend_api_detail, /in.progress/i);
  } finally {
    restoreFetch();
    restore();
  }
});

test("backend-api read returns not-ready (keep waiting) when no answer is fresh past baseline", async () => {
  const { send, hooks, restore } = await loadContentScript("backend_stale", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const restoreFetch = installBackendFetch({ conv: {
    current_node: "a_old",
    mapping: {
      u1: { id: "u1", parent: null, children: ["a_old"], message: { author: { role: "user" }, content: { content_type: "text", parts: ["prior"] }, end_turn: null } },
      a_old: asstTextNode("a_old", "u1", "earlier answer from a prior turn")
    }
  }});
  try {
    // baseline already counts the single existing answer turn -> no NEW answer -> not fresh
    const job = fetchJob(1);
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, true, JSON.stringify(res));
    assert.equal(res.payload.method, "backend_api");
    assert.equal(res.payload.node_fresh, false);
    assert.equal(res.payload.is_generating, true, "stale read must keep the SW waiting, not complete");
    assert.equal(res.payload.text, "");
  } finally {
    restoreFetch();
    restore();
  }
});

test("backend-api read scopes freshness to the active lineage (off-branch answer must not inflate)", async () => {
  // codex's blocking finding: a global answer count could be inflated past baseline by an
  // OFF-branch completed answer (regen/abandoned/alternate branch) while the active current_node
  // lineage still points at a STALE earlier answer -> must NOT return the stale answer as fresh.
  const { send, hooks, restore } = await loadContentScript("backend_offbranch", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const restoreFetch = installBackendFetch({ conv: {
    // active lineage: root -> u1 -> a_old (the visible, stale answer). current_node = a_old.
    // a_alt_new is a sibling regeneration off the a_old lineage (child of u1, not reachable from a_old upward).
    current_node: "a_old",
    mapping: {
      root: { id: "root", parent: null, children: ["u1"], message: { author: { role: "system" }, content: { content_type: "text", parts: [""] } } },
      u1: { id: "u1", parent: "root", children: ["a_old", "a_alt_new"], message: { author: { role: "user" }, content: { content_type: "text", parts: ["q"] }, end_turn: null } },
      a_old: asstTextNode("a_old", "u1", "earlier visible answer on the active branch"),
      a_alt_new: asstTextNode("a_alt_new", "u1", "a regenerated answer on an off-branch not reachable from current_node")
    }
  }});
  try {
    // baseline already counts the single active-lineage answer (a_old) -> no NEW active answer.
    const job = fetchJob(1);
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, true, JSON.stringify(res));
    assert.equal(res.payload.node_fresh, false, "off-branch answer must not make the stale lineage answer look fresh");
    assert.equal(res.payload.is_generating, true);
    assert.equal(res.payload.text, "");
  } finally {
    restoreFetch();
    restore();
  }
});

test("backend-api read surfaces a 401 as backend_api_unauthorized so the SW can fall back", async () => {
  const { send, hooks, restore } = await loadContentScript("backend_401", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const restoreFetch = installBackendFetch({ conversationStatus: 401 });
  try {
    const job = fetchJob(0);
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, false);
    assert.equal(res.code, "backend_api_unauthorized");
  } finally {
    restoreFetch();
    restore();
  }
});

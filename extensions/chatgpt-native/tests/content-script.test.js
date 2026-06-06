import assert from "node:assert/strict";
import test from "node:test";
import { uint8ArrayToBase64 } from "../src/chunks.js";

const helperModule = `const hooks = globalThis.__contentScriptTestHooks;

export function ownedWindowName(job) {
  return \`yoetz-chatgpt-native:\${job.run_id}:\${job.job_id}\`;
}

export function parseOwnedWindowName(value) {
  if (typeof value !== "string" || !value.startsWith("yoetz-chatgpt-native:")) {
    return null;
  }
  const rest = value.slice("yoetz-chatgpt-native:".length);
  const separator = rest.lastIndexOf(":");
  return separator > 0 ? {
    run_id: rest.slice(0, separator),
    job_id: rest.slice(separator + 1)
  } : null;
}

export function getPageText() {
  return hooks.pageText ?? "";
}

export function classifyManualHandoff() {
  return hooks.manualHandoff ?? null;
}

export function classifyWaitManualHandoff() {
  return hooks.waitManualHandoff ?? null;
}

export async function ensureFreshChat(_document, job) {
  hooks.ensureFreshChatCalls.push(job);
  if (hooks.failFreshChat) {
    throw new Error(hooks.failFreshChat);
  }
  return { status: "fresh", pathname: globalThis.location.pathname };
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
  return true;
}

export function configureModelState(_document, job) {
  hooks.configureModelCalls.push(job);
  return { status: "selected", model_used: "Pro • Extended", requested_model: "extended-pro", extended_status: "required" };
}

export function sendAcceptanceBaseline() {
  hooks.sendAcceptanceBaselineCalls += 1;
  return { user_count: 1, assistant_count: 2 };
}

export async function insertPrompt(_document, prompt, options) {
  hooks.insertPromptCalls.push({ prompt, options });
  hooks.afterInsertPrompt?.();
}

export async function clickSend(_document, options) {
  hooks.clickSendCalls.push(options);
}

export async function waitForSendAccepted() {
  return hooks.sendAccepted ?? { accepted: true };
}

export function extractResponse() {
  return hooks.extraction ?? { method: "assistant_dom_fallback", text: "answer", conversation_id: conversationIdFromLocation() };
}

export function modelSelectionDiagnostics() {
  return {};
}

function conversationIdFromLocation() {
  const match = String(globalThis.location.pathname ?? "").match(/^\\/c\\/([^/?#]+)$/);
  return match ? decodeURIComponent(match[1]) : null;
}`;

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
    assert.equal(hooks.uploadFileCalls.length, 1);

    const configured = await send({ type: "yoetz_configure_model", job });
    assert.equal(configured.ok, true);

    const sent = await send({ type: "yoetz_send_prompt", job, prompt: "continue" });
    assert.equal(sent.ok, true);
    assert.equal(sent.payload.conversation_id, "conv-123");
    assert.equal(hooks.clickSendCalls[0].expectedConversationId, "conv-123");

    const extracted = await send({ type: "yoetz_extract_response", job });
    assert.equal(extracted.ok, true);
    assert.equal(extracted.payload.conversation_id, "conv-123");
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
    assert.equal(response.side_effect_started, false);
    assert.equal(hooks.insertPromptCalls.length, 1);
    assert.equal(hooks.clickSendCalls.length, 0);
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

async function loadContentScript(label, href) {
  const originalChrome = globalThis.chrome;
  const originalWindow = globalThis.window;
  const originalDocument = globalThis.document;
  const originalLocation = globalThis.location;
  const location = locationState(href);
  let listener = null;
  const hooks = {
    ensureFreshChatCalls: [],
    ensureConversationLoadedCalls: [],
    markOwnershipCalls: [],
    uploadFileCalls: [],
    configureModelCalls: [],
    insertPromptCalls: [],
    clickSendCalls: [],
    sendAcceptanceBaselineCalls: 0
  };
  globalThis.__contentScriptTestHooks = hooks;
  globalThis.window = { name: "", location };
  globalThis.document = { title: "ChatGPT", defaultView: globalThis.window };
  globalThis.location = location;
  const helperUrl = `data:text/javascript,${encodeURIComponent(helperModule)}#${label}`;
  globalThis.chrome = {
    runtime: {
      getURL: () => helperUrl,
      getManifest: () => ({ version: "test" }),
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

// ---- T1 backend-api read (yoetz_fetch_conversation) ----

function fetchJob(submittedAssistantCount = 0) {
  return {
    job_id: "job_fetch",
    run_id: "run_fetch",
    conversation_id: "conv-123",
    expected_conversation_id: "conv-123",
    submitted_assistant_count: submittedAssistantCount,
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

test("backend-api read walks past reasoning_recap and tool nodes to the real assistant answer", async () => {
  const { send, hooks, restore } = await loadContentScript("backend_walk", "https://chatgpt.com/c/conv-123?_yoetz=run_fetch");
  const FINAL = "I reviewed the bundle end to end; the consumer guard is correct and the producer invariants hold.";
  const restoreFetch = installBackendFetch({ conv: {
    // current_node is a reasoning_recap with end_turn:true (the live trap) whose parent chain leads to the text answer
    current_node: "recap",
    mapping: {
      u1: { id: "u1", parent: null, children: ["a_final"], message: { author: { role: "user" }, content: { content_type: "text", parts: ["review"] }, end_turn: null } },
      a_final: asstTextNode("a_final", "u1", FINAL),
      // a tool turn (recipient not 'all') must NOT count or be selected
      tool1: { id: "tool1", parent: "a_final", children: ["recap"], message: { author: { role: "assistant" }, content: { content_type: "text", parts: ["{search}"] }, end_turn: true, recipient: "file_search.msearch" } },
      // reasoning_recap with end_turn:true must NOT be selected
      recap: { id: "recap", parent: "tool1", children: [], message: { author: { role: "assistant" }, content: { content_type: "reasoning_recap", parts: ["recapped"] }, end_turn: true, recipient: "all" } }
    }
  }});
  try {
    const job = fetchJob(0);
    await prepareFetchJob(send, hooks, job);
    const res = await send({ type: "yoetz_fetch_conversation", job, conversation_id: "conv-123" });
    assert.equal(res.ok, true, JSON.stringify(res));
    assert.equal(res.payload.node_fresh, true);
    assert.equal(res.payload.text, FINAL);
    assert.equal(res.payload.node_id, "a_final");
    assert.equal(res.payload.assistant_count, 1, "recap + tool nodes must not count as answer turns");
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

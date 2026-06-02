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

    const extracted = await send({ type: "yoetz_extract_response", job });
    assert.equal(extracted.ok, true);
    assert.equal(extracted.payload.conversation_id, "conv-123");
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

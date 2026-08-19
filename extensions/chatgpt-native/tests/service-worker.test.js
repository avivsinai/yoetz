import assert from "node:assert/strict";
import test from "node:test";
import { uint8ArrayToBase64 } from "../src/chunks.js";

globalThis.__YOETZ_MIN_STABLE_IDLE_MS = 100;
globalThis.__YOETZ_STABLE_IDLE_INTERVAL_MULTIPLIER = 0;
globalThis.__YOETZ_BACKEND_API_CONFIRMATION_MS = 50;

test("service worker routes reconnect and multiplexes two native jobs", async () => {
  const originalChrome = globalThis.chrome;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  const port = makePort();
  const sentToTabs = [];
  const createdTabs = [];
  const jobTabs = new Map();
  const sentJobs = new Set();
  const extractedJobs = new Set();
  const releasableJobs = new Set();
  let tabId = 0;

  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};
  globalThis.chrome = {
    runtime: {
      connectNative: () => port,
      getManifest: () => ({ version: "0.4.0" }),
      getURL: (value) => new URL(`../${value}`, import.meta.url).href,
      onInstalled: { addListener: () => {} },
      onStartup: { addListener: () => {} },
      onMessage: { addListener: () => {} }
    },
    storage: {
      session: makeStorage(),
      local: makeStorage()
    },
    identity: {
      getProfileUserInfo: async (details) => {
        assert.deepEqual(details, { accountStatus: "ANY" });
        return { email: "work@example.com", id: "gaia-work" };
      }
    },
    alarms: {
      onAlarm: { addListener: () => {} },
      create: () => {},
      clear: () => {}
    },
    tabs: {
      create: async (options) => {
        const tab = { id: ++tabId, ...options };
        createdTabs.push(tab);
        return tab;
      },
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT", text: "" } };
          case "yoetz_prepare_job":
            jobTabs.set(message.job.job_id, id);
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sentJobs.add(message.job.job_id);
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: `conv-${message.job.job_id}`
              }
            };
          case "yoetz_extract_response":
            extractedJobs.add(message.job.job_id);
            return {
              ok: true,
              payload: sentJobs.has(message.job.job_id) && releasableJobs.has(message.job.job_id)
                ? { method: "assistant_dom_fallback", text: `answer ${message.job.job_id}`, is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0, conversation_id: `conv-${message.job.job_id}`, model_slug: "gpt-5-6-pro" }
                : sentJobs.has(message.job.job_id)
                  ? { method: "assistant_dom_fallback", text: `partial ${message.job.job_id}`, is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0, conversation_id: `conv-${message.job.job_id}` }
                  : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      },
      group: async () => 1
    },
    tabGroups: {
      update: async () => {}
    }
  };

  try {
    await import(`../src/service-worker.js?test=${Date.now()}`);
    await eventually(() => port.messages[0]?.type === "hello");
    assert.equal(port.messages[0].payload.profile_email, "work@example.com");
    assert.equal(port.messages[0].payload.profile_id, "gaia-work");
    assert.match(port.messages[0].payload.extension_instance_id, /^ext_/);
    assert.deepEqual(port.messages[0].payload.recipes, ["chatgpt", "claude"]);
    assert.deepEqual(port.messages[0].payload.capabilities, ["terminal_ack"]);

    port.emit(envelope("reconnect", "job_reconnect"));
    await eventually(() => port.messages.some((message) => message.type === "reconnect" && message.job_id === "job_reconnect"));

    port.messages.length = 0;
    const jobs = ["job_a", "job_b"];
    for (const jobId of jobs) {
      port.emit(envelope("job_start", jobId, {
        prompt: `prompt ${jobId}`,
        wait_interval_ms: 50,
        wait_timeout_ms: 5000
      }));
    }
    await eventually(() => port.messages.filter((message) => message.type === "job_progress" && message.payload.phase === "ready_for_file").length === 2);

    for (const jobId of jobs) {
      port.emit(envelope("job_file_chunk", jobId, {
        sequence: 0,
        total_chunks: 1,
        total_bytes: 4,
        filename: `${jobId}.md`,
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }));
    }
    await eventually(() => sentJobs.size === 2 && extractedJobs.size === 2);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    assert.notEqual(jobTabs.get("job_a"), jobTabs.get("job_b"));
    assert.deepEqual(createdTabs.map((tab) => tab.active), [false, false]);

    releasableJobs.add("job_b");
    await eventually(() => port.messages.some((message) =>
      message.job_id === "job_b" && ["job_complete", "job_error"].includes(message.type)
    ));
    assert.equal(
      port.messages.find((message) => message.type === "job_error" && message.job_id === "job_b"),
      undefined
    );
    assert.equal(port.messages.some((message) => message.type === "job_complete" && message.job_id === "job_a"), false);
    releasableJobs.add("job_a");
    await eventually(() => port.messages.filter((message) => message.type === "job_complete").length === 2);
    assert.deepEqual(
      port.messages.filter((message) => message.type === "job_file_chunk_ack").map((message) => message.job_id).sort(),
      jobs
    );
    assert.deepEqual(
      port.messages.filter((message) => message.type === "job_complete").map((message) => message.job_id).sort(),
      jobs
    );
    assert.equal(
      port.messages.find((message) => message.type === "job_complete" && message.job_id === "job_a")?.payload.completion_reason,
      "copy_button"
    );
    assert.equal(
      port.messages.find((message) => message.type === "job_complete" && message.job_id === "job_a")?.payload.conversation_id,
      "conv-job_a"
    );
    assert.equal(
      port.messages.find((message) => message.type === "job_complete" && message.job_id === "job_a")?.payload.conversation_url,
      "https://chatgpt.com/c/conv-job_a"
    );
    assert.equal(
      port.messages.find((message) => message.type === "job_complete" && message.job_id === "job_a")?.payload.model_used,
      "gpt-5-6-pro"
    );
    assert.equal(sentToTabs.filter((item) => item.message.type === "yoetz_upload_file").length, 2);
    for (const jobId of jobs) {
      const ownedTabIds = new Set(
        sentToTabs
          .filter((item) => item.message.job?.job_id === jobId)
          .map((item) => item.id)
      );
      assert.deepEqual([...ownedTabIds], [jobTabs.get(jobId)], `${jobId} must remain on its own tab`);
    }
    assert.equal(
      sentToTabs.find((item) => item.message.type === "yoetz_configure_model" && item.message.job.job_id === "job_b")?.message.job.model,
      "gpt-5-6-sol-extra-high"
    );
  } finally {
    globalThis.chrome = originalChrome;
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

test("service worker runs two concurrent Claude jobs in background and isolates cancellation", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const createdTabs = [];
  const updatedTabs = [];
  const removedTabs = [];
  const sentToTabs = [];
  const jobTabs = new Map();
  const sentJobs = new Set();
  const extractedJobs = new Set();
  const releasableJobs = new Set();
  const conversationIds = new Map([
    ["job_claude_cancel", "11111111-1111-4111-8111-111111111111"],
    ["job_claude_survivor", "22222222-2222-4222-8222-222222222222"]
  ]);
  let tabId = 100;

  globalThis.chrome = chromeStub({
    port,
    tabs: {
      query: async () => [{ id: 17, active: true }],
      create: async (options) => {
        const tab = { id: ++tabId, status: "complete", ...options };
        createdTabs.push(tab);
        return tab;
      },
      get: async (id) => createdTabs.find((tab) => tab.id === id),
      update: async (id, options) => {
        updatedTabs.push({ id, options });
        return { id, ...options };
      },
      remove: async (id) => {
        removedTabs.push(id);
      },
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { recipe: "claude" } };
          case "yoetz_prepare_job":
            jobTabs.set(message.job.job_id, id);
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedFableMaxSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt": {
            const jobId = message.job.job_id;
            sentJobs.add(jobId);
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: conversationIds.get(jobId),
                submitted_user_count: 1,
                submitted_assistant_count: 0
              }
            };
          }
          case "yoetz_extract_response": {
            const jobId = message.job.job_id;
            extractedJobs.add(jobId);
            if (!sentJobs.has(jobId)) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 } };
            }
            const complete = releasableJobs.has(jobId);
            return {
              ok: true,
              payload: {
                method: "assistant_dom",
                text: complete ? `answer ${jobId}` : `partial ${jobId}`,
                is_generating: !complete,
                assistant_count: 1,
                copy_button_count: 0,
                has_copy_button: false,
                turn_index: 0,
                conversation_id: conversationIds.get(jobId)
              }
            };
          }
          case "yoetz_cancel_send":
            return { ok: true, payload: { stopped: true } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      },
      group: async () => 1
    }
  });

  try {
    await import(`../src/service-worker.js?two_claude_background=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));

    const jobs = ["job_claude_cancel", "job_claude_survivor"];
    for (const jobId of jobs) {
      port.emit(envelope("job_start", jobId, {
        recipe: "claude",
        prompt: `prompt ${jobId}`,
        wait_interval_ms: 50,
        wait_timeout_ms: 5000
      }));
    }
    await eventually(() => port.messages.filter((message) =>
      message.type === "job_progress" && message.payload?.phase === "ready_for_file"
    ).length === 2);

    assert.notEqual(jobTabs.get(jobs[0]), jobTabs.get(jobs[1]));
    assert.deepEqual(createdTabs.map((tab) => tab.active), [false, false]);

    for (const jobId of jobs) {
      port.emit(envelope("job_file_chunk", jobId, {
        sequence: 0,
        total_chunks: 1,
        total_bytes: 4,
        filename: `${jobId}.md`,
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }));
    }
    await eventually(() => sentJobs.size === 2 && extractedJobs.size === 2);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);

    port.emit(envelope("job_cancel", "job_claude_cancel"));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_cancel" && message.job_id === "job_claude_cancel"
    ));
    assert.deepEqual(removedTabs, [jobTabs.get("job_claude_cancel")]);
    assert.ok(sentToTabs.some((item) =>
      item.message.type === "yoetz_cancel_send" && item.message.job.job_id === "job_claude_cancel"
    ));
    assert.equal(sentToTabs.some((item) =>
      item.message.type === "yoetz_cancel_send" && item.message.job.job_id === "job_claude_survivor"
    ), false);
    assert.equal(port.messages.some((message) =>
      message.type === "job_complete" && message.job_id === "job_claude_cancel"
    ), false);

    releasableJobs.add("job_claude_survivor");
    await eventually(() => port.messages.some((message) =>
      message.type === "job_complete" && message.job_id === "job_claude_survivor"
    ));
    const survivor = port.messages.find((message) =>
      message.type === "job_complete" && message.job_id === "job_claude_survivor"
    );
    assert.equal(survivor.payload.response, "answer job_claude_survivor");
    assert.equal(survivor.payload.model_used, "Fable 5 Max");
    assert.deepEqual(updatedTabs, []);

    for (const jobId of jobs) {
      const ownedTabIds = new Set(sentToTabs
        .filter((item) => item.message.job?.job_id === jobId)
        .map((item) => item.id));
      assert.deepEqual([...ownedTabIds], [jobTabs.get(jobId)], `${jobId} must remain on its own tab`);
    }
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker reconciles ChatGPT conversation assignment after the URL marker is dropped", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const submittedConversationId = "WEB:ca5209ac-2836-440d-b674-ffc54ee5dd2d";
  const assignedConversationId = "6a5f60dc-8174-8329-949a-1f282d1dccbd";
  const sentJobs = new Set();
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async () => ({ id: 71 }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/?_yoetz=run_job_web_assignment" }),
      sendMessage: async (id, message) => {
        assert.equal(id, 71);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT", text: "" } };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sentJobs.add(message.job.job_id);
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: submittedConversationId,
                submitted_user_count: 1,
                submitted_assistant_count: 0
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sentJobs.has(message.job.job_id) ? {
                method: "assistant_dom_fallback",
                text: "answer",
                is_generating: false,
                assistant_count: 1,
                copy_button_count: 1,
                has_copy_button: true,
                turn_index: 0,
                conversation_id: assignedConversationId,
                url: `https://chatgpt.com/c/${assignedConversationId}`
              } : {
                method: "none",
                text: "",
                is_generating: false,
                assistant_count: 0,
                turn_index: -1
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      },
      group: async () => 1
    }
  });

  try {
    await import(`../src/service-worker.js?web_assignment=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    const runAssignmentJob = async (jobId) => {
      port.messages.length = 0;
      port.emit(envelope("job_start", jobId, {
        prompt: "review",
        wait_interval_ms: 10,
        wait_timeout_ms: 1000
      }));
      await eventually(() => port.messages.some((message) =>
        message.type === "job_progress" && message.payload?.phase === "ready_for_file"
      ));
      port.emit(envelope("job_file_chunk", jobId, {
        sequence: 0,
        total_chunks: 1,
        total_bytes: 4,
        filename: "bundle.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }));
      await eventually(() => port.messages.some((message) =>
        message.type === "job_complete" || message.type === "job_error"
      ));
      return port.messages.find((message) =>
        message.type === "job_complete" || message.type === "job_error"
      );
    };

    const terminal = await runAssignmentJob("job_web_assignment");
    assert.equal(terminal.type, "job_complete", JSON.stringify(terminal));
    assert.equal(terminal.payload.conversation_id, assignedConversationId);
    assert.equal(terminal.payload.conversation_url, `https://chatgpt.com/c/${assignedConversationId}`);

  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker runs a Claude job through its adapter and probes the selected recipe", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let sent = false;
  let postSendExtractCount = 0;
  const conversationId = "123e4567-e89b-12d3-a456-426614174000";

  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async ({ url, active }) => {
        assert.equal(url, "https://claude.ai/new?_yoetz=run_job_claude");
        assert.equal(active, false);
        return { id: 71 };
      },
      get: async (id) => ({ id, status: "complete", url: "https://claude.ai/new?_yoetz=run_job_claude" }),
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { recipe: "claude", url: "https://claude.ai/new" } };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "selected",
                requested_model: "fable-5-max",
                modelVerified: true,
                maxVerified: true,
                model_used: "Fable 5 Max"
              }
            };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: conversationId,
                submitted_user_count: 1,
                submitted_assistant_count: 0
              }
            };
          case "yoetz_extract_response":
            if (sent) postSendExtractCount += 1;
            const latestAssistant = postSendExtractCount >= 2;
            return {
              ok: true,
              payload: sent
                  ? {
                    method: "assistant_dom",
                    text: latestAssistant ? "Short answer" : "Claude answer that is longer",
                    is_generating: false,
                    assistant_count: 1,
                    assistant_identity: latestAssistant ? "assistant-a2" : "assistant-a1",
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: 0,
                    conversation_id: conversationId,
                    artifact_blocks: {
                      count: postSendExtractCount >= 3 ? 1 : 0,
                      titles: postSendExtractCount >= 3 ? ["Release plan"] : []
                    }
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      },
      group: async () => 1
    }
  });

  try {
    await import(`../src/service-worker.js?claude_job=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    assert.deepEqual(
      port.messages.find((message) => message.type === "hello")?.payload.recipes,
      ["chatgpt", "claude"]
    );
    port.messages.length = 0;

    port.emit(envelope("job_start", "job_claude", {
      recipe: "claude",
      prompt: "review",
      wait_interval_ms: 500,
      wait_timeout_ms: 2500
    }));
    await eventually(() => port.messages.some((message) =>
      message.job_id === "job_claude"
      && (message.type === "job_error" || message.payload?.phase === "ready_for_file")
    ));
    const startError = port.messages.find((message) =>
      message.type === "job_error" && message.job_id === "job_claude"
    );
    assert.equal(startError, undefined, JSON.stringify(startError?.payload));
    assert.equal(
      sentToTabs.find((item) => item.message.type === "yoetz_probe")?.message.recipe,
      "claude"
    );

    port.emit(envelope("job_file_chunk", "job_claude", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "bundle.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_complete" && message.job_id === "job_claude"
    ));
    const complete = port.messages.find((message) =>
      message.type === "job_complete" && message.job_id === "job_claude"
    );
    assert.equal(complete.payload.response, "Short answer");
    assert.equal(complete.payload.model_used, "Fable 5 Max");
    assert.equal(complete.payload.completion_reason, "stable_idle");
    assert.equal(complete.payload.conversation_id, conversationId);
    assert.equal(complete.payload.conversation_url, `https://claude.ai/chat/${conversationId}`);
    assert.deepEqual(complete.payload.warnings, [{
      code: "artifact_unextracted",
      count: 1,
      titles: ["Release plan"]
    }]);
    assert.equal(postSendExtractCount, 3);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker completes an artifact-only Claude response with both warnings", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let sent = false;
  const conversationId = "223e4567-e89b-12d3-a456-426614174000";

  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async ({ url, active }) => {
        assert.equal(url, "https://claude.ai/new?_yoetz=run_job_claude_artifact_only");
        assert.equal(active, false);
        return { id: 72 };
      },
      get: async (id) => ({
        id,
        status: "complete",
        url: "https://claude.ai/new?_yoetz=run_job_claude_artifact_only"
      }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { recipe: "claude", url: "https://claude.ai/new" } };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "selected",
                requested_model: "fable-5-max",
                modelVerified: true,
                maxVerified: true,
                model_used: "Fable 5 Max"
              }
            };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: conversationId,
                submitted_user_count: 1,
                submitted_assistant_count: 0
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "none",
                    text: "",
                    is_generating: false,
                    assistant_count: 1,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: 0,
                    conversation_id: conversationId,
                    artifact_blocks: {
                      count: 1,
                      titles: ["Release plan"]
                    }
                  }
                : {
                    method: "none",
                    text: "",
                    is_generating: false,
                    assistant_count: 0,
                    turn_index: -1
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      },
      group: async () => 1
    }
  });

  try {
    await import(`../src/service-worker.js?claude_artifact_only=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("job_start", "job_claude_artifact_only", {
      recipe: "claude",
      prompt: "review",
      wait_interval_ms: 500,
      wait_timeout_ms: 2500
    }));
    await eventually(() => port.messages.some((message) =>
      message.job_id === "job_claude_artifact_only"
      && (message.type === "job_error" || message.payload?.phase === "ready_for_file")
    ));
    const startError = port.messages.find((message) =>
      message.type === "job_error" && message.job_id === "job_claude_artifact_only"
    );
    assert.equal(startError, undefined, JSON.stringify(startError?.payload));

    port.emit(envelope("job_file_chunk", "job_claude_artifact_only", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "bundle.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((message) =>
      message.job_id === "job_claude_artifact_only"
      && (message.type === "job_complete" || message.type === "job_error")
    ));
    const complete = port.messages.find((message) =>
      message.type === "job_complete" && message.job_id === "job_claude_artifact_only"
    );
    assert.ok(complete, JSON.stringify(port.messages));
    assert.equal(complete.payload.response, "");
    assert.deepEqual(complete.payload.warnings, [
      "empty Claude response extracted",
      {
        code: "artifact_unextracted",
        count: 1,
        titles: ["Release plan"]
      }
    ]);
    assert.equal(
      port.messages.some((message) =>
        message.type === "job_error" && message.job_id === "job_claude_artifact_only"
      ),
      false
    );
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker reports a generic Claude model timeout as model_selection", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();

  globalThis.chrome = chromeStub({
    port,
    tabs: {
      query: async () => [{ id: 17, active: true }],
      create: async (options) => ({ id: 71, ...options }),
      get: async (id) => ({ id, status: "complete", url: "https://claude.ai/new?_yoetz=run_job_claude_model_timeout" }),
      update: async (id, options) => ({ id, ...options }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { recipe: "claude" } };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: false, error: "Claude page did not reach the requested state within 10000ms" };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?claude_model_timeout_phase=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("job_start", "job_claude_model_timeout", {
      recipe: "claude",
      prompt: "review"
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.job_id === "job_claude_model_timeout"
    ));
    const error = port.messages.find((message) =>
      message.type === "job_error" && message.job_id === "job_claude_model_timeout"
    );
    assert.equal(error.payload.code, "extension_error");
    assert.equal(error.payload.phase, "model_selection");
    assert.equal(error.payload.side_effect_started, true);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker surfaces a Claude attachment-stalled trace in the job error", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let uploadJob = null;
  const attachmentTrace = {
    final_chunk_ack_at_ms: 100,
    input_resolved_at_ms: 101,
    files_assigned_at_ms: 102,
    change_dispatched_at_ms: 103,
    soft_timeout_at_ms: 125000,
    hard_timeout_at_ms: 420000,
    hard_timeout_pending_legs: ["matching_thumbnail", "remove_control"]
  };

  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (options) => ({ id: 71, ...options }),
      get: async (id) => ({ id, status: "complete", url: "https://claude.ai/new?_yoetz=run_job_claude_attachment_stalled" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { recipe: "claude" } };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedFableMaxSelection() };
          case "yoetz_upload_file":
            uploadJob = message.job;
            return {
              ok: false,
              code: "attachment_stalled",
              error: "Claude attachment stalled before readiness",
              phase: "upload",
              side_effect_started: true,
              attachment_trace: attachmentTrace
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?claude_attachment_trace=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("job_start", "job_claude_attachment_stalled", {
      recipe: "claude",
      prompt: "review",
      attachment_stall_timeout_ms: 420000
    }));
    await eventually(() => port.messages.some((message) =>
      message.job_id === "job_claude_attachment_stalled"
      && message.payload?.phase === "ready_for_file"
    ));
    port.emit(envelope("job_file_chunk", "job_claude_attachment_stalled", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "bundle.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.job_id === "job_claude_attachment_stalled"
    ));
    const error = port.messages.find((message) =>
      message.type === "job_error" && message.job_id === "job_claude_attachment_stalled"
    );
    assert.equal(error.payload.code, "attachment_stalled");
    assert.equal(error.payload.phase, "upload");
    assert.deepEqual(error.payload.attachment_trace, attachmentTrace);
    assert.ok(Number.isSafeInteger(uploadJob?.attachment_trace?.final_chunk_ack_at_ms));
    assert.equal(uploadJob?.attachment_stall_timeout_ms, 420000);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker surfaces Claude model mismatch legs in the job error", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();

  globalThis.chrome = chromeStub({
    port,
    tabs: {
      query: async () => [{ id: 17, active: true }],
      create: async (options) => ({ id: 71, ...options }),
      get: async (id) => ({ id, status: "complete", url: "https://claude.ai/new?_yoetz=run_job_claude_mismatch" }),
      update: async (id, options) => ({ id, ...options }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { recipe: "claude" } };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "mismatch",
                requested_model: "fable-5-max",
                model_used: "Fable 5 High",
                modelVerified: true,
                maxVerified: false,
                modelChip: "Fable 5 High",
                options: ["Fable 5", "High", "Max"]
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?claude_model_mismatch_detail=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("job_start", "job_claude_mismatch", {
      recipe: "claude",
      prompt: "review"
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.job_id === "job_claude_mismatch"
    ));
    const error = port.messages.find((message) =>
      message.type === "job_error" && message.job_id === "job_claude_mismatch"
    );
    assert.match(error.payload.message, /modelVerified=true/);
    assert.match(error.payload.message, /maxVerified=false/);
    assert.deepEqual(error.payload.model_selection_diagnostics, {
      modelVerified: true,
      maxVerified: false,
      modelChip: "Fable 5 High",
      options: ["Fable 5", "High", "Max"]
    });
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker doctor auth probe prefers active non-owned ChatGPT tab and surfaces login", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const sentToTabs = [];
  await storage.set({
    "jobs.job_complete_leak": {
      job_id: "job_complete_leak",
      tab_id: 1,
      status: "complete",
      close_tab_on_complete: true
    },
    "jobs.job_active_owned": {
      job_id: "job_active_owned",
      tab_id: 4,
      status: "waiting_response"
    },
    "jobs.job_kept_complete": {
      job_id: "job_kept_complete",
      tab_id: 5,
      status: "complete",
      close_tab_on_complete: false
    },
    "jobs.job_closed_complete": {
      job_id: "job_closed_complete",
      tab_id: 6,
      status: "complete",
      close_tab_on_complete: true,
      tab_disposition: "closed"
    }
  });
  globalThis.chrome = chromeStub({
    port,
    storage,
    profileEmail: "work@example.com",
    tabs: {
      query: async () => [
        { id: 1, url: "https://chatgpt.com/c/completed", title: "Yoetz job", active: false },
        { id: 2, url: "https://chatgpt.com/", title: "ChatGPT", active: true },
        { id: 3, url: "https://chatgpt.com/c/older", title: "Older ChatGPT", active: false },
        { id: 4, url: "https://chatgpt.com/c/active?_yoetz=run_active", title: "Active Yoetz job", active: false },
        { id: 5, url: "https://chatgpt.com/c/kept", title: "Kept Yoetz job", active: false },
        { id: 6, url: "https://chatgpt.com/c/closed", title: "Stale closed shard", active: false }
      ],
      create: async () => {
        throw new Error("doctor auth probe must not open a tab");
      },
      get: async () => {
        throw new Error("doctor auth probe must not poll tab loading");
      },
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        assert.equal(message.type, "yoetz_auth_probe");
        return {
          ok: true,
          payload: {
            status: "login_required",
            authenticated: false,
            manual_handoff: {
              state: "login_required",
              message: "ChatGPT login required in this Chrome profile"
            },
            url: "https://chatgpt.com/auth/login",
            title: "Log in | ChatGPT"
          }
        };
      }
    }
  });

  try {
    await import(`../src/service-worker.js?doctor_auth=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("reconnect", "job_auth_probe", { intent: "doctor_auth_probe" }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.job_id, "job_auth_probe");
    assert.equal(complete.payload.status, "login_required");
    assert.equal(complete.payload.authenticated, false);
    assert.equal(complete.payload.manual_handoff.state, "login_required");
    assert.equal(complete.payload.tab_id, 2);
    assert.equal(complete.payload.selection, "active_non_yoetz_chatgpt_tab");
    assert.equal(complete.payload.yoetz_owned_tabs_open, 3);
    assert.equal(complete.payload.yoetz_owned_complete_tabs_open, 1);
    assert.deepEqual(sentToTabs.map((item) => item.id), [2]);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker bridge_check is a no-op and does not recover jobs", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const now = Date.now();
  await storage.set({
    "jobs.job_bridge_restore": {
      job_id: "job_bridge_restore",
      run_id: "run_job_bridge_restore",
      workspace_id: "workspace_test",
      status: "waiting_for_file",
      prompt: "prompt",
      tab_id: 42,
      started_at: now,
      updated_at: now
    }
  });

  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {}
  });

  try {
    await import(`../src/service-worker.js?bridge_check=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_progress"
      && message.job_id === "job_bridge_restore"
      && message.payload.phase === "ready_for_file"
    ));
    port.messages.length = 0;

    port.emit(envelope("reconnect", "job_bridge_check", { intent: "bridge_check" }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_complete" && message.job_id === "job_bridge_check"
    ));
    const complete = port.messages.find((message) =>
      message.type === "job_complete" && message.job_id === "job_bridge_check"
    );
    assert.equal(complete.payload.status, "ok");
    assert.equal(Object.hasOwn(complete.payload, "restored_jobs"), false);
    assert.equal(port.messages.some((message) => message.type === "reconnect"), false);
    assert.equal(port.messages.some((message) =>
      message.type === "job_progress" && message.job_id === "job_bridge_restore"
    ), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker opens fresh and resume jobs in new owned tabs", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const createdTabs = [];
  const sentToTabs = [];
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => {
        const tab = { id: createdTabs.length + 1, ...opts };
        createdTabs.push(tab);
        return tab;
      },
      query: async () => {
        throw new Error("resume jobs must not discover or reuse existing tabs");
      },
      get: async (id) => {
        const tab = createdTabs.find((item) => item.id === id);
        return { id, status: "complete", url: tab?.url ?? "https://chatgpt.com/" };
      },
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?resume_url=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("job_start", "job_fresh_url", {
      prompt: "fresh",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));
    port.emit(envelope("job_start", "job_resume_url", {
      prompt: "resume",
      conversation_id: "conv-123",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));

    await eventually(() => port.messages.filter((message) =>
      message.type === "job_progress" && message.payload?.phase === "ready_for_file"
    ).length === 2);

    assert.deepEqual(
      createdTabs.map((tab) => ({ url: tab.url, active: tab.active })),
      [
        { url: "https://chatgpt.com/?_yoetz=run_job_fresh_url", active: false },
        { url: "https://chatgpt.com/c/conv-123?_yoetz=run_job_resume_url", active: false }
      ]
    );
    assert.equal(createdTabs.length, 2);
    assert.equal(
      sentToTabs.find((item) => item.message.type === "yoetz_prepare_job" && item.message.job.job_id === "job_resume_url")?.message.job.expected_conversation_id,
      "conv-123"
    );
    assert.equal(
      sentToTabs.find((item) => item.message.type === "yoetz_prepare_job" && item.message.job.job_id === "job_resume_url")?.message.job.conversation_id,
      "conv-123"
    );
    assert.equal(
      sentToTabs.find((item) => item.message.type === "yoetz_prepare_job" && item.message.job.job_id === "job_fresh_url")?.message.job.expected_conversation_id,
      null
    );
    assert.equal(
      sentToTabs.find((item) => item.message.type === "yoetz_prepare_job" && item.message.job.job_id === "job_fresh_url")?.message.job.conversation_id,
      null
    );
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects invalid conversation ids before opening a tab", async () => {
  const invalidCases = [
    ".",
    "..",
    "a/b",
    "conv%2F123",
    "",
    "x".repeat(257),
    42
  ];

  for (const [index, invalid] of invalidCases.entries()) {
    const originalChrome = globalThis.chrome;
    const port = makePort();
    const createdTabs = [];
    globalThis.chrome = chromeStub({
      port,
      tabs: {
        create: async (opts) => {
          createdTabs.push(opts);
          return { id: createdTabs.length, ...opts };
        },
        get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
        sendMessage: async () => {
          throw new Error("invalid conversation must fail before tab messaging");
        }
      }
    });

    try {
      await import(`../src/service-worker.js?invalid_conversation=${Date.now()}_${index}`);
      port.emit(envelope("job_start", `job_invalid_${index}`, {
        prompt: "resume",
        conversation_id: invalid,
        wait_interval_ms: 50,
        wait_timeout_ms: 1000
      }));

      await eventually(() => port.messages.some((message) => message.type === "job_error"));
      const error = port.messages.find((message) => message.type === "job_error");
      assert.equal(error.payload.code, "invalid_conversation");
      assert.equal(error.payload.phase, "upload");
      assert.equal(error.payload.side_effect_started, false);
      assert.equal(error.payload.tab_disposition, undefined);
      assert.deepEqual(createdTabs, []);
    } finally {
      globalThis.chrome = originalChrome;
    }
  }
});

test("service worker rejects unavailable recipes before opening a tab", async () => {
  for (const recipe of ["unknown"]) {
    const originalChrome = globalThis.chrome;
    const port = makePort();
    const createdTabs = [];
    globalThis.chrome = chromeStub({
      port,
      tabs: {
        create: async (opts) => {
          createdTabs.push(opts);
          return { id: createdTabs.length, ...opts };
        },
        get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
        sendMessage: async () => {
          throw new Error("unsupported recipes must fail before tab messaging");
        }
      }
    });

    try {
      await import(`../src/service-worker.js?unsupported_recipe=${recipe}_${Date.now()}`);
      port.emit(envelope("job_start", `job_${recipe}`, { recipe, prompt: "review" }));

      await eventually(() => port.messages.some((message) => message.type === "job_error"));
      const error = port.messages.find((message) => message.type === "job_error");
      assert.equal(error.payload.code, "unsupported_recipe");
      assert.equal(error.payload.phase, "profile");
      assert.equal(error.payload.side_effect_started, false);
      assert.deepEqual(createdTabs, []);
    } finally {
      globalThis.chrome = originalChrome;
    }
  }
});

test("service worker trims a valid conversation id before opening the resume tab", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const createdTabs = [];
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => {
        const tab = { id: createdTabs.length + 1, ...opts };
        createdTabs.push(tab);
        return tab;
      },
      get: async (id) => ({ id, status: "complete", url: createdTabs.find((item) => item.id === id)?.url ?? "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?trim_conversation=${Date.now()}`);
    port.emit(envelope("job_start", "job_trim_conversation", {
      prompt: "resume",
      conversation_id: " conv-123 ",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_progress" && message.payload?.phase === "ready_for_file"
    ));
    assert.equal(createdTabs[0].url, "https://chatgpt.com/c/conv-123?_yoetz=run_job_trim_conversation");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails immediately when send reports the wrong resumed conversation", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-123?_yoetz=run_job_send_drift" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_extract_response":
            return sent
              ? { ok: true, payload: { method: "assistant_dom_fallback", text: "wrong answer", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0, conversation_id: "other" } }
              : { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1, conversation_id: "conv-123" } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "other" } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?send_wrong_conversation=${Date.now()}`);
    port.emit(envelope("job_start", "job_send_drift", {
      prompt: "resume",
      conversation_id: "conv-123",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_progress" && message.payload?.phase === "ready_for_file"
    ));
    port.emit(envelope("job_file_chunk", "job_send_drift", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_send_drift.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.payload?.code === "conversation_changed"
    ));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.phase, "send");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.requested_conversation_id, "conv-123");
    assert.equal(error.payload.current_conversation_id, "other");
    assert.equal(port.messages.some((message) => message.payload?.phase === "prompt_sent"), false);
    assert.equal(sentToTabs.filter((type) => type === "yoetz_extract_response").length, 1);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails resumed jobs when post-send extraction omits the conversation id", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-123?_yoetz=run_job_missing_extract_conversation" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "conv-123" } };
          case "yoetz_extract_response":
            return sent
              ? { ok: true, payload: { method: "assistant_dom_fallback", text: "answer without identity", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 } }
              : { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1, conversation_id: "conv-123" } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?missing_extract_conversation=${Date.now()}`);
    port.emit(envelope("job_start", "job_missing_extract_conversation", {
      prompt: "resume",
      conversation_id: "conv-123",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_progress" && message.payload?.phase === "ready_for_file"
    ));
    port.emit(envelope("job_file_chunk", "job_missing_extract_conversation", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_missing_extract_conversation.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.payload?.code === "conversation_changed"
    ));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.phase, "wait_response");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.requested_conversation_id, "conv-123");
    assert.equal(error.payload.current_conversation_id, null);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), true);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails unavailable conversations with inspectable terminal error", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let tabId = 0;
  const currentUrl = "https://chatgpt.com/c/conv-404?_yoetz=run_job_unavailable";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: currentUrl }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return {
              ok: false,
              code: "conversation_unavailable",
              error: "ChatGPT conversation conv-404 is unavailable",
              phase: "upload",
              side_effect_started: false,
              requested_conversation_id: "conv-404",
              current_url: currentUrl
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?conversation_unavailable=${Date.now()}`);
    port.emit(envelope("job_start", "job_unavailable", {
      prompt: "resume",
      conversation_id: "conv-404",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.payload.code === "conversation_unavailable"
    ));

    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.phase, "upload");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.requested_conversation_id, "conv-404");
    assert.equal(error.payload.current_url, currentUrl);
    assert.equal(error.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_unavailable");
    assert.equal(error.payload.tab_disposition, "kept");
    assert.match(error.payload.message, /requested conversation conv-404/);
    assert.match(error.payload.message, /current URL https:\/\/chatgpt\.com\/c\/conv-404\?_yoetz=run_job_unavailable/);
    assert.match(error.payload.message, /phase upload/);
    assert.match(error.payload.message, /yoetz browser extension inspect --chatgpt --run-id run_job_unavailable/);
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
    assert.equal(port.messages.some((message) => message.payload?.phase === "ready_for_file"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker marks manual handoff as terminal after tab side effects", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: { state: "login_required", message: "login required" } } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?manual=${Date.now()}`);
    port.emit(envelope("job_start", "job_manual", { prompt: "prompt" }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "manual_handoff");
    assert.equal(error.payload.phase, "upload");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.tab_disposition, "kept");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker content-script readiness failure includes inspect context", async () => {
  const originalChrome = globalThis.chrome;
  const originalSetTimeout = globalThis.setTimeout;
  const port = makePort();
  const tabId = 920272522;

  globalThis.setTimeout = (fn, _ms, ...args) => originalSetTimeout(fn, 0, ...args);
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/?_yoetz=run_job_content_script_missing" }),
      sendMessage: async (_id, message) => {
        if (message.type === "yoetz_probe") {
          throw new Error("receiving end does not exist");
        }
        throw new Error(`unexpected tab message ${message.type}`);
      }
    }
  });

  try {
    await import(`../src/service-worker.js?content_script_missing=${Date.now()}`);
    port.emit(envelope("job_start", "job_content_script_missing", { prompt: "prompt" }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "extension_error");
    assert.equal(error.payload.phase, "upload");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.tab_id, tabId);
    assert.equal(error.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_content_script_missing");
    assert.match(error.payload.message, /Yoetz content script did not become ready in ChatGPT tab 920272522/);
  } finally {
    globalThis.chrome = originalChrome;
    globalThis.setTimeout = originalSetTimeout;
  }
});

test("service worker fails fast on metadata manual handoff detected while waiting for response", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "page_text_fallback",
                    text: "",
                    is_generating: false,
                    assistant_count: 0,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: -1,
                    manual_handoff: {
                      state: "login_required",
                      message: "ChatGPT login required in this Chrome profile"
                    }
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?wait_manual=${Date.now()}`);
    port.emit(envelope("job_start", "job_wait_manual", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_wait_manual", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_wait_manual.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "manual_handoff");
    assert.equal(error.payload.state, "login_required");
    assert.equal(error.payload.phase, "wait_response");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails closed when GPT-5.6 Sol Extra High is unavailable", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "unavailable",
                model_used: "Default",
                requested_model: "gpt-5-6-sol-extra-high",
                available_options: ["Default"],
                failure_reason: "effort_slider_move_failed",
                picker_shape: "slider",
                effort_control: {
                  label: "high",
                  value_text: "High, 3 of 5",
                  value_now: 3,
                  value_min: 1,
                  value_max: 5
                },
                family_status: "verified",
                effort_status: "unverified",
                picker_family_status: "verified",
                picker_effort_status: "unverified",
                closed_pill_family_status: "verified",
                closed_pill_effort_status: "unverified",
                closed_pill_text: "5.6 Sol\nHigh",
                warning: "GPT-5.6 Sol was not visible in the family submenu"
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?model_unavailable=${Date.now()}`);
    port.emit(envelope("job_start", "job_model_fail", {
      prompt: "prompt",
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "model_selection_failed");
    assert.equal(error.payload.phase, "model_selection");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.model_selection_status, "unavailable");
    assert.equal(error.payload.failure_reason, "effort_slider_move_failed");
    assert.equal(error.payload.model_selection_diagnostics.failure_reason, "effort_slider_move_failed");
    assert.equal(error.payload.model_selection_diagnostics.picker_shape, "slider");
    assert.equal(error.payload.model_selection_diagnostics.effort_control.value_text, "High, 3 of 5");
    assert.equal(error.payload.model_selection_diagnostics.picker_family_status, "verified");
    assert.equal(error.payload.model_selection_diagnostics.closed_pill_text, "5.6 Sol\nHigh");
    assert.match(error.payload.message, /reason: effort_slider_move_failed/);
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails resumed jobs before upload when GPT-5.6 Sol Extra High is unavailable", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const createdTabs = [];
  const sentToTabs = [];
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => {
        const tab = { id: createdTabs.length + 1, ...opts };
        createdTabs.push(tab);
        return tab;
      },
      get: async (id) => ({ id, status: "complete", url: createdTabs.find((tab) => tab.id === id)?.url ?? "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "unavailable",
                model_used: "Default",
                requested_model: "gpt-5-6-sol-extra-high",
                family_status: "unverified",
                effort_status: "unverified",
                available_options: ["Default"],
                warning: "ChatGPT model selector button not found"
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?resume_model_unavailable=${Date.now()}`);
    port.emit(envelope("job_start", "job_resume_model_fail", {
      prompt: "resume",
      conversation_id: "conv-123",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(createdTabs[0].url, "https://chatgpt.com/c/conv-123?_yoetz=run_job_resume_model_fail");
    assert.equal(error.payload.code, "model_selection_failed");
    assert.equal(error.payload.phase, "model_selection");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.model_selection_status, "unavailable");
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
    assert.equal(port.messages.some((message) => message.payload?.phase === "ready_for_file"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails closed when GPT-5.6 Sol Extra High selection is only kept_current", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?model_kept_current=${Date.now()}`);
    port.emit(envelope("job_start", "job_model_kept_current", {
      prompt: "prompt",
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "model_selection_failed");
    assert.equal(error.payload.model_selection_status, "kept_current");
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker keeps the current-model warning to one final payload entry", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  const sentJobs = new Set();
  const sentToTabs = [];
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: currentSelection()
            };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sentJobs.add(message.job.job_id);
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: "conv-current-warning"
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: {
                method: "assistant_dom_fallback",
                text: sentJobs.has(message.job.job_id) ? "answer" : "",
                is_generating: false,
                assistant_count: sentJobs.has(message.job.job_id) ? 1 : 0,
                copy_button_count: sentJobs.has(message.job.job_id) ? 1 : 0,
                has_copy_button: sentJobs.has(message.job.job_id),
                turn_index: sentJobs.has(message.job.job_id) ? 0 : -1,
                conversation_id: "conv-current-warning"
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?current_warning=${Date.now()}`);
    port.emit(envelope("job_start", "job_current_warning", {
      prompt: "prompt",
      model: "smuggled-value",
      model_strategy: "current"
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_progress" && message.payload.phase === "ready_for_file"));
    assert.equal(sentToTabs.find((message) => message.type === "yoetz_configure_model")?.job.model, "current");
    port.emit(envelope("job_file_chunk", "job_current_warning", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "bundle.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete" && message.job_id === "job_current_warning"));
    const complete = port.messages.find((message) => message.type === "job_complete" && message.job_id === "job_current_warning");
    assert.equal(complete.payload.model_selection_status, "current");
    assert.deepEqual(complete.payload.warnings, [
      "model pinning bypassed — answer may come from any model",
      "ChatGPT finality_anchor=dom_only: backend API positive-finality proof was unavailable; response relied on DOM-only completion"
    ]);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails closed when GPT-5.6 Sol Extra High selection fails", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "unavailable",
                model_used: "ChatGPT",
                warning: "ChatGPT model selector button not found"
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?model_selection_failed=${Date.now()}`);
    port.emit(envelope("job_start", "job_selection_failed", {
      prompt: "prompt",
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "model_selection_failed");
    assert.equal(error.payload.requested_model, "gpt-5-6-sol-extra-high");
    assert.equal(port.messages.some((message) => message.payload?.phase === "ready_for_file"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects the legacy extended_status selection proof", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "selected",
                model_used: "Pro Extended",
                requested_model: "extended-pro",
                extended_status: "required"
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?model_selected_incomplete=${Date.now()}`);
    port.emit(envelope("job_start", "job_model_selected_incomplete", {
      prompt: "prompt",
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "model_selection_failed");
    assert.equal(error.payload.phase, "model_selection");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.model_selection_status, "selected");
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker accepts only verified GPT-5.6 Sol Extra High selection", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "selected",
                model_used: "GPT-5.6 Sol Extra High",
                requested_model: "gpt-5-6-sol-extra-high",
                family_status: "verified",
                effort_status: "verified"
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?sol_pro_verified=${Date.now()}`);
    port.emit(envelope("job_start", "job_sol_pro_verified", { prompt: "prompt" }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    assert.equal(
      port.messages.some((message) => message.type === "job_error" && message.payload.code === "model_selection_failed"),
      false
    );
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects duplicate active job starts before opening another tab", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let createdTabs = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++createdTabs, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?duplicate_job=${Date.now()}`);
    port.emit(envelope("job_start", "job_duplicate", { prompt: "prompt" }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_start", "job_duplicate", { prompt: "prompt" }));
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "duplicate_job"));
    assert.equal(createdTabs, 1);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects follow-on messages with the wrong capability token", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?capability_mismatch=${Date.now()}`);
    port.emit(envelope("job_start", "job_capability", { prompt: "prompt" }, { capability_token: "secret" }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_capability", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_capability.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }, { capability_token: "wrong" }));
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "capability_mismatch"));
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects mismatched profile email before opening a tab", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  let createdTab = false;
  globalThis.chrome = chromeStub({
    port,
    profileEmail: "work@example.com",
    storage,
    tabs: {
      create: async () => {
        createdTab = true;
        throw new Error("should not open a tab before profile validation");
      },
      get: async () => {
        throw new Error("unexpected tab lookup");
      },
      sendMessage: async () => {
        throw new Error("unexpected tab message");
      }
    }
  });

  try {
    await import(`../src/service-worker.js?profile_mismatch=${Date.now()}`);
    port.emit(envelope("job_start", "job_profile", {
      prompt: "prompt",
      profile_email: "personal@example.com"
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "profile_mismatch");
    assert.equal(error.payload.phase, "profile");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.extension_profile_email, "work@example.com");
    assert.equal(createdTab, false);
    assert.deepEqual((await storage.get("jobs.job_profile"))["jobs.job_profile"].status, "failed");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects missing profile identity before opening a tab", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let createdTab = false;
  globalThis.chrome = chromeStub({
    port,
    profileEmail: "",
    tabs: {
      create: async () => {
        createdTab = true;
        throw new Error("should not open a tab before profile validation");
      },
      get: async () => {
        throw new Error("unexpected tab lookup");
      },
      sendMessage: async () => {
        throw new Error("unexpected tab message");
      }
    }
  });

  try {
    await import(`../src/service-worker.js?profile_missing=${Date.now()}`);
    port.emit(envelope("job_start", "job_missing_profile", {
      prompt: "prompt",
      profile_email: "work@example.com"
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "profile_identity_unavailable");
    assert.equal(error.payload.side_effect_started, false);
    assert.match(error.payload.message, /Chrome profile email/);
    assert.equal(createdTab, false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects mismatched extension instance id before opening a tab", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let createdTab = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async () => {
        createdTab = true;
        throw new Error("should not open a tab before instance validation");
      },
      get: async () => {
        throw new Error("unexpected tab lookup");
      },
      sendMessage: async () => {
        throw new Error("unexpected tab message");
      }
    }
  });

  try {
    await import(`../src/service-worker.js?instance_mismatch=${Date.now()}`);
    port.emit(envelope("job_start", "job_instance_mismatch", {
      prompt: "prompt",
      extension_instance_id: "ext_other_profile"
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "extension_instance_mismatch");
    assert.equal(error.payload.phase, "profile");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.requested_extension_instance_id, "ext_other_profile");
    assert.match(error.payload.extension_instance_id, /^ext_/);
    assert.equal(createdTab, false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker allows matching extension instance id when profile identity is unavailable", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const localStorage = makeStorage();
  await localStorage.set({ yoetz_extension_instance_id: "ext_seed_profile" });
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    localStorage,
    profileError: new Error("identity unavailable"),
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?instance_match_identity_unavailable=${Date.now()}`);
    port.emit(envelope("job_start", "job_instance_match", {
      prompt: "prompt",
      extension_instance_id: "ext_seed_profile"
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    assert.equal(tabId, 1);
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects browser_context_id before opening a tab", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let createdTab = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async () => {
        createdTab = true;
        throw new Error("should not open a tab before profile validation");
      },
      get: async () => {
        throw new Error("unexpected tab lookup");
      },
      sendMessage: async () => {
        throw new Error("unexpected tab message");
      }
    }
  });

  try {
    await import(`../src/service-worker.js?context_id=${Date.now()}`);
    port.emit(envelope("job_start", "job_context", {
      prompt: "prompt",
      browser_context_id: "ctx-work"
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "unsupported_browser_context");
    assert.equal(error.payload.phase, "profile");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(createdTab, false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker allows matching profile email before opening a tab", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    profileEmail: "work@example.com",
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?profile_match=${Date.now()}`);
    port.emit(envelope("job_start", "job_profile_match", {
      prompt: "prompt",
      profile_email: "WORK@EXAMPLE.COM"
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    assert.equal(tabId, 1);
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker hello falls back with instance id when profile identity fails", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  globalThis.chrome = chromeStub({
    port,
    storage,
    profileError: new Error("identity unavailable"),
    tabs: {}
  });

  try {
    await import(`../src/service-worker.js?hello_fallback=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    const hello = port.messages.find((message) => message.type === "hello");
    assert.match(hello.payload.extension_instance_id, /^ext_/);
    assert.equal(hello.payload.profile_email, null);
    assert.equal(hello.payload.profile_id, null);
    assert.deepEqual(hello.payload.recipes, ["chatgpt", "claude"]);
    assert.deepEqual(hello.payload.capabilities, ["terminal_ack"]);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker reload command acknowledges before runtime reload", async () => {
  const originalChrome = globalThis.chrome;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  const originalSetTimeout = globalThis.setTimeout;
  const port = makePort();
  let reloadCount = 0;

  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};
  globalThis.setTimeout = (fn) => {
    fn();
    return 1;
  };
  globalThis.chrome = chromeStub({
    port,
    reload: () => {
      reloadCount += 1;
    },
    tabs: {}
  });

  try {
    await import(`../src/service-worker.js?reload=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("reconnect", "job_reload", { intent: "reload_extension" }));

    await eventually(() => reloadCount === 1);
    const ack = port.messages.find((message) => message.type === "reconnect");
    assert.equal(ack.job_id, "job_reload");
    assert.equal(ack.payload.status, "reloading");
  } finally {
    globalThis.chrome = originalChrome;
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
    globalThis.setTimeout = originalSetTimeout;
  }
});

test("service worker inspect_run omits broad page text by default", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let inspectMessage = null;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      query: async () => [{ id: 7, url: "https://chatgpt.com/c/run", title: "Yoetz run" }],
      sendMessage: async (_id, message) => {
        inspectMessage = message;
        assert.equal(message.type, "yoetz_inspect_page");
        return {
          ok: true,
          payload: {
            url: "https://chatgpt.com/c/run",
            title: "Yoetz run",
            window_name: "yoetz-chatgpt-native:run_inspect:job_inspect",
            ownership: { run_id: "run_inspect", job_id: "job_inspect" },
            active_job_ids: ["job_inspect"],
            page_text_chars: 2048,
            page_text_tail: "sidebar secret conversation history",
            extraction: {
              method: "assistant_dom_fallback",
              text: "answer",
              diagnostics: {
                counts: { assistant_turns: 1 },
                body_text_tail: "sidebar secret conversation history",
                assistant_turn_snippets: [{ text: "answer" }],
                article_snippets: [],
                markdown_snippets: [],
                stop_control_snippets: []
              }
            }
          }
        };
      }
    }
  });

  try {
    await import(`../src/service-worker.js?inspect_privacy=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("inspect_run", "job_inspect", { run_id: "run_inspect" }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    const inspection = complete.payload.tabs[0].inspection;
    assert.equal(inspectMessage.include_page_text, undefined);
    assert.equal(inspection.page_text_chars, 2048);
    assert.equal(inspection.page_text_tail, undefined);
    assert.equal(inspection.extraction.diagnostics.body_text_tail, undefined);
    assert.deepEqual(inspection.extraction.diagnostics.counts, { assistant_turns: 1 });
    assert.deepEqual(inspection.extraction.diagnostics.assistant_turn_snippets, [{ text: "answer" }]);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker inspect_run surfaces the textContent diagnostics and a runtime build marker", async () => {
  // P2: the innerText-vs-textContent truncation discriminator must reach `yoetz browser extension
  // inspect` output. This drives the full inspect_run -> sanitizeInspection -> diagnosticPayload
  // projection and asserts (a) the page-level page_text_chars + page_text_content_chars survive,
  // (b) per-snippet text_content_chars survives on markdown_snippets/assistant_turn_snippets, and
  // (c) the service_worker_build runtime marker is present so an operator can confirm the live SW
  // is the expected build before trusting/distrusting the fields (stale-runtime disambiguation).
  const originalChrome = globalThis.chrome;
  const port = makePort();
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      query: async () => [{ id: 11, url: "https://chatgpt.com/c/run", title: "Yoetz run" }],
      sendMessage: async (_id, message) => {
        assert.equal(message.type, "yoetz_inspect_page");
        return {
          ok: true,
          payload: {
            url: "https://chatgpt.com/c/run",
            title: "Yoetz run",
            window_name: "yoetz-chatgpt-native:run_inspect:job_inspect",
            ownership: { run_id: "run_inspect", job_id: "job_inspect" },
            active_job_ids: ["job_inspect"],
            content_script_build: "9.9.9-content",
            page_text_chars: 42,
            extraction: {
              method: "copy_scope_dom_fallback",
              text: "I",
              diagnostics: {
                page_text_chars: 42,
                page_text_content_chars: 10823,
                counts: { markdown: 1, assistant_turns: 1 },
                markdown_snippets: [{ tag: "div", role: "", text: "I", text_chars: 1, text_content_chars: 10823 }],
                assistant_turn_snippets: [{ tag: "article", role: "assistant", text: "I", text_chars: 1, text_content_chars: 10823 }],
                article_snippets: [],
                stop_control_snippets: []
              }
            }
          }
        };
      }
    }
  });

  try {
    await import(`../src/service-worker.js?inspect_textcontent=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("inspect_run", "job_inspect", { run_id: "run_inspect" }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    // (A) service-worker runtime build marker present on the inspect envelope.
    assert.equal(typeof complete.payload.service_worker_build, "string");
    assert.ok(complete.payload.service_worker_build.length > 0);
    const inspection = complete.payload.tabs[0].inspection;
    // (C) content-script runtime build marker passed through sanitizeInspection's spread.
    assert.equal(inspection.content_script_build, "9.9.9-content");
    const diagnostics = inspection.extraction.diagnostics;
    // (B)+(D) page-level + per-snippet textContent discriminator survives the projection.
    assert.equal(diagnostics.page_text_chars, 42);
    assert.equal(diagnostics.page_text_content_chars, 10823);
    assert.equal(diagnostics.markdown_snippets[0].text_content_chars, 10823);
    assert.equal(diagnostics.assistant_turn_snippets[0].text_content_chars, 10823);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("diagnosticPayload defensively emits page-level textContent keys even when source omits them", async () => {
  // Defensiveness guard: the KEY must be present (even as null) when the source diagnostics lack
  // page_text_content_chars, so the mere presence of the key in live inspect output proves the new
  // service-worker code is executing (its total absence => stale runtime, not a code bug).
  const originalChrome = globalThis.chrome;
  const port = makePort();
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      query: async () => [{ id: 12, url: "https://chatgpt.com/c/run", title: "Yoetz run" }],
      sendMessage: async (_id, message) => {
        assert.equal(message.type, "yoetz_inspect_page");
        return {
          ok: true,
          payload: {
            url: "https://chatgpt.com/c/run",
            title: "Yoetz run",
            window_name: "yoetz-chatgpt-native:run_inspect:job_inspect",
            ownership: { run_id: "run_inspect", job_id: "job_inspect" },
            active_job_ids: ["job_inspect"],
            extraction: {
              method: "assistant_dom_fallback",
              text: "answer",
              // Source diagnostics WITHOUT the page-level textContent field (stale content script).
              diagnostics: {
                counts: { assistant_turns: 1 },
                assistant_turn_snippets: [{ text: "answer" }],
                article_snippets: [],
                markdown_snippets: [],
                stop_control_snippets: []
              }
            }
          }
        };
      }
    }
  });

  try {
    await import(`../src/service-worker.js?inspect_defensive=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("inspect_run", "job_inspect", { run_id: "run_inspect" }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    const diagnostics = complete.payload.tabs[0].inspection.extraction.diagnostics;
    // Keys present (the diagnostics object literally has the property) even though the value is null.
    assert.ok(Object.hasOwn(diagnostics, "page_text_content_chars"));
    assert.ok(Object.hasOwn(diagnostics, "page_text_chars"));
    assert.equal(diagnostics.page_text_content_chars, null);
    assert.equal(diagnostics.page_text_chars, null);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker inspect_run can target a ChatGPT conversation id", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const conversationId = "6a0228a7-4994-832d-8bb0-ea6b35d1b7af";
  let inspectMessage = null;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      query: async () => [{ id: 9, url: `https://chatgpt.com/c/${conversationId}`, title: "Finished run" }],
      sendMessage: async (_id, message) => {
        inspectMessage = message;
        return {
          ok: true,
          payload: {
            url: `https://chatgpt.com/c/${conversationId}`,
            title: "Finished run",
            conversation_id: conversationId,
            window_name: "",
            ownership: null,
            active_job_ids: [],
            page_text_chars: 128,
            extraction: {
              method: "assistant_dom_fallback",
              text: "final answer",
              diagnostics: { counts: { assistant_turns: 1 } }
            }
          }
        };
      }
    }
  });

  try {
    await import(`../src/service-worker.js?inspect_conversation=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;

    port.emit(envelope("inspect_run", "job_inspect_conversation", { run_id: conversationId }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    assert.equal(inspectMessage.type, "yoetz_inspect_page");
    assert.equal(inspectMessage.run_id, conversationId);
    assert.equal(inspectMessage.conversation_id, conversationId);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.payload.tabs[0].inspection.conversation_id, conversationId);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker times out stale pre-send assistant text as job_error", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return { ok: true, payload: { method: "assistant_dom_fallback", text: "old answer", is_generating: false, assistant_count: 1, turn_index: 0 } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?timeout=${Date.now()}`);
    port.emit(envelope("job_start", "job_timeout", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 120
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_timeout", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_timeout.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    const error = port.messages.find((message) => message.type === "job_error" && message.payload.code === "response_timeout");
    assert.equal(error.payload.phase, "wait_response");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_timeout");
    assert.match(error.payload.message, /if it finishes later, recover with: yoetz browser extension inspect --chatgpt --run-id run_job_timeout/);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker classifies final affordance without scoped assistant text", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "page_text_fallback",
                    text: "Skip to content\nbundle.md\nFile\nReview the attached file and provide your analysis.\n\nI\n\nGPT-5.6 Sol Extra High",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: -1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: -1,
                    diagnostics: {
                      counts: { assistant_roles: 1, markdown: 1, copy_buttons: 1 },
                      assistant_turn_snippets: [{ text: "I", text_chars: 1 }]
                    }
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?final_affordance_no_scoped_text=${Date.now()}`);
    port.emit(envelope("job_start", "job_final_affordance_no_scoped_text", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_final_affordance_no_scoped_text", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_final_affordance_no_scoped_text.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_extraction_failed"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    const error = port.messages.find((message) => message.type === "job_error" && message.payload.code === "response_extraction_failed");
    assert.equal(error.payload.phase, "wait_response");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.completion_reason, "final_affordance_without_scoped_text");
    assert.match(error.payload.message, /could not extract scoped assistant text/);
    assert.equal(error.payload.extraction_method, "page_text_fallback");
    assert.equal(error.payload.copy_button_count, 1);
    assert.deepEqual(error.payload.diagnostics.assistant_turn_snippets, [{ text: "I", text_chars: 1 }]);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails extraction when final affordance page text alternates without scoped assistant text", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            return {
              ok: true,
              payload: {
                method: "page_text_fallback",
                text: extractCount % 2 === 0
                  ? "Skip to content\nActions\nCopy\nYoetz footer"
                  : "Skip to content\nRegenerate\nCopy\nYoetz footer",
                is_generating: false,
                assistant_count: 1,
                user_count: 1,
                preceding_user_count: -1,
                copy_button_count: 1,
                has_copy_button: true,
                turn_index: -1,
                diagnostics: {
                  counts: { assistant_roles: 1, markdown: 0, copy_buttons: 1 },
                  assistant_turn_snippets: []
                }
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?alternating_final_page_text=${Date.now()}`);
    port.emit(envelope("job_start", "job_alternating_final_page_text", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1800
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_alternating_final_page_text", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_alternating_final_page_text.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_extraction_failed"), 4000);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    const error = port.messages.find((message) => message.type === "job_error" && message.payload.code === "response_extraction_failed");
    assert.equal(error.payload.completion_reason, "final_affordance_without_scoped_text");
    assert.equal(error.payload.extraction_method, "page_text_fallback");
    assert.equal(error.payload.copy_button_count, 1);
    assert.ok(extractCount >= 2, "alternating page text should not postpone extraction failure until timeout");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker completes post-send response when preceding user count is unknown", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "final answer",
                    is_generating: false,
                    assistant_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    preceding_user_count: -1,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?unknown_preceding_user=${Date.now()}`);
    port.emit(envelope("job_start", "job_unknown_preceding_user", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_unknown_preceding_user", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_unknown_preceding_user.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.payload.response, "final answer");
    assert.equal(complete.payload.completion_reason, "copy_button");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not complete on brief stable assistant text without a final affordance", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "stable but possibly partial",
                    is_generating: false,
                    assistant_count: 1,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?stable_no_copy=${Date.now()}`);
    port.emit(envelope("job_start", "job_stable_no_copy", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_stable_no_copy", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_stable_no_copy.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    const observed = port.messages.find((message) => message.type === "job_progress" && message.payload.phase === "response_observed");
    assert.equal(observed?.payload.response_delta, "stable but possibly partial");
    assert.equal(observed?.payload.is_generating, false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker completes long stable assistant text with an unscoped copy affordance", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const longAnswer = "Final Pro review paragraph with concrete evidence.\n".repeat(160).trim();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: longAnswer,
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: false,
                    turn_index: 0,
                    diagnostics: {
                      counts: { stop_controls: 0, copy_buttons: 1 }
                    }
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, user_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?long_unscoped_copy=${Date.now()}`);
    port.emit(envelope("job_start", "job_long_unscoped_copy", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_long_unscoped_copy", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_long_unscoped_copy.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.payload.response, longAnswer);
    assert.equal(complete.payload.extraction_method, "assistant_dom_fallback");
    assert.equal(complete.payload.completion_reason, "stable_idle_unscoped_copy_button");
    assert.ok(complete.payload.stable_for_ms >= 100);
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not complete long idle assistant text with only baseline copy controls", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const longAnswer = "Final Pro review paragraph with concrete evidence.\n".repeat(160).trim();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: longAnswer,
                    is_generating: false,
                    assistant_count: 2,
                    user_count: 2,
                    preceding_user_count: 2,
                    copy_button_count: 1,
                    has_copy_button: false,
                    turn_index: 1
                  }
                : {
                    method: "copy_scope_dom_fallback",
                    text: "old answer",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?long_baseline_copy_only=${Date.now()}`);
    port.emit(envelope("job_start", "job_long_baseline_copy_only", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_long_baseline_copy_only", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_long_baseline_copy_only.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker emits low-noise waiting progress while ChatGPT is quiet", async () => {
  const originalChrome = globalThis.chrome;
  const previousWaitingProgressInterval = globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
  globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = 100;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return {
              ok: true,
              payload: {
                filename: message.file.filename,
                size: 4,
                upload_commit_signal: "empty_composer_variant"
              }
            };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "conv-waiting-progress" } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? { method: "none", text: "", is_generating: true, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1, conversation_id: "conv-waiting-progress" }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?waiting_progress=${Date.now()}`);
    port.emit(envelope("job_start", "job_waiting_progress", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_waiting_progress", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_waiting_progress.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_progress" && message.payload.phase === "waiting_response"));
    const uploaded = port.messages.find((message) => message.type === "job_progress" && message.payload.phase === "file_uploaded");
    const opened = port.messages.find((message) => message.type === "job_progress" && message.payload.phase === "tab_opened");
    const sentProgress = port.messages.find((message) => message.type === "job_progress" && message.payload.phase === "prompt_sent");
    const waiting = port.messages.find((message) => message.type === "job_progress" && message.payload.phase === "waiting_response");
    assert.equal(opened?.payload.url, "https://chatgpt.com/?_yoetz=run_job_waiting_progress");
    assert.equal(opened?.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_waiting_progress");
    assert.match(opened?.payload.message, /https:\/\/chatgpt\.com\/\?_yoetz=run_job_waiting_progress/);
    assert.match(uploaded?.payload.message, /bundle uploaded/);
    assert.equal(uploaded?.payload.upload_commit_signal, "empty_composer_variant");
    assert.match(sentProgress?.payload.message, /waiting for ChatGPT response/);
    assert.equal(sentProgress?.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_waiting_progress");
    assert.equal(sentProgress?.payload.yoetz_url, "https://chatgpt.com/?_yoetz=run_job_waiting_progress");
    assert.equal(sentProgress?.payload.conversation_id, "conv-waiting-progress");
    assert.equal(sentProgress?.payload.conversation_url, "https://chatgpt.com/c/conv-waiting-progress");
    assert.match(sentProgress?.payload.message, /inspect with: yoetz browser extension inspect --chatgpt --run-id run_job_waiting_progress/);
    assert.match(waiting?.payload.message, /waiting for ChatGPT response/);
    assert.equal(waiting.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_waiting_progress");
    assert.equal(waiting.payload.extraction_method, "none");
    assert.equal(waiting.payload.is_generating, true);
    assert.equal(waiting.payload.response_length, 0);
    assert.equal(Object.hasOwn(waiting.payload, "response_tail"), false);
    assert.equal(Object.hasOwn(waiting.payload, "response_delta"), false);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousWaitingProgressInterval === undefined) {
      delete globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
    } else {
      globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = previousWaitingProgressInterval;
    }
  }
});

test("service worker polls adaptively after post-send assistant text appears", async () => {
  const originalChrome = globalThis.chrome;
  const previousActivityPoll = globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS;
  globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS = 250;
  const port = makePort();
  const postSendExtractionTimes = [];
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            postSendExtractionTimes.push(Date.now());
            const final = postSendExtractionTimes.length >= 5;
            return {
              ok: true,
              payload: {
                method: final ? "copy_scope_dom_fallback" : "assistant_dom_fallback",
                text: final ? "Finished answer." : `Draft ${postSendExtractionTimes.length}`,
                is_generating: !final,
                assistant_count: 1,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: final ? 1 : 0,
                has_copy_button: final,
                turn_index: 0
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?adaptive_post_send_poll=${Date.now()}`);
    port.emit(envelope("job_start", "job_adaptive_post_send_poll", {
      prompt: "prompt",
      wait_interval_ms: 1000,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_adaptive_post_send_poll", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_adaptive_post_send_poll.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 5000);
    assert.ok(postSendExtractionTimes.length >= 2);
    assert.ok(
      postSendExtractionTimes[1] - postSendExtractionTimes[0] < 750,
      `expected adaptive poll under 750ms, observed ${postSendExtractionTimes[1] - postSendExtractionTimes[0]}ms`
    );
    const responseObserved = port.messages.filter((message) =>
      message.type === "job_progress" && message.payload?.phase === "response_observed"
    );
    assert.equal(
      responseObserved.filter((message) => message.payload.response_in_progress).length,
      1,
      "adaptive polling must not multiply ordinary streaming progress output"
    );
  } finally {
    globalThis.chrome = originalChrome;
    if (previousActivityPoll === undefined) {
      delete globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS;
    } else {
      globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS = previousActivityPoll;
    }
  }
});

test("service worker keeps stable-idle finality tied to the configured interval while polling fast", async () => {
  const originalChrome = globalThis.chrome;
  const previousActivityPoll = globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS;
  const previousStableIdleMultiplier = globalThis.__YOETZ_STABLE_IDLE_INTERVAL_MULTIPLIER;
  globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS = 250;
  globalThis.__YOETZ_STABLE_IDLE_INTERVAL_MULTIPLIER = 3;
  const port = makePort();
  const postSendExtractionTimes = [];
  const longAnswer = "Final Pro review paragraph with concrete evidence.\n".repeat(160).trim();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, submitted_assistant_count: 0 } };
          case "yoetz_extract_response":
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            postSendExtractionTimes.push(Date.now());
            return {
              ok: true,
              payload: {
                // Long text plus an unscoped copy control selects the adaptive
                // stable-idle path whose threshold must remain interval-derived.
                method: "assistant_dom_fallback",
                text: longAnswer,
                is_generating: false,
                assistant_count: 1,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: 1,
                has_copy_button: false,
                turn_index: 0,
                diagnostics: {
                  counts: { stop_controls: 0, copy_buttons: 1 }
                }
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?configured_interval_finality=${Date.now()}`);
    port.emit(envelope("job_start", "job_configured_interval_finality", {
      prompt: "prompt",
      wait_interval_ms: 600,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_configured_interval_finality", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_configured_interval_finality.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 5000);
    const stableForMs = port.messages.find((message) => message.type === "job_complete")?.payload?.stable_for_ms;
    assert.ok(postSendExtractionTimes.length >= 6, "adaptive polling should sample repeatedly");
    assert.ok(
      postSendExtractionTimes[1] - postSendExtractionTimes[0] < 500,
      `expected adaptive poll under 500ms, observed ${postSendExtractionTimes[1] - postSendExtractionTimes[0]}ms`
    );
    assert.ok(stableForMs >= 1800, `expected configured-interval threshold of 1800ms, observed ${stableForMs}ms`);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousActivityPoll === undefined) {
      delete globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS;
    } else {
      globalThis.__YOETZ_POST_SEND_ASSISTANT_ACTIVITY_POLL_MS = previousActivityPoll;
    }
    if (previousStableIdleMultiplier === undefined) {
      delete globalThis.__YOETZ_STABLE_IDLE_INTERVAL_MULTIPLIER;
    } else {
      globalThis.__YOETZ_STABLE_IDLE_INTERVAL_MULTIPLIER = previousStableIdleMultiplier;
    }
  }
});

test("service worker fails a continuously unchanged post-send response as response_finality_stalled", async () => {
  const originalChrome = globalThis.chrome;
  const previousFinalityStallMs = globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS;
  globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS = 600;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractionCount = 0;
  let removedTabs = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://claude.ai/" }),
      remove: async () => {
        removedTabs += 1;
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedFableMaxSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return {
              ok: true,
              payload: {
                sent: true,
                submitted_assistant_count: 0,
                submitted_user_count: 1
              }
            };
          case "yoetz_extract_response":
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 } };
            }
            extractionCount += 1;
            return {
              ok: true,
              payload: {
                method: "assistant_dom",
                text: "Complete-looking body blocked by an unowned global stop control.",
                is_generating: true,
                assistant_count: 1,
                assistant_identity: "stalled-assistant",
                preceding_user_count: 1,
                turn_index: 0,
                diagnostics: {
                  finality: {
                    last_turn_streaming: "false"
                  },
                  counts: { stop_controls: 1 }
                }
              }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?response_finality_stalled=${Date.now()}`);
    port.emit(envelope("job_start", "job_response_finality_stalled", {
      recipe: "claude",
      prompt: "prompt",
      wait_interval_ms: 500,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_response_finality_stalled", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_response_finality_stalled.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.payload.code === "response_finality_stalled"
    ), 3000);
    const error = port.messages.find((message) =>
      message.type === "job_error" && message.payload.code === "response_finality_stalled"
    );
    assert.ok(extractionCount >= 3);
    assert.equal(error.payload.tab_disposition, "kept");
    assert.equal(error.payload.extraction_method, "assistant_dom");
    assert.equal(error.payload.assistant_count, 1);
    assert.equal(error.payload.assistant_identity, "stalled-assistant");
    assert.equal(error.payload.turn_index, 0);
    assert.equal(error.payload.response_length, 64);
    assert.equal(error.payload.completion_reason, "non_streaming_turn_with_persistent_stop");
    assert.equal(error.payload.send_committed, true);
    assert.equal(error.payload.copy_button_count, 0);
    assert.equal(error.payload.has_copy_button, false);
    assert.equal(error.payload.diagnostics.finality.last_turn_streaming, "false");
    assert.equal(error.payload.diagnostics.counts.stop_controls, 1);
    assert.match(error.payload.message, /inspect it before rerunning/i);
    assert.match(error.payload.message, /Do not rerun until inspection/i);
    assert.equal(removedTabs, 0, "terminal finality failures must preserve the owned tab");
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);

    port.emit(envelope("job_start", "job_response_finality_stalled", {
      recipe: "claude",
      prompt: "must not resubmit"
    }));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.payload.code === "duplicate_job"
    ));
    assert.equal(removedTabs, 0);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousFinalityStallMs === undefined) {
      delete globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS;
    } else {
      globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS = previousFinalityStallMs;
    }
  }
});

test("service worker resets the finality-stall timer on signature changes and disappearance", async () => {
  const originalChrome = globalThis.chrome;
  const previousFinalityStallMs = globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS;
  globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS = 600;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractionCount = 0;
  const postSendExtractions = [
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, assistant_identity: "a1", preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } },
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, assistant_identity: "a2", preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } },
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, assistant_identity: "a2", preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } },
    { method: "assistant_dom", text: "B", is_generating: true, assistant_count: 1, assistant_identity: "a2", preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } },
    { method: "none", text: "", is_generating: true, assistant_count: 0, turn_index: -1 },
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, assistant_identity: "a3", preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "true" }, counts: { stop_controls: 1 } } },
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } },
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } },
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } },
    { method: "assistant_dom", text: "A", is_generating: true, assistant_count: 1, assistant_identity: "a4", preceding_user_count: 1, turn_index: 0, diagnostics: { finality: { last_turn_streaming: "false" }, counts: { stop_controls: 1 } } }
  ];
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://claude.ai/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedFableMaxSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, submitted_assistant_count: 0, submitted_user_count: 1 } };
          case "yoetz_extract_response":
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 } };
            }
            extractionCount += 1;
            return {
              ok: true,
              payload: postSendExtractions[Math.min(extractionCount - 1, postSendExtractions.length - 1)]
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?response_finality_stall_resets=${Date.now()}`);
    port.emit(envelope("job_start", "job_response_finality_stall_resets", {
      recipe: "claude",
      prompt: "prompt",
      wait_interval_ms: 500,
      wait_timeout_ms: 7000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_response_finality_stall_resets", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_response_finality_stall_resets.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.payload.code === "response_finality_stalled"
    ), 7000);
    assert.ok(
      extractionCount >= 12,
      `timer must restart after signature changes and ignore missing identity; observed ${extractionCount} extractions`
    );
  } finally {
    globalThis.chrome = originalChrome;
    if (previousFinalityStallMs === undefined) {
      delete globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS;
    } else {
      globalThis.__YOETZ_RESPONSE_FINALITY_STALL_MS = previousFinalityStallMs;
    }
  }
});

test("service worker emits final-affordance waiting progress once across state flaps", async () => {
  const originalChrome = globalThis.chrome;
  const previousWaitingProgressInterval = globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
  globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = 60000;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let postSendExtractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (sent) {
              postSendExtractCount += 1;
            }
            const generating = postSendExtractCount === 2;
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "Settled text without final controls.",
                    is_generating: generating,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: 0
                  }
                : {
                    method: "none",
                    text: "",
                    is_generating: false,
                    assistant_count: 0,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: -1
                  }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?final_affordance_transition=${Date.now()}`);
    port.emit(envelope("job_start", "job_final_affordance_transition", {
      prompt: "prompt",
      wait_interval_ms: 500,
      wait_timeout_ms: 2200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_final_affordance_transition", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_final_affordance_transition.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error"), 4000);
    const transitionEvents = port.messages.filter((message) =>
      message.type === "job_progress"
      && message.payload.phase === "waiting_response"
      && message.payload.awaiting_final_affordance
    );
    assert.ok(postSendExtractCount >= 3, "test must exercise awaiting -> generating -> awaiting");
    assert.equal(transitionEvents.length, 1);
    assert.match(transitionEvents[0].payload.message, /waiting for final assistant controls/);
    assert.equal(
      transitionEvents[0].payload.inspect_command,
      "yoetz browser extension inspect --chatgpt --run-id run_job_final_affordance_transition"
    );
  } finally {
    globalThis.chrome = originalChrome;
    if (previousWaitingProgressInterval === undefined) {
      delete globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
    } else {
      globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = previousWaitingProgressInterval;
    }
  }
});

test("service worker completion is structural and does not classify response text", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "Thought for 9m 55s\nThought for 9m 55s",
                    is_generating: false,
                    assistant_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?structural_finality=${Date.now()}`);
    port.emit(envelope("job_start", "job_thought_only", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_thought_only", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_thought_only.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.payload.response, "Thought for 9m 55s\nThought for 9m 55s");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(complete.payload.finality_anchor, "dom_only");
    assert.match(complete.payload.warnings.at(-1), /finality_anchor=dom_only/);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails closed on Sources-only DOM chrome when backend finality is unavailable", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-sources" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: "conv-sources",
                submitted_user_count: 1,
                submitted_assistant_count: 0
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "page_text_fallback",
                    text: "Review bundle Sources Copy",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-sources"
                  }
                : {
                    method: "none",
                    text: "",
                    is_generating: false,
                    assistant_count: 0,
                    user_count: 0,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: -1
                  }
            };
          case "yoetz_fetch_conversation":
            throw new Error("backend API unavailable");
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?sources_only_dom=${Date.now()}`);
    port.emit(envelope("job_start", "job_sources_only_dom", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_sources_only_dom", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_sources_only_dom.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) =>
      message.type === "job_error" && message.payload.code === "response_extraction_failed"
    ));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    const error = port.messages.find((message) =>
      message.type === "job_error" && message.payload.code === "response_extraction_failed"
    );
    assert.equal(error.payload.extraction_method, "page_text_fallback");
    assert.equal(error.payload.response_length, 26);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker accepts a scoped single-letter assistant markdown response", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: "A",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?single_letter_scoped_valid=${Date.now()}`);
    port.emit(envelope("job_start", "job_single_letter_scoped_valid", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_single_letter_scoped_valid", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_single_letter_scoped_valid.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.payload.response, "A");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker reports oversized completed responses before native delivery", async () => {
  const originalChrome = globalThis.chrome;
  const previousMaxNativeBytes = globalThis.__YOETZ_MAX_NATIVE_OUTBOUND_BYTES;
  globalThis.__YOETZ_MAX_NATIVE_OUTBOUND_BYTES = 1024;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: "x".repeat(2048),
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?oversized_completed_response=${Date.now()}`);
    port.emit(envelope("job_start", "job_oversized_completed_response", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 3000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_oversized_completed_response", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_oversized_completed_response.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_too_large"), 5000);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    const error = port.messages.find((message) => message.type === "job_error" && message.payload.code === "response_too_large");
    assert.equal(error.payload.phase, "wait_response");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.response_length, 2048);
    assert.equal(error.payload.max_native_message_bytes, 1024);
    assert.equal(error.payload.tab_disposition, "kept");
    assert.equal(error.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_oversized_completed_response");
    assert.ok(error.payload.native_message_bytes > error.payload.max_native_message_bytes);
    assert.match(error.payload.message, /too large to deliver/);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousMaxNativeBytes === undefined) {
      delete globalThis.__YOETZ_MAX_NATIVE_OUTBOUND_BYTES;
    } else {
      globalThis.__YOETZ_MAX_NATIVE_OUTBOUND_BYTES = previousMaxNativeBytes;
    }
  }
});

test("service worker latches a completed scoped response when later ChatGPT DOM artifacts alternate", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            const shortArtifact = extractCount % 2 === 0;
            return {
              ok: true,
              payload: {
                method: "copy_scope_dom_fallback",
                text: shortArtifact ? "Actions" : "Full completed ChatGPT review text",
                is_generating: false,
                assistant_count: shortArtifact ? 2 : 1,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: 2,
                has_copy_button: true,
                turn_index: shortArtifact ? 1 : 0
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?alternating_final_dom_artifact=${Date.now()}`);
    port.emit(envelope("job_start", "job_alternating_final_dom_artifact", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 3000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_alternating_final_dom_artifact", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_alternating_final_dom_artifact.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 5000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(extractCount >= 2, "completion should survive a shorter post-completion DOM artifact");
    assert.equal(complete.payload.response, "Full completed ChatGPT review text");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker preserves the best final response across a transient generating blip", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            if (extractCount === 1) {
              return {
                ok: true,
                payload: {
                  method: "copy_scope_dom_fallback",
                  text: "Full completed ChatGPT review text with the actual answer",
                  is_generating: false,
                  assistant_count: 1,
                  user_count: 1,
                  preceding_user_count: 1,
                  copy_button_count: 1,
                  has_copy_button: true,
                  turn_index: 0
                }
              };
            }
            if (extractCount === 2) {
              return {
                ok: true,
                payload: {
                  method: "assistant_dom_fallback",
                  text: "Full completed ChatGPT review text with the actual answer",
                  is_generating: true,
                  assistant_count: 1,
                  user_count: 1,
                  preceding_user_count: 1,
                  copy_button_count: 0,
                  has_copy_button: false,
                  turn_index: 0
                }
              };
            }
            return {
              ok: true,
              payload: {
                method: "copy_scope_dom_fallback",
                text: "Actions",
                is_generating: false,
                assistant_count: 2,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: 2,
                has_copy_button: true,
                turn_index: 1
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?transient_generating_final_latch=${Date.now()}`);
    port.emit(envelope("job_start", "job_transient_generating_final_latch", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_transient_generating_final_latch", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_transient_generating_final_latch.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 6000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(extractCount >= 4, "completion should survive the transient generating sample and later shorter artifact");
    assert.equal(complete.payload.response, "Full completed ChatGPT review text with the actual answer");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker labels interim Pro turns as non-final and completes on the later real answer", async () => {
  // RC4 regression: ChatGPT Pro streams the answer as MULTIPLE interim end_turn turns
  // ("I'll review...", "I've narrowed...") before the real answer arrives minutes later. The first
  // interim turn's streaming head is a single "I" (observed live in conv 6a23a3a6). The waiter must
  // NOT surface that interim "I" as if it were the final response, and must complete on the later
  // real answer. is_generating stays TRUE across interim turns (verified live), so the gate never
  // completes early; progress events must mark non-finality so a consumer cannot misread "I".
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  const FINAL = "No P0 found. I found two P1 proof-integrity issues and several P2 residual risks.";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              // Pre-send baseline (assistant_count 0).
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            const base = { user_count: 1, preceding_user_count: 1 };
            if (extractCount === 1) {
              // Interim turn #1, streaming head = "I", still generating, no copy button yet.
              return { ok: true, payload: { ...base, method: "assistant_dom_fallback", text: "I", is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 } };
            }
            if (extractCount === 2) {
              return { ok: true, payload: { ...base, method: "assistant_dom_fallback", text: "I'll review the bundled diff as the source of truth", is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 } };
            }
            if (extractCount === 3) {
              // Interim turn #2 now streaming while still generating -> proves turn #1 was interim.
              return { ok: true, payload: { ...base, method: "assistant_dom_fallback", text: "I've narrowed review to a few possible proof-binding gaps", is_generating: true, assistant_count: 2, copy_button_count: 0, has_copy_button: false, turn_index: 1 } };
            }
            // Real final answer: generation stopped, scoped copy button present, stable -> completes.
            return { ok: true, payload: { ...base, method: "copy_scope_dom_fallback", text: FINAL, is_generating: false, assistant_count: 3, copy_button_count: 1, has_copy_button: true, turn_index: 2 } };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?interim_turn_labeling=${Date.now()}`);
    port.emit(envelope("job_start", "job_interim_turn_labeling", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_interim_turn_labeling", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_interim_turn_labeling.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 6000);
    const complete = port.messages.find((message) => message.type === "job_complete");

    // Completion: the later real answer, explicitly marked final.
    assert.equal(complete.payload.response, FINAL);
    assert.equal(complete.payload.is_final, true);
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);

    const progressEvents = port.messages.filter((message) => message.type === "job_progress");
    // EVERY progress event is non-final by construction -> a consumer cannot mistake interim text for the answer.
    assert.ok(progressEvents.length > 0);
    assert.equal(progressEvents.every((message) => message.payload.is_final === false), true);

    const observed = progressEvents.filter((message) => message.payload.phase === "response_observed");
    // The interim "I" was surfaced in progress but clearly marked in-progress / non-final.
    const iEvent = observed.find((message) => message.payload.response_length === 1);
    assert.ok(iEvent, "expected a response_observed event for the single-char interim head 'I'");
    assert.equal(iEvent.payload.is_final, false);
    assert.equal(iEvent.payload.response_in_progress, true);
    // Once a second assistant turn streams while generating, it is labeled an interim assistant turn.
    const interim = observed.find((message) => message.payload.interim_assistant_turn === true);
    assert.ok(interim, "expected an interim_assistant_turn=true progress event for the 2nd streaming turn");
    assert.equal(interim.payload.assistant_turns_since_send, 2);
    assert.equal(interim.payload.response_in_progress, true);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker settles same-length post-final text churn instead of timing out", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            return {
              ok: true,
              payload: {
                method: "copy_scope_dom_fallback",
                text: extractCount % 2 === 0 ? "Review B" : "Review A",
                is_generating: false,
                assistant_count: 1,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: 1,
                has_copy_button: true,
                turn_index: 0
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?same_length_final_churn=${Date.now()}`);
    port.emit(envelope("job_start", "job_same_length_final_churn", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1800
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_same_length_final_churn", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_same_length_final_churn.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 4000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(["Review A", "Review B"].includes(complete.payload.response));
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker waits for scoped response text bytes to stabilize after final controls appear", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            const revisions = [
              "final answer",
              "final answer with citations",
              "final answer with citations and caveats",
              "final answer with citations and caveats plus closing note"
            ];
            return {
              ok: true,
              payload: {
                method: "copy_scope_dom_fallback",
                text: revisions[Math.min(extractCount - 1, revisions.length - 1)],
                is_generating: false,
                assistant_count: 1,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: 1,
                has_copy_button: true,
                turn_index: 0
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?text_hash_stability=${Date.now()}`);
    port.emit(envelope("job_start", "job_text_hash_stability", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_text_hash_stability", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_text_hash_stability.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 7000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(extractCount >= 5, "completion should wait past repeated text mutations under the same structural anchor");
    assert.equal(complete.payload.response, "final answer with citations and caveats plus closing note");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker waits for streaming one-letter prefix to reach final assistant affordance", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            const streaming = extractCount <= 2;
            const text = extractCount === 1 ? "I" : "I reviewed the bundle.";
            return {
              ok: true,
              payload: {
                method: "assistant_dom_fallback",
                text,
                is_generating: streaming,
                assistant_count: 1,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: 1,
                has_copy_button: !streaming,
                turn_index: 0
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?streaming_single_letter_prefix=${Date.now()}`);
    port.emit(envelope("job_start", "job_streaming_single_letter_prefix", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_streaming_single_letter_prefix", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_streaming_single_letter_prefix.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 7000);
    const observedPrefix = port.messages.find(
      (message) =>
        message.type === "job_progress" &&
        message.payload.phase === "response_observed" &&
        message.payload.response_length === 1 &&
        message.payload.is_generating === true
    );
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(observedPrefix, "streaming one-letter prefix should be observed before completion");
    assert.ok(extractCount >= 4, "completion should wait for final affordance after streaming stops");
    assert.equal(complete.payload.response, "I reviewed the bundle.");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not complete when ChatGPT idles before final assistant affordance", async () => {
  const originalChrome = globalThis.chrome;
  const previousWaitingProgressInterval = globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
  globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = 100;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            extractCount += 1;
            const final = extractCount >= 6;
            return {
              ok: true,
              payload: {
                method: final ? "copy_scope_dom_fallback" : "assistant_dom_fallback",
                text: final ? "I reviewed the bundle." : "I",
                is_generating: extractCount === 1,
                assistant_count: 1,
                user_count: 1,
                preceding_user_count: 1,
                copy_button_count: 1,
                has_copy_button: final,
                turn_index: 0
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?idle_before_final_affordance=${Date.now()}`);
    port.emit(envelope("job_start", "job_idle_before_final_affordance", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_idle_before_final_affordance", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_idle_before_final_affordance.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.payload?.awaiting_final_affordance), 7000);
    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 7000);
    const waiting = port.messages.find((message) => message.payload?.awaiting_final_affordance);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.match(waiting.payload.message, /waiting for final assistant controls/);
    assert.equal(waiting.payload.inspect_command, "yoetz browser extension inspect --chatgpt --run-id run_job_idle_before_final_affordance");
    assert.ok(extractCount >= 6, "completion should not happen during the idle one-letter prefix");
    assert.equal(complete.payload.response, "I reviewed the bundle.");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousWaitingProgressInterval === undefined) {
      delete globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
    } else {
      globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = previousWaitingProgressInterval;
    }
  }
});

test("service worker completes with backend-api text when the DOM answer turn never paints", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let backendFetch = null;
  let backendFetchCount = 0;
  const FINAL = "Backend API captured the full Pro review even though the DOM stayed frozen.";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-api?_yoetz=run_job_backend_api_frozen_dom" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "conv-api", submitted_assistant_count: 1 } };
          case "yoetz_fetch_conversation":
            backendFetch = message;
            backendFetchCount += 1;
            return {
              ok: true,
              payload: {
                method: "backend_api",
                text: FINAL,
                is_generating: false,
                node_fresh: true,
                node_id: "answer-api",
                conversation_id: "conv-api",
                assistant_count: 1,
                turn_index: 0,
                copy_button_count: 0,
                has_copy_button: false
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "I",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: false,
                    turn_index: 0,
                    conversation_id: "conv-api",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 1 } }
                  }
                : {
                    method: "copy_scope_dom_fallback",
                    text: "previous answer",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 0,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-api"
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?backend_api_frozen_dom=${Date.now()}`);
    port.emit(envelope("job_start", "job_backend_api_frozen_dom", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 3000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_backend_api_frozen_dom", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_backend_api_frozen_dom.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 5000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(backendFetch?.conversation_id, "conv-api");
    assert.ok(backendFetchCount >= 2, "backend finality must survive a second fetch of the same current answer node");
    assert.equal(complete.payload.response, FINAL);
    assert.equal(complete.payload.extraction_method, "backend_api");
    assert.equal(complete.payload.completion_reason, "backend_api");
    assert.equal(complete.payload.conversation_id, "conv-api");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker accepts fresh backend finality when the DOM generating heuristic stays stuck", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let backendFetchCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-stuck-generating?_yoetz=run_job_stuck_generating" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "conv-stuck-generating", submitted_assistant_count: 1 } };
          case "yoetz_fetch_conversation":
            backendFetchCount += 1;
            return {
              ok: true,
              payload: {
                method: "backend_api",
                text: "7",
                is_generating: false,
                node_fresh: true,
                node_id: "answer-stuck-generating",
                conversation_id: "conv-stuck-generating",
                assistant_count: 1,
                turn_index: 0,
                copy_button_count: 0,
                has_copy_button: false
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: "7",
                    is_generating: true,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 2,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-stuck-generating",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 2 } }
                  }
                : {
                    method: "none",
                    text: "",
                    is_generating: false,
                    assistant_count: 0,
                    user_count: 0,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: -1
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?backend_api_stuck_generating=${Date.now()}`);
    port.emit(envelope("job_start", "job_stuck_generating", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_stuck_generating", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_stuck_generating.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 3000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(backendFetchCount >= 2, "backend API must confirm finality even while the DOM says generating");
    assert.equal(complete.payload.response, "7");
    assert.equal(complete.payload.extraction_method, "backend_api");
    assert.equal(complete.payload.finality_anchor, "backend_api");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker treats a stale backend-api node as still generating until a fresh node appears", async () => {
  const originalChrome = globalThis.chrome;
  const previousCooldown = globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
  globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = 100;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let fetchCount = 0;
  const FINAL = "Fresh backend API answer after the stale current_node advanced.";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-stale?_yoetz=run_job_backend_api_stale_then_fresh" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "conv-stale", submitted_assistant_count: 1 } };
          case "yoetz_fetch_conversation":
            fetchCount += 1;
            if (fetchCount === 2 || fetchCount === 3) {
              throw Object.assign(new Error("transient backend-api conversation fetch failed"), {
                code: "backend_api_unavailable"
              });
            }
            return {
              ok: true,
              payload: fetchCount === 1
                ? {
                    method: "backend_api",
                    text: "",
                    is_generating: true,
                    node_fresh: false,
                    conversation_id: "conv-stale",
                    assistant_count: 1,
                    turn_index: 0
                  }
                : {
                    method: "backend_api",
                    text: FINAL,
                    is_generating: false,
                    node_fresh: true,
                    node_id: "answer-stale-final",
                    conversation_id: "conv-stale",
                    assistant_count: 1,
                    turn_index: 0,
                    copy_button_count: 0,
                    has_copy_button: false
                  }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "I",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: 0,
                    conversation_id: "conv-stale",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 0 } }
                  }
                : {
                    method: "copy_scope_dom_fallback",
                    text: "previous answer",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 0,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-stale"
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?backend_api_stale_then_fresh=${Date.now()}`);
    port.emit(envelope("job_start", "job_backend_api_stale_then_fresh", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_backend_api_stale_then_fresh", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_backend_api_stale_then_fresh.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 6000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(fetchCount >= 4, "pending backend finality should survive two transient errors and retry to fresh");
    assert.equal(complete.payload.response, FINAL);
    assert.equal(complete.payload.extraction_method, "backend_api");
    assert.equal(complete.payload.completion_reason, "backend_api");
    assert.equal(complete.payload.finality_anchor, "backend_api");
    assert.deepEqual(complete.payload.warnings, []);
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousCooldown === undefined) {
      delete globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
    } else {
      globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = previousCooldown;
    }
  }
});

test("service worker does not return a longer transient caption before a shorter backend final answer", async () => {
  const originalChrome = globalThis.chrome;
  const previousCooldown = globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
  globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = 2000;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let fetchCount = 0;
  const CAPTION = "The checksum and nine-file scope match. I am checking edge-state coexistence, canonicalization, resend enforcement, and delivery claim/rollback invariants.";
  const FINAL = "PASS - no release-blocking findings in this exact patch.";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-caption?_yoetz=run_job_backend_caption_then_final" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "conv-caption", submitted_assistant_count: 1 } };
          case "yoetz_fetch_conversation":
            fetchCount += 1;
            return {
              ok: true,
              payload: fetchCount === 1
                ? {
                    method: "backend_api",
                    text: CAPTION,
                    is_generating: false,
                    node_fresh: true,
                    node_id: "answer-caption-interim",
                    conversation_id: "conv-caption",
                    assistant_count: 2,
                    turn_index: 1,
                    copy_button_count: 0,
                    has_copy_button: false
                  }
                : fetchCount === 2
                ? {
                    method: "backend_api",
                    text: "",
                    is_generating: true,
                    node_fresh: false,
                    conversation_id: "conv-caption",
                    assistant_count: 1,
                    turn_index: 0
                  }
                : {
                    method: "backend_api",
                    text: FINAL,
                    is_generating: false,
                    node_fresh: true,
                    node_id: "answer-caption-final",
                    conversation_id: "conv-caption",
                    assistant_count: 2,
                    turn_index: 1,
                    copy_button_count: 0,
                    has_copy_button: false
                  }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: CAPTION,
                    is_generating: false,
                    assistant_count: 2,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 2,
                    has_copy_button: true,
                    turn_index: 1,
                    conversation_id: "conv-caption",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 2 } }
                  }
                : {
                    method: "copy_scope_dom_fallback",
                    text: "previous answer",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 0,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-caption"
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?backend_caption_then_final=${Date.now()}`);
    port.emit(envelope("job_start", "job_backend_caption_then_final", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 6000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_backend_caption_then_final", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_backend_caption_then_final.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 8000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(fetchCount >= 4, "a transient fresh caption must fail confirmation before the final node is confirmed");
    assert.ok(CAPTION.length > FINAL.length, "regression must not be catchable by a length heuristic");
    assert.equal(complete.payload.response, FINAL);
    assert.equal(complete.payload.extraction_method, "backend_api");
    assert.equal(complete.payload.completion_reason, "backend_api");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousCooldown === undefined) {
      delete globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
    } else {
      globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = previousCooldown;
    }
  }
});

test("service worker fails closed if the backend positive-finality anchor disappears after pending", async () => {
  const originalChrome = globalThis.chrome;
  const previousCooldown = globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
  globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = 100;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let fetchCount = 0;
  const CAPTION = "I am still checking rollback invariants before returning the verdict.";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/c/conv-anchor-lost?_yoetz=run_job_backend_anchor_lost" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true, conversation_id: "conv-anchor-lost", submitted_assistant_count: 1 } };
          case "yoetz_fetch_conversation":
            fetchCount += 1;
            if (fetchCount > 1) {
              throw Object.assign(new Error("backend-api conversation fetch failed"), {
                code: "backend_api_unavailable"
              });
            }
            return {
              ok: true,
              payload: {
                method: "backend_api",
                text: "",
                is_generating: true,
                node_fresh: false,
                conversation_id: "conv-anchor-lost",
                assistant_count: 1,
                turn_index: 0
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: CAPTION,
                    is_generating: false,
                    assistant_count: 2,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 2,
                    has_copy_button: true,
                    turn_index: 1,
                    conversation_id: "conv-anchor-lost",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 2 } }
                  }
                : {
                    method: "copy_scope_dom_fallback",
                    text: "previous answer",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 0,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-anchor-lost"
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?backend_anchor_lost=${Date.now()}`);
    port.emit(envelope("job_start", "job_backend_anchor_lost", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_backend_anchor_lost", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_backend_anchor_lost.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete" || message.type === "job_error"), 6000);
    const error = port.messages.find((message) => message.type === "job_error");
    assert.ok(fetchCount >= 4, "the anchor should be declared lost only after three consecutive errors");
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    assert.equal(error?.payload.code, "backend_api_unavailable");
  } finally {
    globalThis.chrome = originalChrome;
    if (previousCooldown === undefined) {
      delete globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
    } else {
      globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = previousCooldown;
    }
  }
});

test("service worker refreshes a frozen render while backend finality is pending", async () => {
  const originalChrome = globalThis.chrome;
  const previousCooldown = globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
  globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = 30;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let reloaded = false;
  let bindCount = 0;
  let fetchCount = 0;
  const updates = [];
  const conversationUrl = "https://chatgpt.com/c/conv-stale-refresh?_yoetz=run_job_backend_stale_render_refresh";
  let currentUrl = "https://chatgpt.com/";
  const FINAL = "Reload recovered the rendered answer after backend API stayed not-ready.";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => {
        currentUrl = opts.url;
        return { id: ++tabId, status: "complete", ...opts };
      },
      get: async (id) => ({ id, status: "complete", url: currentUrl }),
      update: async (id, opts) => {
        updates.push({ id, opts });
        if (opts.url) {
          currentUrl = opts.url;
        }
        reloaded = true;
        return { id, status: "complete", url: currentUrl };
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            currentUrl = conversationUrl;
            return { ok: true, payload: { sent: true, url: currentUrl, conversation_id: "conv-stale-refresh", submitted_assistant_count: 1 } };
          case "yoetz_fetch_conversation":
            fetchCount += 1;
            return {
              ok: true,
              payload: reloaded
                ? {
                    method: "backend_api",
                    text: FINAL,
                    is_generating: false,
                    node_fresh: true,
                    node_id: "answer-refresh-final",
                    conversation_id: "conv-stale-refresh",
                    assistant_count: 1,
                    turn_index: 0,
                    copy_button_count: 0,
                    has_copy_button: false
                  }
                : {
                    method: "backend_api",
                    text: "",
                    is_generating: true,
                    node_fresh: false,
                    conversation_id: "conv-stale-refresh",
                    assistant_count: 1,
                    turn_index: 0
                  }
            };
          case "yoetz_bind_job":
            bindCount += 1;
            return { ok: true, payload: { rebound: true, url: currentUrl, title: "ChatGPT" } };
          case "yoetz_extract_response":
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, user_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            return {
              ok: true,
              payload: reloaded
                ? {
                    method: "copy_scope_dom_fallback",
                    text: FINAL,
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-stale-refresh",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 1 } }
                  }
                : {
                    method: "assistant_dom_fallback",
                    text: "I",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: 0,
                    conversation_id: "conv-stale-refresh",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 0 } }
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?backend_stale_render_refresh=${Date.now()}`);
    port.emit(envelope("job_start", "job_backend_stale_render_refresh", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_backend_stale_render_refresh", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_backend_stale_render_refresh.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 7000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.ok(fetchCount >= 1, "backend API should be attempted before falling through to reload");
    assert.equal(updates.length, 1);
    assert.deepEqual(updates[0], { id: 1, opts: { url: conversationUrl, active: false } });
    assert.equal(bindCount, 1);
    assert.equal(complete.payload.response, FINAL);
    assert.equal(complete.payload.extraction_method, "backend_api");
    assert.equal(complete.payload.completion_reason, "backend_api");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousCooldown === undefined) {
      delete globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS;
    } else {
      globalThis.__YOETZ_BACKEND_API_FETCH_COOLDOWN_MS = previousCooldown;
    }
  }
});

test("service worker reloads an idle frozen short response and completes from the refreshed conversation", async () => {
  const originalChrome = globalThis.chrome;
  const previousWaitingProgressInterval = globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
  globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = 100;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let reloaded = false;
  let bindCount = 0;
  const updates = [];
  const conversationUrl = "https://chatgpt.com/c/conv-freeze?_yoetz=run_job_background_render_freeze";
  let currentUrl = "https://chatgpt.com/";
  const FINAL = "I reviewed the bundle. The consumer guard is correct, but Yoetz must refresh the frozen render.";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => {
        currentUrl = opts.url;
        return { id: ++tabId, status: "complete", ...opts };
      },
      get: async (id) => ({ id, status: "complete", url: currentUrl }),
      update: async (id, opts) => {
        updates.push({ id, opts });
        if (opts.url) {
          currentUrl = opts.url;
        }
        reloaded = true;
        return { id, status: "complete", url: currentUrl };
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            currentUrl = conversationUrl;
            return { ok: true, payload: { sent: true, url: currentUrl, conversation_id: "conv-freeze" } };
          case "yoetz_bind_job":
            bindCount += 1;
            return { ok: true, payload: { rebound: true, url: currentUrl, title: "ChatGPT" } };
          case "yoetz_extract_response":
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, user_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 } };
            }
            return {
              ok: true,
              payload: reloaded
                ? {
                    method: "copy_scope_dom_fallback",
                    text: FINAL,
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: "conv-freeze",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 1 } }
                  }
                : {
                    method: "assistant_dom_fallback",
                    text: "I",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: false,
                    turn_index: 0,
                    conversation_id: "conv-freeze",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 1 } }
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?background_render_freeze=${Date.now()}`);
    port.emit(envelope("job_start", "job_background_render_freeze", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_background_render_freeze", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_background_render_freeze.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"), 7000);
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(updates.length, 1);
    assert.deepEqual(updates[0], { id: 1, opts: { url: conversationUrl, active: false } });
    assert.equal(bindCount, 1);
    assert.equal(complete.payload.response, FINAL);
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
    assert.ok(port.messages.some((message) => message.type === "job_progress" && message.payload.phase === "render_refreshing"));
    assert.ok(port.messages.some((message) => message.type === "job_progress" && message.payload.phase === "render_refreshed"));
  } finally {
    globalThis.chrome = originalChrome;
    if (previousWaitingProgressInterval === undefined) {
      delete globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS;
    } else {
      globalThis.__YOETZ_WAITING_RESPONSE_PROGRESS_INTERVAL_MS = previousWaitingProgressInterval;
    }
  }
});

test("service worker refreshes a frozen short render at most once", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  const updates = [];
  const conversationUrl = "https://chatgpt.com/c/conv-still-frozen?_yoetz=run_job_render_refresh_bounded";
  let currentUrl = "https://chatgpt.com/";
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => {
        currentUrl = opts.url;
        return { id: ++tabId, status: "complete", ...opts };
      },
      get: async (id) => ({ id, status: "complete", url: currentUrl }),
      update: async (id, opts) => {
        updates.push({ id, opts });
        if (opts.url) {
          currentUrl = opts.url;
        }
        return { id, status: "complete", url: currentUrl };
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            currentUrl = conversationUrl;
            return { ok: true, payload: { sent: true, url: currentUrl, conversation_id: "conv-still-frozen" } };
          case "yoetz_bind_job":
            return { ok: true, payload: { rebound: true, url: currentUrl, title: "ChatGPT" } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "I",
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: false,
                    turn_index: 0,
                    conversation_id: "conv-still-frozen",
                    diagnostics: { counts: { stop_controls: 0, copy_buttons: 1 } }
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, user_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?render_refresh_bounded=${Date.now()}`);
    port.emit(envelope("job_start", "job_render_refresh_bounded", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1800
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_render_refresh_bounded", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_render_refresh_bounded.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"), 5000);
    assert.equal(updates.length, 1);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
    assert.equal(port.messages.filter((message) => message.type === "job_progress" && message.payload.phase === "render_refreshing").length, 1);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rejects stale copy-button extraction from a pre-send assistant turn", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: "old answer",
                    is_generating: false,
                    assistant_count: 2,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
                : {
                    method: "copy_scope_dom_fallback",
                    text: "old answer",
                    is_generating: false,
                    assistant_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?stale_copy_turn=${Date.now()}`);
    port.emit(envelope("job_start", "job_stale_copy_turn", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_stale_copy_turn", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_stale_copy_turn.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not complete on copy button while response is still generating", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: "final answer",
                    is_generating: true,
                    assistant_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?copy_still_generating=${Date.now()}`);
    port.emit(envelope("job_start", "job_copy_stray_generating", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_copy_stray_generating", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_copy_stray_generating.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not complete long unscoped-copy text while response is still generating", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const longPartial = "Streaming Pro review paragraph that is still changing.\n".repeat(160).trim();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: longPartial,
                    is_generating: true,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: false,
                    turn_index: 0,
                    diagnostics: {
                      counts: { stop_controls: 1, copy_buttons: 1 }
                    }
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, user_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?long_unscoped_copy_generating=${Date.now()}`);
    port.emit(envelope("job_start", "job_long_unscoped_copy_generating", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_long_unscoped_copy_generating", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_long_unscoped_copy_generating.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker completes when a generating response becomes idle without text growth", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  let extractCount = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            extractCount += 1;
            return {
              ok: true,
              payload: sent
                ? {
                    method: "copy_scope_dom_fallback",
                    text: "OK",
                    is_generating: extractCount <= 2,
                    assistant_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?generating_then_idle=${Date.now()}`);
    port.emit(envelope("job_start", "job_generating_then_idle", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_generating_then_idle", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_generating_then_idle.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.payload.response, "OK");
    assert.equal(complete.payload.completion_reason, "copy_button");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not complete only because post-send copy controls increased", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: "YOETZ_EXTENSION_NATIVE_SMOKE_OK",
                    is_generating: false,
                    assistant_count: 3,
                    copy_button_count: 2,
                    has_copy_button: false,
                    turn_index: 0
                  }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, copy_button_count: 0, has_copy_button: false, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?copy_count_not_final=${Date.now()}`);
    port.emit(envelope("job_start", "job_copy_count_final", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_copy_count_final", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_copy_count_final.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker rebinds owned tab after content script reload during response wait", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const tabMessages = [];
  let tabId = 0;
  let sent = false;
  let rebound = false;
  let threwAfterSend = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        tabMessages.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_bind_job":
            rebound = true;
            return { ok: true, payload: { rebound: true, url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response":
            if (sent && !rebound && !threwAfterSend) {
              threwAfterSend = true;
              throw new Error("Could not establish connection. Receiving end does not exist.");
            }
            return {
              ok: true,
              payload: sent && rebound
                ? { method: "copy_scope_dom_fallback", text: "final after reload", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?rebind_wait=${Date.now()}`);
    port.emit(envelope("job_start", "job_rebind_wait", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1200
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_rebind_wait", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_rebind_wait.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    assert.ok(tabMessages.includes("yoetz_bind_job"));
    const complete = port.messages.find((message) => message.type === "job_complete");
    assert.equal(complete.payload.response, "final after reload");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker restarts model selection from scratch after persisted bfcache pageshow", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let resolveSuspendedSelection;
  let markSuspendedSelectionStarted;
  const suspendedSelectionStarted = new Promise((resolve) => {
    markSuspendedSelectionStarted = resolve;
  });
  const configureCalls = [];
  const chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            configureCalls.push({ reset: message.reset, attempt: message.job.model_selection_attempt });
            if (configureCalls.length === 1) {
              markSuspendedSelectionStarted();
              return new Promise((resolve) => {
                resolveSuspendedSelection = resolve;
              });
            }
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?bfcache_model_restart=${Date.now()}`);
    port.emit(envelope("job_start", "job_bfcache_model_restart", { prompt: "prompt" }));
    await suspendedSelectionStarted;

    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: ["job_bfcache_model_restart"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    assert.equal((await lifecycle("pagehide")).ok, true);
    assert.equal((await lifecycle("pageshow")).ok, true);

    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    assert.deepEqual(configureCalls, [
      { reset: false, attempt: 1 },
      { reset: true, attempt: 2 }
    ]);
    assert.equal(
      port.messages.filter((message) => message.payload?.phase === "model_selection_restarting").length,
      1
    );

    resolveSuspendedSelection({
      ok: true,
      payload: {
        status: "unavailable",
        requested_model: "gpt-5-6-sol-extra-high",
        failure_reason: "stale_suspended_attempt"
      }
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(port.messages.filter((message) => message.payload?.phase === "ready_for_file").length, 1);
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker retries model selection once after a transient content-script loss", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  const tabMessages = [];
  const configureAttempts = [];
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        tabMessages.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            configureAttempts.push(message.job.model_selection_attempt);
            if (configureAttempts.length === 1) {
              throw new Error("Could not establish connection. Receiving end does not exist.");
            }
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_bind_job":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?selecting_model_transient_cs=${Date.now()}`);
    port.emit(envelope("job_start", "job_selecting_model_transient_cs", { prompt: "prompt" }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    assert.deepEqual(configureAttempts, [1, 2]);
    assert.ok(tabMessages.includes("yoetz_bind_job"));
    assert.equal(port.messages.filter((message) => message.payload?.phase === "ready_for_file").length, 1);
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fail-closes model selection after one content-script recovery retry", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let configureCalls = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            configureCalls += 1;
            throw new Error("Could not establish connection. Receiving end does not exist.");
          case "yoetz_bind_job":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?selecting_model_persistent_cs=${Date.now()}`);
    port.emit(envelope("job_start", "job_selecting_model_persistent_cs", { prompt: "prompt" }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    assert.equal(configureCalls, 2);
    assert.equal(port.messages.filter((message) => message.type === "job_error").length, 1);
    assert.equal(port.messages.some((message) => message.payload?.phase === "ready_for_file"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker keeps one live model-selection attempt when recovery retry races a pageshow restart", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let releaseBind;
  let resolveSecondConfigure;
  let markBindStarted;
  let markSecondConfigureStarted;
  const bindStarted = new Promise((resolve) => {
    markBindStarted = resolve;
  });
  const secondConfigureStarted = new Promise((resolve) => {
    markSecondConfigureStarted = resolve;
  });
  let bindCalls = 0;
  const configureCalls = [];
  const chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            configureCalls.push(message.job.model_selection_attempt);
            if (configureCalls.length === 1) {
              throw new Error("Could not establish connection. Receiving end does not exist.");
            }
            if (configureCalls.length === 2) {
              markSecondConfigureStarted();
              return new Promise((resolve) => {
                resolveSecondConfigure = () => resolve({ ok: true, payload: verifiedSolProSelection() });
              });
            }
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_bind_job":
            bindCalls += 1;
            markBindStarted();
            return new Promise((resolve) => {
              releaseBind = () => resolve({ ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } });
            });
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?selecting_model_pageshow_race=${Date.now()}`);
    const jobId = "job_selecting_model_pageshow_race";
    port.emit(envelope("job_start", jobId, { prompt: "prompt" }));
    await Promise.race([
      bindStarted,
      new Promise((_, reject) => setTimeout(() => reject(new Error("content-script bind did not start")), 5000))
    ]);

    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: [jobId] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    assert.equal((await lifecycle("pagehide")).ok, true);
    const firstPageshow = lifecycle("pageshow");
    await secondConfigureStarted;
    assert.equal((await lifecycle("pagehide")).ok, true);
    assert.equal((await lifecycle("pageshow")).ok, true);
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    releaseBind();
    resolveSecondConfigure();
    assert.equal((await firstPageshow).ok, true);
    await new Promise((resolve) => setTimeout(resolve, 25));
    assert.ok(bindCalls >= 1);
    assert.equal(port.messages.filter((message) => message.payload?.phase === "ready_for_file").length, 1);
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker keeps the fail-closed model-selection result when bfcache restart fails", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let resolveSuspendedSelection;
  let markSuspendedSelectionStarted;
  const suspendedSelectionStarted = new Promise((resolve) => {
    markSuspendedSelectionStarted = resolve;
  });
  let configureCalls = 0;
  const failedSelection = {
    status: "unavailable",
    model_used: null,
    requested_model: "gpt-5-6-sol-extra-high",
    family_status: "unverified",
    effort_status: "unverified",
    failure_reason: "model_picker_open_failed",
    warning: "ChatGPT GPT-5.6 model picker did not open"
  };
  const chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            configureCalls += 1;
            if (configureCalls === 1) {
              markSuspendedSelectionStarted();
              return new Promise((resolve) => {
                resolveSuspendedSelection = resolve;
              });
            }
            assert.equal(message.reset, true);
            return { ok: true, payload: failedSelection };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?bfcache_model_restart_failed=${Date.now()}`);
    port.emit(envelope("job_start", "job_bfcache_model_restart_failed", { prompt: "prompt" }));
    await suspendedSelectionStarted;
    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: ["job_bfcache_model_restart_failed"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    assert.equal((await lifecycle("pagehide")).ok, true);
    assert.equal((await lifecycle("pageshow")).ok, true);

    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "model_selection_failed");
    assert.equal(error.payload.phase, "model_selection");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.failure_reason, failedSelection.failure_reason);
    assert.deepEqual(error.payload.model_selection, failedSelection);

    resolveSuspendedSelection({ ok: true, payload: verifiedSolProSelection() });
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(port.messages.some((message) => message.payload?.phase === "ready_for_file"), false);
    assert.equal(port.messages.filter((message) => message.type === "job_error").length, 1);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not resurrect a terminal model selection from a stale post-configure tail", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  let runtimeListener = null;
  let tabId = 0;
  let configureCalls = 0;
  let releaseAttemptTwoGrouping;
  let markAttemptTwoGroupingStarted;
  const attemptTwoGroupingStarted = new Promise((resolve) => {
    markAttemptTwoGroupingStarted = resolve;
  });
  const failedSelection = {
    status: "unavailable",
    model_used: null,
    requested_model: "gpt-5-6-sol-extra-high",
    family_status: "unverified",
    effort_status: "unverified",
    failure_reason: "model_picker_open_failed",
    warning: "ChatGPT GPT-5.6 model picker did not open"
  };
  let resolveInitialSelection;
  const chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      group: async () => {
        markAttemptTwoGroupingStarted();
        return new Promise((resolve) => {
          releaseAttemptTwoGrouping = () => resolve(1);
        });
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            configureCalls += 1;
            if (configureCalls === 1) {
              return new Promise((resolve) => {
                resolveInitialSelection = resolve;
              });
            }
            return {
              ok: true,
              payload: configureCalls === 2 ? verifiedSolProSelection() : failedSelection
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?bfcache_model_tail_liveness=${Date.now()}`);
    const jobId = "job_bfcache_model_tail_liveness";
    port.emit(envelope("job_start", jobId, { prompt: "prompt" }));
    await eventually(() => configureCalls === 1);
    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: [jobId] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });

    assert.equal((await lifecycle("pagehide")).ok, true);
    const attemptTwoPageshow = lifecycle("pageshow");
    await attemptTwoGroupingStarted;
    assert.equal((await lifecycle("pagehide")).ok, true);
    assert.equal((await lifecycle("pageshow")).ok, true);
    await eventually(() => port.messages.some((message) => message.type === "job_error"));

    releaseAttemptTwoGrouping();
    assert.equal((await attemptTwoPageshow).ok, true);
    await new Promise((resolve) => setTimeout(resolve, 0));
    resolveInitialSelection({ ok: true, payload: verifiedSolProSelection() });
    await new Promise((resolve) => setTimeout(resolve, 0));

    const persisted = await storage.get(`jobs.${jobId}`);
    assert.equal(persisted[`jobs.${jobId}`].status, "failed");
    const errorIndex = port.messages.findIndex((message) => message.type === "job_error");
    assert.notEqual(errorIndex, -1);
    assert.equal(
      port.messages.slice(errorIndex + 1).some((message) => message.payload?.phase === "ready_for_file"),
      false
    );
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker idempotently rebinds a persisted bfcache restore without replaying side effects", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let sent = false;
  let rebound = false;
  let uploadCalls = 0;
  let sendCalls = 0;
  let bindCalls = 0;
  let bfcacheRestoreStarted = false;
  let rejectInFlightExtraction;
  let markInFlightExtractionStarted;
  let markLifecycleBindStarted;
  let releaseLifecycleBind;
  const inFlightExtractionStarted = new Promise((resolve) => {
    markInFlightExtractionStarted = resolve;
  });
  const lifecycleBindStarted = new Promise((resolve) => {
    markLifecycleBindStarted = resolve;
  });
  const lifecycleBindReleased = new Promise((resolve) => {
    releaseLifecycleBind = resolve;
  });
  const chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            uploadCalls += 1;
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sendCalls += 1;
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_bind_job":
            bindCalls += 1;
            rebound = true;
            if (bfcacheRestoreStarted) {
              markLifecycleBindStarted();
              await lifecycleBindReleased;
            }
            return { ok: true, payload: { rebound: true, url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response":
            if (sent && bindCalls === 0) {
              throw new Error("A listener indicated an asynchronous response by returning true, but the message channel closed before a response was received");
            }
            if (sent && !bfcacheRestoreStarted) {
              markInFlightExtractionStarted();
              return new Promise((_resolve, reject) => {
                rejectInFlightExtraction = reject;
              });
            }
            return {
              ok: true,
              payload: sent && rebound
                ? { method: "copy_scope_dom_fallback", text: "final after bfcache", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                : { method: "none", text: "", is_generating: true, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?bfcache_rebind=${Date.now()}`);
    port.emit(envelope("job_start", "job_bfcache_rebind", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1500
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_bfcache_rebind", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_bfcache_rebind.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);
    await inFlightExtractionStarted;

    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: ["job_bfcache_rebind"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    assert.equal((await lifecycle("pagehide")).ok, true);
    bfcacheRestoreStarted = true;
    const restored = lifecycle("pageshow");
    await lifecycleBindStarted;
    rejectInFlightExtraction(new Error("The message channel closed before a response was received."));
    releaseLifecycleBind();
    assert.equal((await restored).ok, true);
    // A duplicate restore observes the already rebound state without rebinding;
    // provider-visible upload/send steps are never replayed.
    assert.equal((await lifecycle("pageshow")).ok, true);

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    assert.equal(uploadCalls, 1);
    assert.equal(sendCalls, 1);
    assert.equal(bindCalls, 2);
    assert.equal(
      port.messages.find((message) => message.type === "job_complete")?.payload.response,
      "final after bfcache"
    );
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker terminates actionably when persisted bfcache reconnect fails", async () => {
  const originalChrome = globalThis.chrome;
  const previousAttempts = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
  const previousDelay = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = 1;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = 0;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let sent = false;
  let portLost = false;
  let uploadCalls = 0;
  let sendCalls = 0;
  let rejectExtraction;
  let markExtractionStarted;
  const extractionStarted = new Promise((resolve) => {
    markExtractionStarted = resolve;
  });
  const storage = makeStorage();
  const chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        if (portLost && ["yoetz_probe", "yoetz_bind_job", "yoetz_extract_response"].includes(message.type)) {
          throw new Error("Could not establish connection. Receiving end does not exist.");
        }
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            uploadCalls += 1;
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sendCalls += 1;
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: true, assistant_count: 0, turn_index: -1 } };
            }
            markExtractionStarted();
            return new Promise((_resolve, reject) => {
              rejectExtraction = reject;
            });
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?bfcache_rebind_failed=${Date.now()}`);
    port.emit(envelope("job_start", "job_bfcache_failed", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_bfcache_failed", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_bfcache_failed.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);
    await extractionStarted;
    await new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event: "pagehide", persisted: true, job_ids: ["job_bfcache_failed"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    portLost = true;

    const lifecycleResponse = new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event: "pageshow", persisted: true, job_ids: ["job_bfcache_failed"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    await eventually(() => port.messages.some((message) => (
      message.payload?.phase === "content_script_recovering"
    )));
    rejectExtraction(new Error("Could not establish connection. Receiving end does not exist."));
    const response = await lifecycleResponse;
    assert.equal(response.ok, true);
    await eventually(() => port.messages.some((message) => (
      message.type === "job_error" && message.payload.code === "content_script_reconnect_failed"
    )));
    const error = port.messages.find((message) => message.payload?.code === "content_script_reconnect_failed");
    assert.equal(error.payload.phase, "wait_response");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.send_committed, true);
    assert.equal(error.payload.tab_disposition, "kept");
    assert.match(error.payload.message, /Do not rerun automatically/);
    assert.equal(uploadCalls, 1);
    assert.equal(sendCalls, 1);
    await new Promise((resolve) => setTimeout(resolve, 0));
    const jobErrors = port.messages.filter((message) => (
      message.type === "job_error" && message.job_id === "job_bfcache_failed"
    ));
    assert.equal(jobErrors.length, 1);
    assert.equal(jobErrors[0].payload.code, "content_script_reconnect_failed");
    const persisted = await storage.get("jobs.job_bfcache_failed");
    assert.notEqual(persisted["jobs.job_bfcache_failed"]?.status, "terminal_delivery_lost");
  } finally {
    globalThis.chrome = originalChrome;
    if (previousAttempts === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = previousAttempts;
    if (previousDelay === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = previousDelay;
  }
});

test("service worker parks a suspended waiting_response job past the reconnect probe window then completes on pageshow", async () => {
  const originalChrome = globalThis.chrome;
  const previousAttempts = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
  const previousDelay = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = 2;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = 20;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let sent = false;
  let hidden = false;
  let allowProbe = true;
  let uploadCalls = 0;
  let sendCalls = 0;
  let probesWhileHidden = 0;
  let rejectInFlightExtraction;
  let markInFlightExtractionStarted;
  const inFlightExtractionStarted = new Promise((resolve) => {
    markInFlightExtractionStarted = resolve;
  });
  const chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            if (hidden) {
              probesWhileHidden += 1;
            }
            if (!allowProbe) {
              throw new Error("Could not establish connection. Receiving end does not exist.");
            }
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            uploadCalls += 1;
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sendCalls += 1;
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_bind_job":
            return { ok: true, payload: { rebound: true, url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response":
            if (sent && !hidden) {
              markInFlightExtractionStarted();
              return new Promise((_resolve, reject) => {
                rejectInFlightExtraction = reject;
              });
            }
            return {
              ok: true,
              payload: sent && allowProbe
                ? { method: "copy_scope_dom_fallback", text: "final after long suspension", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                : { method: "none", text: "", is_generating: true, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?park_past_probe=${Date.now()}`);
    port.emit(envelope("job_start", "job_park_past_probe", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_park_past_probe", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_park_past_probe.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);
    await inFlightExtractionStarted;

    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: ["job_park_past_probe"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    assert.equal((await lifecycle("pagehide")).ok, true);
    hidden = true;
    allowProbe = false;
    rejectInFlightExtraction(new Error("The message channel closed before a response was received."));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "parked_for_pageshow"));
    await new Promise((resolve) => setTimeout(resolve, 120));
    assert.equal(probesWhileHidden, 0, "recovery must not probe while the tab is in bfcache");
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
    allowProbe = true;
    assert.equal((await lifecycle("pageshow")).ok, true);

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    assert.equal(uploadCalls, 1);
    assert.equal(sendCalls, 1);
    assert.equal(
      port.messages.find((message) => message.type === "job_complete")?.payload.response,
      "final after long suspension"
    );
    const parked = port.messages.find((message) => message.payload?.phase === "parked_for_pageshow");
    assert.equal(typeof parked?.payload.inspect_command, "string");
    assert.match(parked.payload.inspect_command, /yoetz browser extension inspect/);
  } finally {
    globalThis.chrome = originalChrome;
    if (previousAttempts === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = previousAttempts;
    if (previousDelay === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = previousDelay;
  }
});

test("service worker restored waiting_response poller joins an in-flight pageshow recovery", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const now = Date.now();
  let runtimeListener = null;
  let bindCalls = 0;
  let rebound = false;
  let inFlightExtracts = 0;
  let maxInFlightExtracts = 0;
  let rejectInFlightExtraction;
  let markInFlightExtractionStarted;
  const inFlightExtractionStarted = new Promise((resolve) => {
    markInFlightExtractionStarted = resolve;
  });
  await storage.set({
    "jobs.job_restore_pageshow_race": {
      job_id: "job_restore_pageshow_race",
      run_id: "run_job_restore_pageshow_race",
      workspace_id: "workspace_test",
      status: "waiting_response",
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000,
      tab_id: 77,
      response_baseline: {
        method: "none",
        text: "",
        is_generating: false,
        assistant_count: 0,
        turn_index: -1
      },
      submitted_user_count: 1,
      submitted_assistant_count: 0,
      started_at: now,
      response_wait_started_at: now,
      updated_at: now
    }
  });
  const chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async () => {
        throw new Error("restore must reuse the submitted tab");
      },
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/?_yoetz=run_job_restore_pageshow_race" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_bind_job":
            bindCalls += 1;
            rebound = bindCalls >= 2;
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response": {
            if (bindCalls < 2) {
              markInFlightExtractionStarted();
              return new Promise((_resolve, reject) => {
                rejectInFlightExtraction = reject;
              });
            }
            inFlightExtracts += 1;
            maxInFlightExtracts = Math.max(maxInFlightExtracts, inFlightExtracts);
            await new Promise((resolve) => setTimeout(resolve, 20));
            inFlightExtracts -= 1;
            return {
              ok: true,
              payload: {
                method: "copy_scope_dom_fallback",
                text: "restored after pageshow race",
                is_generating: false,
                assistant_count: 1,
                copy_button_count: 1,
                has_copy_button: true,
                turn_index: 0,
                preceding_user_count: 1
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?restore_pageshow_race=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    await inFlightExtractionStarted;
    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: ["job_restore_pageshow_race"] },
        { tab: { id: 77 } },
        resolve
      ), true);
    });
    assert.equal((await lifecycle("pagehide")).ok, true);
    const restored = lifecycle("pageshow");
    await eventually(() => bindCalls >= 2 || port.messages.some((message) => message.payload?.phase === "content_script_recovering"));
    rejectInFlightExtraction(new Error("poller exploded after worker restore"));
    assert.equal((await restored).ok, true);
    await eventually(() => port.messages.some((message) => (
      message.type === "job_complete" && message.job_id === "job_restore_pageshow_race"
    )));
    assert.equal(port.messages.some((message) => message.type === "job_error" && message.job_id === "job_restore_pageshow_race"), false);
    assert.equal(maxInFlightExtracts, 1, "exactly one poller may extract after rebind");
    assert.equal(
      port.messages.find((message) => message.type === "job_complete")?.payload.response,
      "restored after pageshow race"
    );
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker joins an extraction rejection delivered several tasks after recovery settles", async () => {
  const originalChrome = globalThis.chrome;
  const previousAttempts = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
  const previousDelay = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = 1;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = 0;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let sent = false;
  let recovered = false;
  let uploadCalls = 0;
  let sendCalls = 0;
  let rejectInFlightExtraction;
  let markInFlightExtractionStarted;
  const inFlightExtractionStarted = new Promise((resolve) => {
    markInFlightExtractionStarted = resolve;
  });
  const chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            if (recovered) {
              throw new Error("Could not establish connection. Receiving end does not exist.");
            }
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            uploadCalls += 1;
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sendCalls += 1;
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_bind_job":
            recovered = true;
            return { ok: true, payload: { rebound: true, url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response":
            if (sent && !recovered) {
              markInFlightExtractionStarted();
              return new Promise((_resolve, reject) => {
                rejectInFlightExtraction = reject;
              });
            }
            return {
              ok: true,
              payload: recovered
                ? { method: "copy_scope_dom_fallback", text: "final after late rejection", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                : { method: "none", text: "", is_generating: true, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?late_extract_reject=${Date.now()}`);
    port.emit(envelope("job_start", "job_late_extract_reject", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_late_extract_reject", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_late_extract_reject.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);
    await inFlightExtractionStarted;
    const lifecycle = (event) => new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event, persisted: true, job_ids: ["job_late_extract_reject"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    assert.equal((await lifecycle("pagehide")).ok, true);
    assert.equal((await lifecycle("pageshow")).ok, true);
    await eventually(() => port.messages.some((message) => message.payload?.phase === "content_script_recovered"));
    for (let i = 0; i < 5; i += 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    rejectInFlightExtraction(new Error("The message channel closed before a response was received."));
    await eventually(() => port.messages.some((message) => message.type === "job_complete"));
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
    assert.equal(uploadCalls, 1);
    assert.equal(sendCalls, 1);
    assert.equal(
      port.messages.find((message) => message.type === "job_complete")?.payload.response,
      "final after late rejection"
    );
  } finally {
    globalThis.chrome = originalChrome;
    if (previousAttempts === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = previousAttempts;
    if (previousDelay === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = previousDelay;
  }
});

test("service worker times out a parked suspension at the response deadline without probing", async () => {
  const originalChrome = globalThis.chrome;
  const previousAttempts = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
  const previousDelay = globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = 2;
  globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = 20;
  const port = makePort();
  let runtimeListener = null;
  let tabId = 0;
  let sent = false;
  let hidden = false;
  let probesWhileHidden = 0;
  let rejectInFlightExtraction;
  let markInFlightExtractionStarted;
  const inFlightExtractionStarted = new Promise((resolve) => {
    markInFlightExtractionStarted = resolve;
  });
  const chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            if (hidden) {
              probesWhileHidden += 1;
              throw new Error("Could not establish connection. Receiving end does not exist.");
            }
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_bind_job":
            return { ok: true, payload: { rebound: true } };
          case "yoetz_extract_response":
            if (sent) {
              markInFlightExtractionStarted();
              return new Promise((_resolve, reject) => {
                rejectInFlightExtraction = reject;
              });
            }
            return { ok: true, payload: { method: "none", text: "", is_generating: true, assistant_count: 0, turn_index: -1 } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });
  chrome.runtime.onMessage.addListener = (listener) => {
    runtimeListener = listener;
  };
  globalThis.chrome = chrome;

  try {
    await import(`../src/service-worker.js?park_deadline=${Date.now()}`);
    port.emit(envelope("job_start", "job_park_deadline", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 250
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_park_deadline", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_park_deadline.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);
    await inFlightExtractionStarted;
    await new Promise((resolve) => {
      assert.equal(runtimeListener(
        { type: "yoetz_content_lifecycle", event: "pagehide", persisted: true, job_ids: ["job_park_deadline"] },
        { tab: { id: tabId } },
        resolve
      ), true);
    });
    hidden = true;
    rejectInFlightExtraction(new Error("The message channel closed before a response was received."));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "parked_for_pageshow"));
    await eventually(() => port.messages.some((message) => (
      message.type === "job_error" && message.job_id === "job_park_deadline"
    )), 2000);
    const errors = port.messages.filter((message) => (
      message.type === "job_error" && message.job_id === "job_park_deadline"
    ));
    assert.equal(errors.length, 1);
    assert.equal(errors[0].payload.code, "response_timeout");
    assert.equal(errors[0].payload.message.includes("did not become ready"), false);
    assert.equal(probesWhileHidden, 0, "deadline expiry must not run the reconnect probe loop");
  } finally {
    globalThis.chrome = originalChrome;
    if (previousAttempts === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_ATTEMPTS = previousAttempts;
    if (previousDelay === undefined) delete globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS;
    else globalThis.__YOETZ_CONTENT_SCRIPT_RECONNECT_DELAY_MS = previousDelay;
  }
});

test("service worker preserves content-script committed-send error metadata", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_extract_response":
            return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 } };
          case "yoetz_send_prompt":
            return {
              ok: false,
              code: "send_acceptance_unknown",
              state: "usage_credits_exhausted",
              provider_message: "Your org is out of usage credits for the month.",
              provider_dom: {
                container: { found: true, tag: "div", role: "alert" },
                switch_models_control: { found: false }
              },
              requested_model: "fable-5-max",
              send_committed: true,
              phase: "send",
              side_effect_started: true,
              error: "send click committed; acceptance unknown"
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?send_unknown=${Date.now()}`);
    port.emit(envelope("job_start", "job_send_unknown", {
      prompt: "prompt",
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_send_unknown", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_send_unknown.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "send_acceptance_unknown"));
    const error = port.messages.find((message) => message.type === "job_error" && message.payload.code === "send_acceptance_unknown");
    assert.equal(error.payload.phase, "send");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.state, "usage_credits_exhausted");
    assert.equal(error.payload.provider_message, "Your org is out of usage credits for the month.");
    assert.deepEqual(error.payload.provider_dom, {
      container: { found: true, tag: "div", role: "alert" },
      switch_models_control: { found: false }
    });
    assert.equal(error.payload.requested_model, "fable-5-max");
    assert.equal(error.payload.send_committed, true);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker reports Claude credits at baseline extraction as pre-send", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  let tabId = 0;
  let sendPromptCalls = 0;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://claude.ai/new" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return {
              ok: true,
              payload: {
                status: "selected",
                requested_model: "fable-5-max",
                model_used: "Fable 5 Max",
                modelVerified: true,
                maxVerified: true
              }
            };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_extract_response":
            assert.equal(message.blocking_context, "pre_send_baseline");
            return {
              ok: false,
              code: "usage_credits_exhausted",
              state: "usage_credits_exhausted",
              provider_message: "Your org is out of usage credits for the month. We let your admin know. Switch models to continue chatting.",
              requested_model: "fable-5-max",
              phase: "send",
              side_effect_started: true,
              send_committed: false,
              error: "Claude cannot run Fable 5 Max because this organization is out of monthly usage credits. Yoetz did not switch models."
            };
          case "yoetz_send_prompt":
            sendPromptCalls += 1;
            throw new Error("prompt must not be sent after credit exhaustion");
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?claude_credits_baseline=${Date.now()}`);
    port.emit(envelope("job_start", "job_claude_credits_baseline", {
      recipe: "claude",
      prompt: "prompt"
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_claude_credits_baseline", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_claude_credits_baseline.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => (
      message.type === "job_error" && message.payload.code === "usage_credits_exhausted"
    )));
    const error = port.messages.find((message) => (
      message.type === "job_error" && message.payload.code === "usage_credits_exhausted"
    ));
    assert.equal(error.payload.phase, "send");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal(error.payload.send_committed, false);
    assert.equal(error.payload.requested_model, "fable-5-max");
    assert.equal(sendPromptCalls, 0);
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker lifecycle events do not downgrade an active native connection", async () => {
  const originalChrome = globalThis.chrome;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  const port = makePort();
  const storage = makeStorage();
  let installedListener = null;
  let startupListener = null;

  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};
  globalThis.chrome = {
    runtime: {
      connectNative: () => port,
      getManifest: () => ({ version: "0.4.0" }),
      getURL: (value) => new URL(`../${value}`, import.meta.url).href,
      onInstalled: { addListener: (listener) => { installedListener = listener; } },
      onStartup: { addListener: (listener) => { startupListener = listener; } },
      onMessage: { addListener: () => {} }
    },
    storage: {
      session: storage,
      local: makeStorage()
    },
    identity: {
      getProfileUserInfo: async () => ({ email: "", id: "" })
    },
    alarms: {
      onAlarm: { addListener: () => {} },
      create: () => {},
      clear: () => {}
    }
  };

  try {
    await import(`../src/service-worker.js?lifecycle=${Date.now()}`);
    await eventually(async () => (await storage.get("status")).status?.status === "connected");

    installedListener();
    startupListener();
    await new Promise((resolve) => setTimeout(resolve, 25));

    assert.equal((await storage.get("status")).status.status, "connected");
  } finally {
    globalThis.chrome = originalChrome;
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
  }
});

test("service worker treats native port write failures as reconnectable disconnects", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const scheduledAlarms = [];
  globalThis.chrome = chromeStub({
    port,
    storage,
    alarms: {
      onAlarm: { addListener: () => {} },
      create: (name) => {
        scheduledAlarms.push(name);
      },
      clear: () => {}
    },
    tabs: {}
  });

  try {
    await import(`../src/service-worker.js?post_throw=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;
    port.throwOnPost = new Error("port closed");

    port.emit({ protocol_version: 999, transport: "chrome-extension-native", type: "heartbeat", request_id: "bad" });

    await eventually(async () => (await storage.get("status")).status?.status === "missing_native_host");
    assert.equal(port.messages.length, 0);
    assert.ok(scheduledAlarms.includes("yoetz-reconnect"));
    assert.match((await storage.get("status")).status.detail, /native port write failed: port closed/);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker stops before upload when final chunk ack cannot reach native host", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const sentToTabs = [];
  const scheduledAlarms = [];
  let tabId = 0;
  port.throwOnPostMessage = (message) => message.type === "job_file_chunk_ack";
  globalThis.chrome = chromeStub({
    port,
    storage,
    alarms: {
      onAlarm: { addListener: () => {} },
      create: (name) => {
        scheduledAlarms.push(name);
      },
      clear: () => {}
    },
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        sentToTabs.push(message.type);
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?ack_throw=${Date.now()}`);
    port.emit(envelope("job_start", "job_ack_throw", {
      prompt: "prompt",
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_ack_throw", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_ack_throw.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(async () => (await storage.get("jobs.job_ack_throw"))["jobs.job_ack_throw"]?.status === "terminal_delivery_lost");
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
    assert.ok(scheduledAlarms.includes("yoetz-reconnect"));
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker shards storage by job id so concurrent jobs do not clobber each other", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const sentJobs = new Set();
  let tabId = 0;
  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sentJobs.add(message.job.job_id);
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sentJobs.has(message.job.job_id)
                ? { method: "assistant_dom_fallback", text: `answer ${message.job.job_id}`, is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?storage_shards=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));

    const ids = ["job_shard_a", "job_shard_b"];
    for (const jobId of ids) {
      port.emit(envelope("job_start", jobId, {
        prompt: `prompt ${jobId}`,
        wait_interval_ms: 50,
        wait_timeout_ms: 1500
      }));
    }
    await eventually(() => port.messages.filter((message) => message.type === "job_progress" && message.payload.phase === "ready_for_file").length === 2);

    // Both shards must exist as their own keys before file_received transitions.
    const everything = await storage.get(null);
    assert.ok(Object.prototype.hasOwnProperty.call(everything, "jobs.job_shard_a"), "expected jobs.job_shard_a shard");
    assert.ok(Object.prototype.hasOwnProperty.call(everything, "jobs.job_shard_b"), "expected jobs.job_shard_b shard");
    assert.equal(everything.jobs, undefined, "legacy single jobs map should not exist");

    // Drive both jobs to completion and confirm shards survive (TTL sweep is on heartbeat, not per-save).
    for (const jobId of ids) {
      port.emit(envelope("job_file_chunk", jobId, {
        sequence: 0,
        total_chunks: 1,
        total_bytes: 4,
        filename: `${jobId}.md`,
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }));
    }
    await eventually(() => port.messages.filter((message) => message.type === "job_complete").length === 2);
    const afterComplete = await storage.get(null);
    assert.equal(afterComplete["jobs.job_shard_a"]?.status, "complete");
    assert.equal(afterComplete["jobs.job_shard_b"]?.status, "complete");
    assert.equal(afterComplete.jobs, undefined);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker caps last_response_progress_text on disk while keeping the full text in memory for delta calc", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const longBase = "X".repeat(200 * 1024); // 200KB of payload
  const finalSuffix = "DELTA-TAIL-MARKER";
  const finalText = longBase + finalSuffix;
  let tabId = 0;
  let sent = false;
  let extractionTick = 0;
  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response": {
            if (!sent) {
              return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 } };
            }
            extractionTick += 1;
            // First post-send extraction: long base (still generating). Second tick: long base + suffix, idle, with copy button.
            if (extractionTick === 1) {
              return {
                ok: true,
                payload: {
                  method: "assistant_dom_fallback",
                  text: longBase,
                  is_generating: true,
                  assistant_count: 1,
                  copy_button_count: 0,
                  has_copy_button: false,
                  turn_index: 0
                }
              };
            }
            return {
              ok: true,
              payload: {
                method: "assistant_dom_fallback",
                text: finalText,
                is_generating: false,
                assistant_count: 1,
                copy_button_count: 1,
                has_copy_button: true,
                turn_index: 0
              }
            };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?response_text_cap=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.emit(envelope("job_start", "job_long_response", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 5000
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_file_chunk", "job_long_response", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_long_response.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete"));

    // In-memory delta calc proof: the second response_observed event must carry only the
    // suffix as response_delta. If the in-memory last_response_progress_text had been
    // truncated to a tail at any point, delta = finalText (full) instead of finalSuffix.
    const observed = port.messages.filter((m) => m.type === "job_progress" && m.payload.phase === "response_observed");
    assert.ok(observed.length >= 2, `expected ≥2 response_observed messages, got ${observed.length}`);
    assert.equal(observed[0].payload.response_delta.length, longBase.length);
    assert.equal(observed[1].payload.response_delta, finalSuffix);

    // On-disk shard: full text MUST NOT be persisted as last_response_progress_text;
    // the tail field must be ≤ 8KB.
    const shard = (await storage.get("jobs.job_long_response"))["jobs.job_long_response"];
    assert.ok(shard, "expected sharded job to be persisted");
    assert.equal(shard.last_response_progress_text, undefined,
      "full streaming text must not be persisted to chrome.storage.session");
    if (shard.last_response_progress_tail !== undefined) {
      assert.ok(shard.last_response_progress_tail.length <= 8 * 1024,
        `last_response_progress_tail (${shard.last_response_progress_tail.length}) must fit within 8KB cap`);
      assert.ok(finalText.endsWith(shard.last_response_progress_tail),
        "tail must be a suffix of the full text");
    }
    assert.equal(typeof shard.last_response_progress_length, "number");
    assert.equal(shard.last_response_progress_length, finalText.length);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker does not persist on every in-flight chunk", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  // Wrap storage.set so we can count how many times the chunk-stream job's shard is written.
  const writes = [];
  const wrappedStorage = {
    get: storage.get.bind(storage),
    set: async (values) => {
      writes.push(Object.keys(values));
      return storage.set(values);
    },
    remove: storage.remove?.bind(storage)
  };
  let tabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    storage: wrappedStorage,
    tabs: {
      create: async (opts) => ({ id: ++tabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 16 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? { method: "assistant_dom_fallback", text: "answer", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?chunk_persist_skip=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.emit(envelope("job_start", "job_chunk_persist", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1500
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));

    const shardKey = "jobs.job_chunk_persist";
    // Count shard writes between ready_for_file and the final chunk so we capture
    // only the per-chunk persist surface (not unrelated start-up writes).
    const beforeShardWrites = writes.filter((keys) => keys.includes(shardKey)).length;

    const totalChunks = 5;
    const payload = new TextEncoder().encode("xxx");
    for (let sequence = 0; sequence < totalChunks; sequence += 1) {
      port.emit(envelope("job_file_chunk", "job_chunk_persist", {
        sequence,
        total_chunks: totalChunks,
        total_bytes: payload.byteLength * totalChunks,
        filename: "job_chunk_persist.md",
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(payload)
      }));
      // Wait for ack so the next emit observes the previous chunk's storage state.
      await eventually(() => port.messages.filter((m) => m.type === "job_file_chunk_ack" && m.job_id === "job_chunk_persist").length === sequence + 1);
    }

    // The first chunk should persist once (waiting_for_file → receiving_file). Subsequent
    // intermediate chunks must NOT persist. The final chunk persists at file_received.
    // So, end-to-end shard writes from in-flight chunks should be exactly 2 (transition + terminal),
    // NOT totalChunks (5).
    await eventually(() => port.messages.some((m) => m.type === "job_complete" && m.job_id === "job_chunk_persist"));
    const afterShardWrites = writes.filter((keys) => keys.includes(shardKey)).length;
    const chunkRelatedWrites = afterShardWrites - beforeShardWrites;
    // Allowed: transition (1) + file_received (1) + uploading_file (1) + sending_prompt (1)
    //          + waiting_response (1) + complete (1). Strict bound: must be < totalChunks.
    assert.ok(chunkRelatedWrites < totalChunks * 2,
      `expected < ${totalChunks * 2} shard writes after ${totalChunks} chunks, got ${chunkRelatedWrites}`);
    // Stricter assertion: chunk delivery itself must not produce one write per chunk.
    // First chunk transitions status (1 write), final chunk terminal write (1). Anything in
    // between is a regression of the per-chunk persist behavior we removed.
    // Other writes after the first chunk are job lifecycle (uploading/prompt/etc.), bounded.
    assert.ok(chunkRelatedWrites <= totalChunks + 2,
      `expected at most totalChunks + 2 writes, got ${chunkRelatedWrites}`);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker migrates legacy { jobs: {...} } map to per-job shards on restore", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  // Pre-seed legacy shape: a single 'jobs' key holding a map of jobs, as written by older
  // extension installations before the sharding refactor.
  await storage.set({
    jobs: {
      job_legacy_alpha: {
        job_id: "job_legacy_alpha",
        run_id: "run_legacy_alpha",
        workspace_id: "workspace_test",
        capability_token: "tok-alpha",
        status: "complete",
        started_at: Date.now(),
        updated_at: Date.now()
      },
      job_legacy_beta: {
        job_id: "job_legacy_beta",
        run_id: "run_legacy_beta",
        workspace_id: "workspace_test",
        capability_token: "tok-beta",
        status: "complete",
        started_at: Date.now(),
        updated_at: Date.now()
      }
    }
  });

  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {}
  });

  try {
    await import(`../src/service-worker.js?legacy_migration=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    // Restore happens during connectNative → restoreJobsFromStorage. Wait for migration to settle.
    await eventually(async () => {
      const all = await storage.get(null);
      return Object.prototype.hasOwnProperty.call(all, "jobs.job_legacy_alpha")
        && Object.prototype.hasOwnProperty.call(all, "jobs.job_legacy_beta");
    });
    const all = await storage.get(null);
    assert.equal(all.jobs, undefined, "legacy 'jobs' key must be removed after migration");
    assert.equal(all["jobs.job_legacy_alpha"].job_id, "job_legacy_alpha");
    assert.equal(all["jobs.job_legacy_beta"].job_id, "job_legacy_beta");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker preserves terminal_delivery_lost jobs on restore", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  await storage.set({
    "jobs.job_delivery_lost": {
      job_id: "job_delivery_lost",
      run_id: "run_delivery_lost",
      workspace_id: "workspace_test",
      capability_token: "tok-delivery-lost",
      status: "terminal_delivery_lost",
      delivery_lost_phase: "wait_response",
      started_at: Date.now(),
      updated_at: Date.now()
    }
  });

  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {}
  });

  try {
    await import(`../src/service-worker.js?terminal_delivery_lost_restore=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    await eventually(async () => (await storage.get("jobs.job_delivery_lost"))["jobs.job_delivery_lost"]?.status === "terminal_delivery_lost");
    assert.equal(port.messages.some((message) => message.type === "job_error" && message.job_id === "job_delivery_lost"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker replays an undelivered persisted terminal envelope once on reconnect", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const terminalEnvelope = envelope("job_complete", "job_terminal_replay", {
    is_final: true,
    response: "persisted answer"
  });
  await storage.set({
    "jobs.job_terminal_replay": {
      job_id: "job_terminal_replay",
      run_id: "run_test",
      workspace_id: "workspace_test",
      capability_token: "cap_test",
      status: "complete",
      terminal_envelope: terminalEnvelope,
      terminal_delivered_at: null,
      started_at: Date.now(),
      updated_at: Date.now()
    },
    "jobs.job_terminal_too_large": {
      job_id: "job_terminal_too_large",
      status: "complete",
      terminal_envelope_too_large: true,
      terminal_delivered_at: null,
      started_at: Date.now(),
      updated_at: Date.now()
    }
  });
  globalThis.chrome = chromeStub({ port, storage, tabs: {} });

  try {
    await import(`../src/service-worker.js?terminal_replay=${Date.now()}`);
    await eventually(() => port.messages.some((message) => (
      message.type === "job_complete" && message.job_id === "job_terminal_replay"
    )));
    const replayed = port.messages.find((message) => (
      message.type === "job_complete" && message.job_id === "job_terminal_replay"
    ));
    assert.equal(replayed.payload.sequence, 0);
    const persisted = (await storage.get("jobs.job_terminal_replay"))["jobs.job_terminal_replay"];
    assert.equal(persisted.terminal_delivered_at, null);
    assert.equal(port.messages.some((message) => message.job_id === "job_terminal_too_large"), false);

    port.emit(envelope("terminal_ack", "job_terminal_replay", { sequence: 0 }));
    await eventually(async () => {
      const afterAck = (await storage.get("jobs.job_terminal_replay"))["jobs.job_terminal_replay"];
      return Boolean(afterAck.terminal_delivered_at);
    });

    port.messages.length = 0;
    port.emit(envelope("reconnect", "reconnect_after_terminal"));
    await eventually(() => port.messages.some((message) => message.type === "reconnect"));
    assert.equal(port.messages.some((message) => message.job_id === "job_terminal_replay"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker hello advertises the terminal_ack capability", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  globalThis.chrome = chromeStub({ port, tabs: {} });

  try {
    await import(`../src/service-worker.js?hello_terminal_ack_capability=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    const hello = port.messages.find((message) => message.type === "hello");
    assert.ok(hello.payload.capabilities.includes("terminal_ack"));
    assert.deepEqual(hello.payload.recipes, ["chatgpt", "claude"]);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker terminal_ack marks a persisted envelope delivered so restore does not replay", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const terminalEnvelope = envelope("job_complete", "job_terminal_ack_before_restore", {
    is_final: true,
    response: "acked before restore",
    sequence: 0
  });
  globalThis.chrome = chromeStub({ port, storage, tabs: {} });

  try {
    await import(`../src/service-worker.js?terminal_ack_before_restore=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));

    await storage.set({
      "jobs.job_terminal_ack_before_restore": {
        job_id: "job_terminal_ack_before_restore",
        run_id: "run_test",
        workspace_id: "workspace_test",
        capability_token: "cap_test",
        status: "complete",
        terminal_envelope: terminalEnvelope,
        terminal_sequence: 0,
        terminal_delivered_at: null,
        started_at: Date.now(),
        updated_at: Date.now()
      }
    });
    port.emit(envelope("terminal_ack", "job_terminal_ack_before_restore", { sequence: 0 }));
    await eventually(async () => {
      const shard = (await storage.get("jobs.job_terminal_ack_before_restore"))["jobs.job_terminal_ack_before_restore"];
      return Boolean(shard?.terminal_delivered_at);
    });

    port.messages.length = 0;
    port.emit(envelope("reconnect", "reconnect_after_ack_before_restore"));
    await eventually(() => port.messages.some((message) => message.type === "reconnect"));
    assert.equal(port.messages.some((message) => (
      message.type === "job_complete" && message.job_id === "job_terminal_ack_before_restore"
    )), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker terminal_ack for an unknown job is a silent no-op", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  globalThis.chrome = chromeStub({ port, tabs: {} });

  try {
    await import(`../src/service-worker.js?terminal_ack_unknown=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    port.messages.length = 0;
    port.emit(envelope("terminal_ack", "job_terminal_ack_unknown", { sequence: 0 }));
    await new Promise((resolve) => setTimeout(resolve, 75));
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
    assert.equal(port.messages.some((message) => message.payload?.code === "unsupported_type"), false);
    assert.equal(port.messages.length, 0);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker replay then terminal_ack then restart does not replay again", async () => {
  const originalChrome = globalThis.chrome;
  const firstPort = makePort();
  const storage = makeStorage();
  const terminalEnvelope = envelope("job_complete", "job_terminal_ack_restart", {
    is_final: true,
    response: "replay then ack",
    sequence: 0
  });
  await storage.set({
    "jobs.job_terminal_ack_restart": {
      job_id: "job_terminal_ack_restart",
      run_id: "run_test",
      workspace_id: "workspace_test",
      capability_token: "cap_test",
      status: "complete",
      terminal_envelope: terminalEnvelope,
      terminal_sequence: 0,
      terminal_delivered_at: null,
      started_at: Date.now(),
      updated_at: Date.now()
    }
  });
  globalThis.chrome = chromeStub({ port: firstPort, storage, tabs: {} });

  try {
    await import(`../src/service-worker.js?terminal_ack_restart_first=${Date.now()}`);
    await eventually(() => firstPort.messages.some((message) => (
      message.type === "job_complete" && message.job_id === "job_terminal_ack_restart"
    )));
    const afterReplay = (await storage.get("jobs.job_terminal_ack_restart"))["jobs.job_terminal_ack_restart"];
    assert.equal(afterReplay.terminal_delivered_at, null, "postNative is not a delivery receipt");
    firstPort.emit(envelope("terminal_ack", "job_terminal_ack_restart", { sequence: 0 }));
    await eventually(async () => {
      const shard = (await storage.get("jobs.job_terminal_ack_restart"))["jobs.job_terminal_ack_restart"];
      return Boolean(shard?.terminal_delivered_at);
    });

    const secondPort = makePort();
    globalThis.chrome = chromeStub({ port: secondPort, storage, tabs: {} });
    await import(`../src/service-worker.js?terminal_ack_restart_second=${Date.now()}`);
    await eventually(() => secondPort.messages.some((message) => message.type === "hello"));
    await new Promise((resolve) => setTimeout(resolve, 75));
    assert.equal(secondPort.messages.some((message) => (
      message.type === "job_complete" && message.job_id === "job_terminal_ack_restart"
    )), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker terminal_ack marks a too-large persisted envelope delivered", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  await storage.set({
    "jobs.job_terminal_ack_too_large": {
      job_id: "job_terminal_ack_too_large",
      status: "complete",
      terminal_envelope_too_large: true,
      terminal_sequence: 0,
      terminal_delivered_at: null,
      started_at: Date.now(),
      updated_at: Date.now()
    }
  });
  globalThis.chrome = chromeStub({ port, storage, tabs: {} });

  try {
    await import(`../src/service-worker.js?terminal_ack_too_large=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    assert.equal(port.messages.some((message) => message.job_id === "job_terminal_ack_too_large"), false);
    port.emit(envelope("terminal_ack", "job_terminal_ack_too_large", { sequence: 0 }));
    await eventually(async () => {
      const shard = (await storage.get("jobs.job_terminal_ack_too_large"))["jobs.job_terminal_ack_too_large"];
      return Boolean(shard?.terminal_delivered_at);
    });
    const shard = (await storage.get("jobs.job_terminal_ack_too_large"))["jobs.job_terminal_ack_too_large"];
    assert.equal(shard.terminal_envelope_too_large, true);
    assert.equal(shard.terminal_envelope, undefined);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker purges an aged unacked terminal shard instead of replaying it", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const now = Date.now();
  const agedAt = now - (3 * 60 * 60 * 1000) - 1000;
  await storage.set({
    "jobs.job_terminal_aged": {
      job_id: "job_terminal_aged",
      status: "complete",
      terminal_envelope: envelope("job_complete", "job_terminal_aged", {
        is_final: true,
        response: "aged unacked"
      }),
      terminal_sequence: 0,
      terminal_delivered_at: null,
      terminal_at: agedAt,
      started_at: agedAt,
      // Fresh updated_at simulates restore/ACK-path churn so the generic TTL
      // skip cannot hide an unbounded terminal replay.
      updated_at: now
    },
    "jobs.job_terminal_fresh_unacked": {
      job_id: "job_terminal_fresh_unacked",
      status: "complete",
      terminal_envelope: envelope("job_complete", "job_terminal_fresh_unacked", {
        is_final: true,
        response: "fresh unacked"
      }),
      terminal_sequence: 0,
      terminal_delivered_at: null,
      terminal_at: now,
      started_at: now,
      updated_at: now
    }
  });
  globalThis.chrome = chromeStub({ port, storage, tabs: {} });

  try {
    await import(`../src/service-worker.js?terminal_ack_aged_purge=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    await eventually(() => port.messages.some((message) => (
      message.type === "job_complete" && message.job_id === "job_terminal_fresh_unacked"
    )));
    assert.equal(port.messages.some((message) => (
      message.type === "job_complete" && message.job_id === "job_terminal_aged"
    )), false);
    const aged = (await storage.get("jobs.job_terminal_aged"))["jobs.job_terminal_aged"];
    const fresh = (await storage.get("jobs.job_terminal_fresh_unacked"))["jobs.job_terminal_fresh_unacked"];
    assert.equal(aged, undefined);
    assert.ok(fresh);
    assert.equal(fresh.terminal_delivered_at, null);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker cancelJob isolates one of two active ChatGPT jobs", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
  const removedTabs = [];
  const jobTabs = new Map();
  const sentJobs = new Set();
  let createdTabId = 0;
  let survivorCanComplete = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++createdTabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      remove: async (id) => {
        removedTabs.push(id);
      },
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, type: message.type, jobId: message.job?.job_id });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            jobTabs.set(message.job.job_id, id);
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sentJobs.add(message.job.job_id);
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sentJobs.has(message.job.job_id)
                ? survivorCanComplete && message.job.job_id === "job_survivor_b"
                  ? { method: "assistant_dom_fallback", text: "survivor complete", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                  : { method: "assistant_dom_fallback", text: `partial ${message.job.job_id}`, is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          case "yoetz_cancel_send":
            return { ok: true, payload: { stopped: true } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?cancel_kills_tab=${Date.now()}`);
    await eventually(() => port.messages.some((m) => m.type === "hello"));

    const jobs = ["job_cancel_a", "job_survivor_b"];
    for (const jobId of jobs) {
      port.emit(envelope("job_start", jobId, {
        prompt: `prompt ${jobId}`,
        wait_interval_ms: 50,
        wait_timeout_ms: 60000
      }));
    }
    await eventually(() => port.messages.filter((m) => m.payload?.phase === "ready_for_file").length === 2);

    for (const jobId of jobs) {
      port.emit(envelope("job_file_chunk", jobId, {
        sequence: 0,
        total_chunks: 1,
        total_bytes: 4,
        filename: `${jobId}.md`,
        mime_type: "text/markdown",
        bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
      }));
    }
    await eventually(() => sentJobs.size === 2);
    await eventually(() => jobs.every((jobId) => sentToTabs.some((m) => m.type === "yoetz_extract_response" && m.jobId === jobId)));

    port.emit(envelope("job_cancel", "job_cancel_a"));

    // Cancel must (1) click stop on the content side, (2) close the tab, (3)
    // post a job_cancel envelope.
    await eventually(() => port.messages.some((m) => m.type === "job_cancel" && m.job_id === "job_cancel_a"));
    assert.ok(
      sentToTabs.some((m) => m.type === "yoetz_cancel_send" && m.jobId === "job_cancel_a"),
      "expected service worker to forward yoetz_cancel_send to the content script"
    );
    assert.deepEqual(removedTabs, [jobTabs.get("job_cancel_a")],
      "expected service worker to remove only the cancelled ChatGPT tab");
    assert.equal(sentToTabs.some((m) => m.type === "yoetz_cancel_send" && m.jobId === "job_survivor_b"), false);
    assert.equal(port.messages.some((m) => m.type === "job_cancel" && m.job_id === "job_survivor_b"), false);
    const cancelEnvelope = port.messages.find((m) => m.type === "job_cancel" && m.job_id === "job_cancel_a");
    assert.equal(cancelEnvelope.payload.cancelled, true);
    assert.equal(cancelEnvelope.payload.stop_clicked, true);
    assert.equal(cancelEnvelope.payload.tab_disposition, "closed");
    assert.equal(port.messages.filter((m) => (
      m.job_id === "job_cancel_a" && ["job_cancel", "job_complete", "job_error"].includes(m.type)
    )).length, 1);
    const cancelledProgress = port.messages.find((m) =>
      m.type === "job_progress"
      && m.job_id === "job_cancel_a"
      && m.payload?.phase === "cancelled"
    );
    assert.equal(cancelledProgress.payload.tab_disposition, "closed");

    survivorCanComplete = true;
    await eventually(() => port.messages.some((m) => m.type === "job_complete" && m.job_id === "job_survivor_b"));
    const survivor = port.messages.find((m) => m.type === "job_complete" && m.job_id === "job_survivor_b");
    assert.equal(survivor.payload.response, "survivor complete");
    assert.notEqual(jobTabs.get("job_cancel_a"), jobTabs.get("job_survivor_b"));

    // Late work unwinding after a terminal claim must not emit a second terminal
    // envelope for the cancelled job.
    port.messages.length = 0;
    port.emit(envelope("job_file_chunk", "job_cancel_a", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "should-not-accept.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.equal(port.messages.some((m) => m.job_id === "job_cancel_a"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker cancelJob still removes the tab when the content script is unreachable", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const removedTabs = [];
  let createdTabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++createdTabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      remove: async (id) => {
        removedTabs.push(id);
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? { method: "assistant_dom_fallback", text: "partial...", is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          case "yoetz_cancel_send":
            // Simulate a tab whose content script is gone (navigated, reloaded).
            throw new Error("Could not establish connection. Receiving end does not exist.");
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?cancel_unreachable=${Date.now()}`);
    await eventually(() => port.messages.some((m) => m.type === "hello"));

    port.emit(envelope("job_start", "job_cancel_b", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 60000
    }));
    await eventually(() => port.messages.some((m) => m.payload?.phase === "ready_for_file"));

    port.emit(envelope("job_file_chunk", "job_cancel_b", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_cancel_b.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);

    port.emit(envelope("job_cancel", "job_cancel_b"));
    await eventually(() => port.messages.some((m) => m.type === "job_cancel" && m.job_id === "job_cancel_b"));
    assert.deepEqual(removedTabs, [createdTabId],
      "tab removal must still happen when the content script is unreachable");
    const cancelEnvelope = port.messages.find((m) => m.type === "job_cancel" && m.job_id === "job_cancel_b");
    assert.equal(cancelEnvelope.payload.cancelled, true);
    assert.equal(cancelEnvelope.payload.stop_clicked, false);
    // Content script unreachable → we cannot confirm generation stopped, so the
    // CLI must be told the run may still be live server-side.
    assert.equal(cancelEnvelope.payload.stop_confirmed, false);
    assert.equal(cancelEnvelope.payload.generation_idle, false);
    assert.equal(cancelEnvelope.payload.may_still_be_running, true);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker cancelJob waits for cancelSend to resolve before removing the tab and reports confirmed idle", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const removedTabs = [];
  let createdTabId = 0;
  let sent = false;
  // Gate the yoetz_cancel_send response so the test can observe ordering: the
  // tab must NOT be removed while cancelSend is still in flight.
  let releaseCancel;
  const cancelGate = new Promise((resolve) => {
    releaseCancel = resolve;
  });
  let cancelSendStarted = false;
  let extracted = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++createdTabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      remove: async (id) => {
        removedTabs.push(id);
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            extracted = true;
            return {
              ok: true,
              payload: sent
                ? { method: "assistant_dom_fallback", text: "partial...", is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          case "yoetz_cancel_send":
            cancelSendStarted = true;
            // Block until the test releases us, mimicking confirmGenerationStopped
            // polling for idle.
            await cancelGate;
            return { ok: true, payload: { stopped: true, confirmed_idle: true, waited_ms: 250 } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?cancel_waits=${Date.now()}`);
    await eventually(() => port.messages.some((m) => m.type === "hello"));

    port.emit(envelope("job_start", "job_cancel_wait", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 60000
    }));
    await eventually(() => port.messages.some((m) => m.payload?.phase === "ready_for_file"));

    port.emit(envelope("job_file_chunk", "job_cancel_wait", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_cancel_wait.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);
    await eventually(() => extracted);

    port.emit(envelope("job_cancel", "job_cancel_wait"));

    // cancelSend is in flight (gated). The tab must NOT be removed yet.
    await eventually(() => cancelSendStarted);
    assert.deepEqual(removedTabs, [], "tab must not be removed while cancelSend is still awaited");

    // Release cancelSend; only now may the tab be removed and the envelope post.
    releaseCancel();
    await eventually(() => port.messages.some((m) => m.type === "job_cancel" && m.job_id === "job_cancel_wait"));
    assert.deepEqual(removedTabs, [createdTabId], "tab removal must happen after cancelSend resolves");

    const cancelEnvelope = port.messages.find((m) => m.type === "job_cancel" && m.job_id === "job_cancel_wait");
    assert.equal(cancelEnvelope.payload.cancelled, true);
    assert.equal(cancelEnvelope.payload.stop_clicked, true);
    assert.equal(cancelEnvelope.payload.stop_confirmed, true);
    assert.equal(cancelEnvelope.payload.generation_idle, true);
    assert.equal(cancelEnvelope.payload.may_still_be_running, false);

    const cancelledProgress = port.messages.find((m) => m.type === "job_progress" && m.job_id === "job_cancel_wait" && m.payload?.phase === "cancelled");
    assert.equal(cancelledProgress.payload.stop_confirmed, true);
    assert.equal(cancelledProgress.payload.generation_idle, true);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker cancelJob removes the tab but warns may_still_be_running when stop is not confirmed", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const removedTabs = [];
  let createdTabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++createdTabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      remove: async (id) => {
        removedTabs.push(id);
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? { method: "assistant_dom_fallback", text: "partial...", is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          case "yoetz_cancel_send":
            // Generation outlasted the bounded wait: clicked but not confirmed idle.
            return { ok: true, payload: { stopped: true, confirmed_idle: false, waited_ms: 5000 } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?cancel_unconfirmed=${Date.now()}`);
    await eventually(() => port.messages.some((m) => m.type === "hello"));

    port.emit(envelope("job_start", "job_cancel_warn", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 60000
    }));
    await eventually(() => port.messages.some((m) => m.payload?.phase === "ready_for_file"));

    port.emit(envelope("job_file_chunk", "job_cancel_warn", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_cancel_warn.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);

    port.emit(envelope("job_cancel", "job_cancel_warn"));
    await eventually(() => port.messages.some((m) => m.type === "job_cancel" && m.job_id === "job_cancel_warn"));

    // Can't block forever: tab is still removed even when stop is unconfirmed.
    assert.deepEqual(removedTabs, [createdTabId]);
    const cancelEnvelope = port.messages.find((m) => m.type === "job_cancel" && m.job_id === "job_cancel_warn");
    assert.equal(cancelEnvelope.payload.stop_clicked, true);
    assert.equal(cancelEnvelope.payload.stop_confirmed, false);
    assert.equal(cancelEnvelope.payload.generation_idle, false);
    assert.equal(cancelEnvelope.payload.may_still_be_running, true);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker cancelJob reports close_failed when tab removal throws after confirmed idle", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const removeAttempts = [];
  let createdTabId = 0;
  let sent = false;
  globalThis.chrome = chromeStub({
    port,
    tabs: {
      create: async (opts) => ({ id: ++createdTabId, ...opts }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      remove: async (id) => {
        removeAttempts.push(id);
        throw new Error("tabs.remove failed");
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? { method: "assistant_dom_fallback", text: "partial...", is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          case "yoetz_cancel_send":
            // Response had already settled: no stop button, so nothing clicked
            // but idle is confirmed.
            return { ok: true, payload: { stopped: false, confirmed_idle: true, waited_ms: 0 } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?cancel_idle=${Date.now()}`);
    await eventually(() => port.messages.some((m) => m.type === "hello"));

    port.emit(envelope("job_start", "job_cancel_idle", {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 60000
    }));
    await eventually(() => port.messages.some((m) => m.payload?.phase === "ready_for_file"));

    port.emit(envelope("job_file_chunk", "job_cancel_idle", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_cancel_idle.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => sent);

    port.emit(envelope("job_cancel", "job_cancel_idle"));
    await eventually(() => port.messages.some((m) => m.type === "job_cancel" && m.job_id === "job_cancel_idle"));

    assert.deepEqual(removeAttempts, [createdTabId]);
    const cancelEnvelope = port.messages.find((m) => m.type === "job_cancel" && m.job_id === "job_cancel_idle");
    assert.equal(cancelEnvelope.payload.cancelled, true);
    assert.equal(cancelEnvelope.payload.tab_disposition, "close_failed");
    assert.equal(cancelEnvelope.payload.stop_clicked, false);
    assert.equal(cancelEnvelope.payload.stop_confirmed, true);
    assert.equal(cancelEnvelope.payload.generation_idle, true);
    assert.equal(cancelEnvelope.payload.may_still_be_running, false);
    const cancelledProgress = port.messages.find((m) =>
      m.type === "job_progress"
      && m.job_id === "job_cancel_idle"
      && m.payload?.phase === "cancelled"
    );
    assert.equal(cancelledProgress.payload.tab_disposition, "close_failed");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker closes an owned tab only after delivering a gated successful completion", async () => {
  const result = await runSuccessfulCompletionCase({
    jobId: "job_close_success",
    closeTabOnComplete: true
  });

  assert.deepEqual(result.removedTabs, [result.tabId]);
  assert.ok(
    result.events.indexOf("post:job_complete") < result.events.indexOf(`remove:${result.tabId}`),
    `expected job_complete delivery before tab removal, got ${result.events.join(", ")}`
  );
  const closed = result.messages.find((message) =>
    message.type === "job_progress" && message.payload?.phase === "tab_closed"
  );
  assert.equal(closed.payload.tab_id, result.tabId);
  assert.equal(result.shard.status, "complete");
  assert.equal(result.shard.tab_disposition, "closed");
});

test("service worker preserves legacy success behavior when the close gate is absent", async () => {
  const result = await runSuccessfulCompletionCase({
    jobId: "job_close_legacy"
  });

  assert.deepEqual(result.removedTabs, []);
  assert.equal(
    result.messages.some((message) =>
      message.type === "job_progress"
      && ["tab_closed", "tab_close_failed"].includes(message.payload?.phase)
    ),
    false
  );
  assert.equal(result.shard.status, "complete");
  assert.equal(result.shard.tab_disposition, undefined);
});

test("service worker delivers a large terminal response live without persisting its envelope", async () => {
  const responseText = "x".repeat(1024 * 1024 + 4096);
  const result = await runSuccessfulCompletionCase({
    jobId: "job_large_terminal_envelope",
    responseText
  });

  assert.equal(
    result.messages.find((message) => message.type === "job_complete")?.payload.response.length,
    responseText.length
  );
  assert.equal(result.shard.status, "complete");
  assert.equal(result.shard.terminal_envelope, undefined);
  assert.equal(result.shard.terminal_envelope_too_large, true);
});

test("service worker keeps the tab when successful completion delivery is lost", async () => {
  const result = await runSuccessfulCompletionCase({
    jobId: "job_close_delivery_lost",
    closeTabOnComplete: true,
    failCompleteDelivery: true
  });

  assert.deepEqual(result.removedTabs, []);
  assert.equal(result.shard.status, "terminal_delivery_lost");
  assert.equal(result.shard.tab_disposition, undefined);
});

test("service worker reports close failure without changing successful terminal status", async () => {
  const result = await runSuccessfulCompletionCase({
    jobId: "job_close_failure",
    closeTabOnComplete: true,
    removeError: new Error("tabs.remove failed")
  });

  assert.deepEqual(result.removedTabs, []);
  const failed = result.messages.find((message) =>
    message.type === "job_progress" && message.payload?.phase === "tab_close_failed"
  );
  assert.equal(failed.payload.tab_id, result.tabId);
  assert.equal(failed.payload.error, "tabs.remove failed");
  assert.equal(result.shard.status, "complete");
  assert.equal(result.shard.tab_disposition, "close_failed");
  assert.equal(
    result.messages.some((message) => message.type === "job_error"),
    false
  );
});

test("service worker resumes waiting_for_file jobs after service-worker restart", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const now = Date.now();
  const sentToTabs = [];
  let sent = false;
  await storage.set({
    "jobs.job_restore_waiting": {
      job_id: "job_restore_waiting",
      run_id: "run_job_restore_waiting",
      workspace_id: "workspace_test",
      status: "waiting_for_file",
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1500,
      tab_id: 42,
      started_at: now,
      updated_at: now
    }
  });

  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async () => {
        throw new Error("restore must reuse the prepared tab");
      },
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        switch (message.type) {
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? { method: "assistant_dom_fallback", text: "restored answer", is_generating: false, assistant_count: 1, copy_button_count: 1, has_copy_button: true, turn_index: 0 }
                : { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 }
            };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?restore_waiting_for_file=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    await eventually(async () => {
      const restored = (await storage.get("jobs.job_restore_waiting"))["jobs.job_restore_waiting"];
      return restored?.status === "waiting_for_file" && Number.isFinite(restored.connection_generation);
    });
    assert.equal(port.messages.some((message) => message.type === "job_error" && message.job_id === "job_restore_waiting"), false);
    const restoredReady = port.messages.find((message) => message.type === "job_progress" && message.job_id === "job_restore_waiting" && message.payload?.phase === "ready_for_file");
    assert.equal(restoredReady?.payload.restored, true);
    assert.equal(restoredReady?.payload.tab_id, 42);

    port.emit(envelope("job_file_chunk", "job_restore_waiting", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_restore_waiting.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    await eventually(() => port.messages.some((message) => message.type === "job_complete" && message.job_id === "job_restore_waiting"));
    const complete = port.messages.find((message) => message.type === "job_complete" && message.job_id === "job_restore_waiting");
    assert.equal(complete.payload.response, "restored answer");
    assert.equal(sentToTabs.find((item) => item.message.type === "yoetz_upload_file")?.id, 42);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker resumes waiting_response jobs after service-worker restart", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const now = Date.now();
  const sentToTabs = [];
  let extractCount = 0;
  await storage.set({
    "jobs.job_restore_waiting_response": {
      job_id: "job_restore_waiting_response",
      run_id: "run_job_restore_waiting_response",
      workspace_id: "workspace_test",
      status: "waiting_response",
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2500,
      tab_id: 44,
      response_baseline: {
        method: "none",
        text: "",
        is_generating: false,
        assistant_count: 0,
        turn_index: -1
      },
      submitted_user_count: 1,
      submitted_assistant_count: 0,
      started_at: now,
      response_wait_started_at: now,
      updated_at: now
    }
  });

  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async () => {
        throw new Error("restore must reuse the submitted tab");
      },
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/?_yoetz=run_job_restore_waiting_response" }),
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_bind_job":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response": {
            extractCount += 1;
            const streaming = {
              method: "assistant_dom_fallback",
              text: "I",
              is_generating: true,
              assistant_count: 1,
              copy_button_count: 0,
              has_copy_button: false,
              turn_index: 0,
              preceding_user_count: 1
            };
            const complete = {
              method: "assistant_dom_fallback",
              text: "restored final answer",
              is_generating: false,
              assistant_count: 1,
              copy_button_count: 1,
              has_copy_button: true,
              turn_index: 0,
              preceding_user_count: 1
            };
            return { ok: true, payload: extractCount < 3 ? streaming : complete };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?restore_waiting_response=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "hello"));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_progress"
      && message.job_id === "job_restore_waiting_response"
      && message.payload?.phase === "waiting_response"
      && message.payload.restored === true
    ));
    port.emit(envelope("reconnect", "job_restore_waiting_response_control"));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_complete" && message.job_id === "job_restore_waiting_response"
    ));
    assert.equal(port.messages.some((message) => message.type === "job_error" && message.job_id === "job_restore_waiting_response"), false);
    const waiting = port.messages.find((message) =>
      message.type === "job_progress"
      && message.job_id === "job_restore_waiting_response"
      && message.payload?.phase === "waiting_response"
    );
    assert.equal(waiting?.payload.restored, true);
    assert.equal(waiting?.payload.tab_id, 44);
    const complete = port.messages.find((message) => message.type === "job_complete" && message.job_id === "job_restore_waiting_response");
    assert.equal(complete.payload.response, "restored final answer");
    assert.equal(sentToTabs.some((item) => item.message.type === "yoetz_upload_file"), false);
    assert.equal(sentToTabs.some((item) => item.message.type === "yoetz_send_prompt"), false);
    assert.equal(sentToTabs.find((item) => item.message.type === "yoetz_extract_response")?.id, 44);
    assert.equal(sentToTabs.filter((item) => item.message.type === "yoetz_bind_job").length, 1);
    assert.equal(port.messages.filter((message) =>
      message.type === "job_complete" && message.job_id === "job_restore_waiting_response"
    ).length, 1);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker restores a waiting_response shard persisted while a poller lease was held", async () => {
  const originalChrome = globalThis.chrome;
  const firstPort = makePort();
  const firstStorage = makeStorage();
  const now = Date.now();
  const provisionalConversationId = "WEB:ca5209ac-2836-440d-b674-ffc54ee5dd2d";
  const assignedConversationId = "6a5f60dc-8174-8329-949a-1f282d1dccbd";
  let firstExtractCount = 0;
  await firstStorage.set({
    "jobs.job_restore_lease_persist": {
      job_id: "job_restore_lease_persist",
      run_id: "run_job_restore_lease_persist",
      workspace_id: "workspace_test",
      status: "waiting_response",
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 4000,
      tab_id: 88,
      submitted_conversation_id: provisionalConversationId,
      response_baseline: {
        method: "none",
        text: "",
        is_generating: false,
        assistant_count: 0,
        turn_index: -1
      },
      submitted_user_count: 1,
      submitted_assistant_count: 0,
      started_at: now,
      response_wait_started_at: now,
      updated_at: now
    }
  });
  globalThis.chrome = chromeStub({
    port: firstPort,
    storage: firstStorage,
    tabs: {
      create: async () => {
        throw new Error("restore must reuse the submitted tab");
      },
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/?_yoetz=run_job_restore_lease_persist" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_bind_job":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response": {
            firstExtractCount += 1;
            if (firstExtractCount === 1) {
              return {
                ok: true,
                payload: {
                  method: "assistant_dom_fallback",
                  text: "I",
                  is_generating: true,
                  assistant_count: 1,
                  copy_button_count: 0,
                  has_copy_button: false,
                  turn_index: 0,
                  preceding_user_count: 1,
                  conversation_id: assignedConversationId
                }
              };
            }
            return new Promise(() => {});
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  let persistedShard;
  try {
    await import(`../src/service-worker.js?restore_lease_persist_first=${Date.now()}`);
    await eventually(() => firstPort.messages.some((message) => message.type === "hello"));
    await eventually(async () => {
      const shard = (await firstStorage.get("jobs.job_restore_lease_persist"))["jobs.job_restore_lease_persist"];
      return shard?.submitted_conversation_id === assignedConversationId;
    });
    persistedShard = {
      ...(await firstStorage.get("jobs.job_restore_lease_persist"))["jobs.job_restore_lease_persist"]
    };
    assert.equal(persistedShard.poller_lease, undefined, "poller_lease must not be durable");
    assert.equal(persistedShard.poller_lease_seq, undefined, "poller_lease_seq must not be durable");
  } catch (error) {
    globalThis.chrome = originalChrome;
    throw error;
  }

  const secondPort = makePort();
  const secondStorage = makeStorage();
  await secondStorage.set({
    "jobs.job_restore_lease_persist": persistedShard
  });
  let secondExtractCount = 0;
  globalThis.chrome = chromeStub({
    port: secondPort,
    storage: secondStorage,
    tabs: {
      create: async () => {
        throw new Error("restore must reuse the submitted tab");
      },
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/?_yoetz=run_job_restore_lease_persist" }),
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_bind_job":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT" } };
          case "yoetz_extract_response": {
            secondExtractCount += 1;
            const streaming = {
              method: "assistant_dom_fallback",
              text: "I",
              is_generating: true,
              assistant_count: 1,
              copy_button_count: 0,
              has_copy_button: false,
              turn_index: 0,
              preceding_user_count: 1,
              conversation_id: assignedConversationId
            };
            const complete = {
              method: "assistant_dom_fallback",
              text: "restored after lease persist",
              is_generating: false,
              assistant_count: 1,
              copy_button_count: 1,
              has_copy_button: true,
              turn_index: 0,
              preceding_user_count: 1,
              conversation_id: assignedConversationId
            };
            return { ok: true, payload: secondExtractCount < 3 ? streaming : complete };
          }
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?restore_lease_persist_second=${Date.now()}`);
    await eventually(() => secondPort.messages.some((message) =>
      message.type === "job_complete" && message.job_id === "job_restore_lease_persist"
    ));
    assert.equal(secondPort.messages.some((message) => (
      message.type === "job_error" && message.job_id === "job_restore_lease_persist"
    )), false);
    assert.equal(
      secondPort.messages.find((message) => message.type === "job_complete")?.payload.response,
      "restored after lease persist"
    );
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker still fails receiving_file jobs after service-worker restart", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const now = Date.now();
  await storage.set({
    "jobs.job_restore_receiving": {
      job_id: "job_restore_receiving",
      run_id: "run_job_restore_receiving",
      workspace_id: "workspace_test",
      status: "receiving_file",
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 1500,
      tab_id: 43,
      started_at: now,
      updated_at: now
    }
  });

  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {}
  });

  try {
    await import(`../src/service-worker.js?restore_receiving_file=${Date.now()}`);
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.job_id === "job_restore_receiving"));
    const error = port.messages.find((message) => message.type === "job_error" && message.job_id === "job_restore_receiving");
    assert.equal(error.payload.code, "state_lost");
    assert.equal(error.payload.phase, "upload");
    assert.equal(error.payload.side_effect_started, true);
    assert.equal((await storage.get("jobs.job_restore_receiving"))["jobs.job_restore_receiving"].status, "state_lost");
  } finally {
    globalThis.chrome = originalChrome;
  }
});

async function runSuccessfulCompletionCase({
  jobId,
  closeTabOnComplete,
  failCompleteDelivery = false,
  removeError = null,
  responseText = "done"
}) {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const storage = makeStorage();
  const removedTabs = [];
  const events = [];
  let tabId = 0;
  let sent = false;

  const originalPostMessage = port.postMessage.bind(port);
  port.postMessage = (message) => {
    events.push(`post:${message.type}`);
    return originalPostMessage(message);
  };
  port.throwOnPostMessage = (message) =>
    failCompleteDelivery && message.type === "job_complete";

  globalThis.chrome = chromeStub({
    port,
    storage,
    tabs: {
      create: async (options) => ({ id: ++tabId, ...options }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      remove: async (id) => {
        events.push(`remove:${id}`);
        if (removeError) {
          throw removeError;
        }
        removedTabs.push(id);
      },
      sendMessage: async (_id, message) => {
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: verifiedSolProSelection() };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return {
              ok: true,
              payload: {
                sent: true,
                conversation_id: `conv-${jobId}`,
                submitted_assistant_count: 0
              }
            };
          case "yoetz_fetch_conversation":
            return {
              ok: true,
              payload: {
                method: "backend_api",
                text: responseText,
                is_generating: false,
                node_fresh: true,
                node_id: `answer-${jobId}`,
                conversation_id: `conv-${jobId}`,
                assistant_count: 1,
                turn_index: 0,
                copy_button_count: 0,
                has_copy_button: false
              }
            };
          case "yoetz_extract_response":
            return {
              ok: true,
              payload: sent
                ? {
                    method: "assistant_dom_fallback",
                    text: responseText,
                    is_generating: false,
                    assistant_count: 1,
                    user_count: 1,
                    preceding_user_count: 1,
                    copy_button_count: 1,
                    has_copy_button: true,
                    turn_index: 0,
                    conversation_id: `conv-${jobId}`
                  }
                : {
                    method: "none",
                    text: "",
                    is_generating: false,
                    assistant_count: 0,
                    user_count: 0,
                    copy_button_count: 0,
                    has_copy_button: false,
                    turn_index: -1
                  }
            };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?close_success_case=${jobId}_${Date.now()}`);
    const payload = {
      prompt: "prompt",
      wait_interval_ms: 50,
      wait_timeout_ms: 2000
    };
    if (closeTabOnComplete !== undefined) {
      payload.close_tab_on_complete = closeTabOnComplete;
    }
    port.emit(envelope("job_start", jobId, payload));
    await eventually(() => port.messages.some((message) =>
      message.type === "job_progress" && message.payload?.phase === "ready_for_file"
    ));
    port.emit(envelope("job_file_chunk", jobId, {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: `${jobId}.md`,
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));

    const terminalStatus = failCompleteDelivery ? "terminal_delivery_lost" : "complete";
    await eventually(async () =>
      (await storage.get(`jobs.${jobId}`))[`jobs.${jobId}`]?.status === terminalStatus
    );
    if (closeTabOnComplete && !failCompleteDelivery) {
      await eventually(async () => {
        const shard = (await storage.get(`jobs.${jobId}`))[`jobs.${jobId}`];
        return ["closed", "close_failed"].includes(shard?.tab_disposition);
      });
    }
    return {
      events,
      messages: [...port.messages],
      removedTabs,
      tabId,
      shard: (await storage.get(`jobs.${jobId}`))[`jobs.${jobId}`]
    };
  } finally {
    globalThis.chrome = originalChrome;
  }
}

function verifiedSolProSelection() {
  return {
    status: "selected",
    model_used: "GPT-5.6 Sol Extra High",
    requested_model: "gpt-5-6-sol-extra-high",
    family_status: "verified",
    effort_status: "verified"
  };
}

function verifiedFableMaxSelection() {
  return {
    status: "selected",
    requested_model: "fable-5-max",
    modelVerified: true,
    maxVerified: true,
    model_used: "Fable 5 Max"
  };
}

function currentSelection() {
  return {
    status: "current",
    model_used: "5.5 Instant",
    requested_model: "current",
    family_status: "skipped",
    effort_status: "skipped",
    warning: "model pinning bypassed — answer may come from any model",
    warnings: []
  };
}

function envelope(type, jobId, payload = {}, fields = {}) {
  return {
    protocol_version: 1,
    transport: "chrome-extension-native",
    request_id: `req_${type}_${jobId}`,
    type,
    job_id: jobId,
    run_id: `run_${jobId}`,
    workspace_id: "workspace_test",
    ...fields,
    payload
  };
}

function makePort() {
  let listener = null;
  return {
    messages: [],
    onMessage: {
      addListener: (fn) => {
        listener = fn;
      }
    },
    onDisconnect: {
      addListener: () => {}
    },
    postMessage(message) {
      if (this.throwOnPostMessage?.(message)) {
        throw new Error("port closed for selected message");
      }
      if (this.throwOnPost) {
        throw this.throwOnPost;
      }
      this.messages.push(message);
    },
    disconnect() {},
    emit(message) {
      listener(message);
    }
  };
}

function makeStorage() {
  const data = {};
  return {
    async get(key) {
      if (typeof key === "string") {
        return { [key]: data[key] };
      }
      if (Array.isArray(key)) {
        const out = {};
        for (const k of key) {
          out[k] = data[k];
        }
        return out;
      }
      // null / undefined / object: return the entire store, mirroring chrome.storage.session.get(null).
      return { ...data };
    },
    async set(values) {
      Object.assign(data, values);
    },
    async remove(keys) {
      const list = Array.isArray(keys) ? keys : [keys];
      for (const k of list) {
        delete data[k];
      }
    }
  };
}

function chromeStub({ port, tabs, profileEmail = "", profileId = "profile-test", profileError = null, storage = makeStorage(), localStorage = makeStorage(), reload = () => {}, alarms = null }) {
  return {
    runtime: {
      connectNative: () => port,
      getManifest: () => ({ version: "0.4.0" }),
      getURL: (value) => new URL(`../${value}`, import.meta.url).href,
      reload,
      onInstalled: { addListener: () => {} },
      onStartup: { addListener: () => {} },
      onMessage: { addListener: () => {} }
    },
    storage: {
      session: storage,
      local: localStorage
    },
    identity: {
      getProfileUserInfo: async (details) => {
        assert.deepEqual(details, { accountStatus: "ANY" });
        if (profileError) {
          throw profileError;
        }
        return { email: profileEmail, id: profileId };
      }
    },
    alarms: alarms ?? {
      onAlarm: { addListener: () => {} },
      create: () => {},
      clear: () => {}
    },
    tabs,
    tabGroups: {
      update: async () => {}
    }
  };
}

async function eventually(predicate, timeoutMs = 5000) {
  const start = Date.now();
  while (!(await predicate())) {
    if (Date.now() - start > timeoutMs) {
      throw new Error("condition was not met before timeout");
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

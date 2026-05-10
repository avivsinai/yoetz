import assert from "node:assert/strict";
import test from "node:test";
import { uint8ArrayToBase64 } from "../src/chunks.js";

globalThis.__YOETZ_MIN_STABLE_IDLE_MS = 100;

test("service worker routes reconnect and multiplexes two native jobs", async () => {
  const originalChrome = globalThis.chrome;
  const originalSetInterval = globalThis.setInterval;
  const originalClearInterval = globalThis.clearInterval;
  const port = makePort();
  const sentToTabs = [];
  const sentJobs = new Set();
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
      create: async () => ({ id: ++tabId }),
      get: async (id) => ({ id, status: "complete", url: "https://chatgpt.com/" }),
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, message });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: { url: "https://chatgpt.com/", title: "ChatGPT", text: "" } };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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

    port.emit(envelope("reconnect", "job_reconnect"));
    await eventually(() => port.messages.some((message) => message.type === "reconnect" && message.job_id === "job_reconnect"));

    port.messages.length = 0;
    const jobs = ["job_a", "job_b"];
    for (const jobId of jobs) {
      port.emit(envelope("job_start", jobId, {
        prompt: `prompt ${jobId}`,
        model: "current",
        disable_extended: jobId === "job_b",
        wait_interval_ms: 500,
        wait_timeout_ms: 2500
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
    assert.equal(sentToTabs.filter((item) => item.message.type === "yoetz_upload_file").length, 2);
    assert.equal(
      sentToTabs.find((item) => item.message.type === "yoetz_configure_model" && item.message.job.job_id === "job_b")?.message.job.disable_extended,
      true
    );
  } finally {
    globalThis.chrome = originalChrome;
    globalThis.setInterval = originalSetInterval;
    globalThis.clearInterval = originalClearInterval;
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
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails closed when an explicit model is unavailable", async () => {
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
                requested_model: "pro",
                available_options: ["Default"],
                warning: "requested ChatGPT model pro was not visible"
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
      model: "pro"
    }));
    await eventually(() => port.messages.some((message) => message.type === "job_error"));
    const error = port.messages.find((message) => message.type === "job_error");
    assert.equal(error.payload.code, "model_selection_failed");
    assert.equal(error.payload.phase, "model_selection");
    assert.equal(error.payload.side_effect_started, false);
    assert.equal(error.payload.model_selection_status, "unavailable");
    assert.equal(sentToTabs.includes("yoetz_upload_file"), false);
    assert.equal(sentToTabs.includes("yoetz_send_prompt"), false);
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("service worker fails closed when explicit model selection is only kept_current", async () => {
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
      model: "pro"
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

test("service worker treats auto model selection as best-effort", async () => {
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
    await import(`../src/service-worker.js?model_auto_best_effort=${Date.now()}`);
    port.emit(envelope("job_start", "job_auto_best_effort", {
      prompt: "prompt",
      model: "auto"
    }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    assert.equal(port.messages.some((message) => message.type === "job_error"), false);
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?duplicate_job=${Date.now()}`);
    port.emit(envelope("job_start", "job_duplicate", { prompt: "prompt", model: "current" }));
    await eventually(() => port.messages.some((message) => message.payload?.phase === "ready_for_file"));
    port.emit(envelope("job_start", "job_duplicate", { prompt: "prompt", model: "current" }));
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
          default:
            throw new Error(`unexpected tab message ${message.type}`);
        }
      }
    }
  });

  try {
    await import(`../src/service-worker.js?capability_mismatch=${Date.now()}`);
    port.emit(envelope("job_start", "job_capability", { prompt: "prompt", model: "current" }, { capability_token: "secret" }));
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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

test("service worker does not complete on thought/status-only assistant text", async () => {
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
    await import(`../src/service-worker.js?thought_only=${Date.now()}`);
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
    await eventually(() => port.messages.some((message) => message.type === "job_error" && message.payload.code === "response_timeout"));
    assert.equal(port.messages.some((message) => message.type === "job_complete"), false);
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
      model: "current",
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
      model: "current",
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_extract_response":
            return { ok: true, payload: { method: "none", text: "", is_generating: false, assistant_count: 0, turn_index: -1 } };
          case "yoetz_send_prompt":
            return {
              ok: false,
              code: "send_acceptance_unknown",
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
      model: "current"
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
      model: "current"
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
        model: "current",
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
      model: "current",
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
      model: "current",
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

test("service worker cancelJob clicks stop, removes tab, and evicts the in-memory job", async () => {
  const originalChrome = globalThis.chrome;
  const port = makePort();
  const sentToTabs = [];
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
      sendMessage: async (id, message) => {
        sentToTabs.push({ id, type: message.type, jobId: message.job?.job_id });
        switch (message.type) {
          case "yoetz_probe":
            return { ok: true, payload: {} };
          case "yoetz_prepare_job":
            return { ok: true, payload: { manual_handoff: null } };
          case "yoetz_configure_model":
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
          case "yoetz_upload_file":
            return { ok: true, payload: { filename: message.file.filename, size: 4 } };
          case "yoetz_send_prompt":
            sent = true;
            return { ok: true, payload: { sent: true } };
          case "yoetz_extract_response":
            // Keep the response perpetually "still generating" so the worker
            // remains in waitForResponse until cancel arrives.
            return {
              ok: true,
              payload: sent
                ? { method: "assistant_dom_fallback", text: "partial...", is_generating: true, assistant_count: 1, copy_button_count: 0, has_copy_button: false, turn_index: 0 }
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

    port.emit(envelope("job_start", "job_cancel_a", {
      prompt: "prompt",
      model: "current",
      wait_interval_ms: 50,
      wait_timeout_ms: 60000
    }));
    await eventually(() => port.messages.some((m) => m.payload?.phase === "ready_for_file"));

    port.emit(envelope("job_file_chunk", "job_cancel_a", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "job_cancel_a.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    // Wait for the prompt to be sent so the job is mid-response when we cancel.
    await eventually(() => sent);
    // Wait for at least one extract_response cycle so the job is firmly inside
    // waitForResponse before cancel arrives.
    await eventually(() => sentToTabs.some((m) => m.type === "yoetz_extract_response"));

    port.emit(envelope("job_cancel", "job_cancel_a"));

    // Cancel must (1) click stop on the content side, (2) close the tab, (3)
    // post a job_cancel envelope.
    await eventually(() => port.messages.some((m) => m.type === "job_cancel" && m.job_id === "job_cancel_a"));
    assert.ok(
      sentToTabs.some((m) => m.type === "yoetz_cancel_send" && m.jobId === "job_cancel_a"),
      "expected service worker to forward yoetz_cancel_send to the content script"
    );
    assert.deepEqual(removedTabs, [createdTabId],
      "expected service worker to remove the ChatGPT tab on cancel");
    const cancelEnvelope = port.messages.find((m) => m.type === "job_cancel" && m.job_id === "job_cancel_a");
    assert.equal(cancelEnvelope.payload.cancelled, true);
    assert.equal(cancelEnvelope.payload.stop_clicked, true);

    // Subsequent extract_response for the cancelled job_id must surface
    // "unknown job" — the in-memory map can no longer carry a cancelled entry.
    port.messages.length = 0;
    port.emit(envelope("job_file_chunk", "job_cancel_a", {
      sequence: 0,
      total_chunks: 1,
      total_bytes: 4,
      filename: "should-not-accept.md",
      mime_type: "text/markdown",
      bytes_base64: uint8ArrayToBase64(new TextEncoder().encode("body"))
    }));
    await eventually(() => port.messages.some((m) => m.type === "job_error" && m.job_id === "job_cancel_a"));
    const followupError = port.messages.find((m) => m.type === "job_error" && m.job_id === "job_cancel_a");
    assert.match(String(followupError.payload.message ?? ""), /unknown job/);
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
            return { ok: true, payload: { status: "kept_current", model_used: "ChatGPT" } };
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
      model: "current",
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
  } finally {
    globalThis.chrome = originalChrome;
  }
});

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

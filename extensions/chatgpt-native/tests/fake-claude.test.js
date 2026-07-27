import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import {
  classifyBlockingState,
  clickSend,
  configureModelState,
  ensureConversationLoaded,
  ensureFreshChat,
  extractResponse,
  insertPrompt,
  isResponseGenerating,
  uploadFile,
  waitForSendAccepted
} from "../src/claude-dom.js";
import { claudeSiteAdapter } from "../src/sites/claude.js";

const claudeDomSource = await readFile(new URL("../src/claude-dom.js", import.meta.url), "utf8");

function fakeCreditBanner({
  excluded = false,
  visible = true,
  text = "Your org is out of usage credits for the month. We let your admin know. Switch models to continue chatting.",
  depth = 8,
  style = {},
  rect = null,
  ancestorRect = null,
  conversationDescendant = false,
  hiddenAncestor = false,
  switchControlQueryable = true,
  role = "alert",
  ariaLive = null,
  tagName = null,
  nestedMessage = false,
  renderedText = text,
  messageHidden = false,
  messageRect = rect,
  messageStyle = {},
  displayContentsAncestor = false,
  shellStyle = {}
} = {}) {
  const shell = {
    innerText: renderedText,
    textContent: text,
    tagName: "BODY",
    parentElement: null,
    getAttribute() {
      return null;
    },
    closest() {
      return excluded ? {} : null;
    },
    querySelector() {
      return conversationDescendant ? {} : null;
    },
    getClientRects() {
      return displayContentsAncestor ? [] : [{}];
    },
    getBoundingClientRect() {
      return ancestorRect;
    }
  };
  const banner = {
    attrs: { role, "aria-live": ariaLive },
    innerText: renderedText,
    textContent: text,
    tagName,
    parentElement: shell,
    getAttribute(name) {
      return this.attrs[name] ?? null;
    },
    getClientRects() {
      return visible ? [{}] : [];
    },
    getBoundingClientRect() {
      return messageRect;
    },
    closest() {
      return excluded ? {} : null;
    },
    querySelector() {
      return conversationDescendant ? {} : null;
    }
  };
  const message = nestedMessage || messageHidden ? {
    attrs: {},
    innerText: text,
    textContent: text,
    tagName: "P",
    parentElement: banner,
    getAttribute(name) {
      return this.attrs[name] ?? null;
    },
    getClientRects() {
      return messageHidden ? [] : [{}];
    },
    getBoundingClientRect() {
      return rect;
    },
    closest() {
      return excluded ? {} : null;
    },
    querySelector() {
      return null;
    }
  } : null;
  const switchModels = {
    attrs: { role: "button" },
    innerText: "Switch models",
    textContent: "Switch models",
    parentElement: null,
    clickCount: 0,
    click() {
      this.clickCount += 1;
    },
    closest() {
      return excluded ? {} : null;
    },
    getAttribute(name) {
      return this.attrs[name] ?? null;
    },
    getClientRects() {
      return visible ? [{}] : [];
    },
    getBoundingClientRect() {
      return rect;
    }
  };
  let parent = banner;
  for (let index = 0; index < depth; index += 1) {
    parent = {
      innerText: "Switch models",
      textContent: "Switch models",
      parentElement: parent,
      getAttribute() {
        return null;
      },
      getClientRects() {
        return visible ? [{}] : [];
      },
      getBoundingClientRect() {
        return rect;
      }
    };
  }
  switchModels.parentElement = parent;
  const root = {
    body: shell,
    documentElement: shell,
    defaultView: {
      innerWidth: 1200,
      innerHeight: 800,
      getComputedStyle: (element) => ({
        display: messageHidden && element === message
          ? "none"
          : (displayContentsAncestor && element === shell ? "contents" : (visible ? "block" : "none")),
        visibility: messageHidden && element === message ? "hidden" : (visible ? "visible" : "hidden"),
        opacity: hiddenAncestor && element === shell ? "0" : (visible ? "1" : "0"),
        ...(element === banner ? style : {}),
        ...(element === message ? messageStyle : {}),
        ...(element === shell ? shellStyle : {})
      })
    },
    querySelectorAll(selector) {
      if (selector === "body *") return [message, banner, switchModels].filter(Boolean);
      return switchControlQueryable && selector.includes("button") ? [switchModels] : [];
    }
  };
  return { root, shell, banner, message, switchModels };
}

test("classifyBlockingState detects the visible organization credit banner", () => {
  const banner = fakeCreditBanner();

  assert.deepEqual(classifyBlockingState(banner.root), {
    state: "usage_credits_exhausted",
    code: "usage_credits_exhausted",
    requested_model: "fable-5-max",
    provider_message: "Your org is out of usage credits for the month. We let your admin know. Switch models to continue chatting.",
    provider_dom: {
      container: {
        found: true,
        tag: null,
        role: "alert",
        testid: null,
        class_fragment: null
      },
      switch_models_control: {
        found: true,
        tag: null,
        role: "button",
        testid: null,
        class_fragment: null
      }
    },
    message: "Claude cannot run Fable 5 Max because this organization is out of monthly usage credits. Yoetz did not switch models."
  });
  assert.equal(banner.switchModels.clickCount, 0);
});

test("classifyBlockingState survives unknown Switch models markup and records diagnostics", () => {
  const banner = fakeCreditBanner({ switchControlQueryable: false });

  const blockingState = classifyBlockingState(banner.root);

  assert.equal(blockingState?.code, "usage_credits_exhausted");
  assert.deepEqual(blockingState?.provider_dom.switch_models_control, { found: false });
  assert.equal(banner.switchModels.clickCount, 0);
});

test("classifyBlockingState bounds credit text to a provider notice surface", () => {
  const sidebarTitle = fakeCreditBanner({
    role: null,
    tagName: "ASIDE",
    switchControlQueryable: false
  });
  const ordinaryPageText = fakeCreditBanner({
    role: null,
    text: "Your org is out of usage credits for the month.",
    switchControlQueryable: false
  });
  const alertWithoutControl = fakeCreditBanner({ switchControlQueryable: false });
  const nestedAlert = fakeCreditBanner({ nestedMessage: true, depth: 1 });
  const controlBackedNotice = fakeCreditBanner({ role: null, nestedMessage: true, depth: 1 });

  assert.equal(classifyBlockingState(sidebarTitle.root), null);
  assert.equal(classifyBlockingState(ordinaryPageText.root), null);
  assert.equal(classifyBlockingState(alertWithoutControl.root)?.code, "usage_credits_exhausted");
  assert.equal(classifyBlockingState(nestedAlert.root)?.provider_dom.switch_models_control.found, true);
  assert.equal(classifyBlockingState(controlBackedNotice.root)?.code, "usage_credits_exhausted");
});

test("classifyBlockingState matches rendered notice text and painted visibility", () => {
  const hiddenMatchingChild = fakeCreditBanner({
    renderedText: "A different visible notification.",
    messageHidden: true,
    switchControlQueryable: false
  });
  const zeroArea = fakeCreditBanner({
    rect: { left: 10, top: 10, right: 10, bottom: 10, width: 0, height: 0 }
  });
  const transparentMatchingChild = fakeCreditBanner({
    nestedMessage: true,
    messageStyle: { opacity: "0" },
    switchControlQueryable: false
  });
  const offscreenMatchingChild = fakeCreditBanner({
    nestedMessage: true,
    messageRect: { left: 0, top: 900, right: 500, bottom: 1000, width: 500, height: 100 },
    switchControlQueryable: false
  });
  const zeroAreaMatchingChild = fakeCreditBanner({
    nestedMessage: true,
    messageRect: { left: 10, top: 10, right: 10, bottom: 10, width: 0, height: 0 },
    switchControlQueryable: false
  });
  const visibleClipPath = fakeCreditBanner({ style: { clipPath: "inset(0)" } });
  const overflowAutoClipped = fakeCreditBanner({
    rect: { left: 200, top: 200, right: 300, bottom: 300, width: 100, height: 100 },
    ancestorRect: { left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100 },
    shellStyle: { overflow: "auto" }
  });
  const overflowScrollClipped = fakeCreditBanner({
    rect: { left: 200, top: 200, right: 300, bottom: 300, width: 100, height: 100 },
    ancestorRect: { left: 0, top: 0, right: 100, bottom: 100, width: 100, height: 100 },
    shellStyle: { overflow: "scroll" }
  });
  const displayContentsAncestor = fakeCreditBanner({ displayContentsAncestor: true });

  assert.equal(classifyBlockingState(hiddenMatchingChild.root), null);
  assert.equal(classifyBlockingState(zeroArea.root), null);
  assert.equal(classifyBlockingState(transparentMatchingChild.root), null);
  assert.equal(classifyBlockingState(offscreenMatchingChild.root), null);
  assert.equal(classifyBlockingState(zeroAreaMatchingChild.root), null);
  assert.equal(classifyBlockingState(visibleClipPath.root)?.code, "usage_credits_exhausted");
  assert.equal(classifyBlockingState(overflowAutoClipped.root), null);
  assert.equal(classifyBlockingState(overflowScrollClipped.root), null);
  assert.equal(classifyBlockingState(displayContentsAncestor.root)?.code, "usage_credits_exhausted");
});

test("classifyBlockingState accepts wording drift but rejects structural false positives", () => {
  const quoted = fakeCreditBanner({ excluded: true });
  const hidden = fakeCreditBanner({ visible: false });
  const transparent = fakeCreditBanner({ style: { opacity: "0" } });
  const offscreen = fakeCreditBanner({
    rect: { left: 0, top: 900, right: 500, bottom: 1000, width: 500, height: 100 }
  });
  const shortened = fakeCreditBanner({
    text: "Your org is out of usage credits for the month. Switch models to continue chatting."
  });
  const reordered = fakeCreditBanner({
    text: "Switch models to continue chatting. We let your admin know. Your org is out of usage credits for the month."
  });
  const extra = fakeCreditBanner({
    text: "Your org is out of usage credits for the month. We let your admin know. Prompt quoted this notice. Switch models to continue chatting."
  });
  const drifted = fakeCreditBanner({
    text: "You're out of usage credits. Switch models to continue."
  });
  const conversationCompleted = fakeCreditBanner({ conversationDescendant: true });
  const ancestorHidden = fakeCreditBanner({ hiddenAncestor: true });
  const unrelated = fakeCreditBanner({
    text: "Your organization has a billing notice. Switch models to continue chatting."
  });

  assert.equal(classifyBlockingState(quoted.root), null);
  assert.equal(classifyBlockingState(hidden.root), null);
  assert.equal(classifyBlockingState(transparent.root), null);
  assert.equal(classifyBlockingState(offscreen.root), null);
  assert.equal(classifyBlockingState(shortened.root)?.code, "usage_credits_exhausted");
  assert.equal(classifyBlockingState(reordered.root)?.code, "usage_credits_exhausted");
  assert.equal(classifyBlockingState(extra.root)?.code, "usage_credits_exhausted");
  assert.equal(classifyBlockingState(drifted.root)?.code, "usage_credits_exhausted");
  assert.equal(classifyBlockingState(conversationCompleted.root), null);
  assert.equal(classifyBlockingState(ancestorHidden.root), null);
  assert.equal(classifyBlockingState(unrelated.root), null);
});

test("classifyBlockingState distinguishes terminal semantic notices from warnings", () => {
  const terminal = fakeCreditBanner({
    text: "You're out of usage credits.",
    switchControlQueryable: false
  });
  const terminalAfterUnrelatedNegation = fakeCreditBanner({
    text: "We could not verify your billing. Your org is out of usage credits.",
    switchControlQueryable: false
  });
  const warnings = [
    "You're almost out of usage credits.",
    "You're nearly out of usage credits.",
    "Your org is not out of usage credits.",
    "You are no longer out of usage credits.",
    "Your org isn't out of usage credits.",
    "You aren't out of usage credits.",
    "You're about to be out of usage credits.",
    "You're running low, not out of usage credits."
  ].map((text) => fakeCreditBanner({ text, switchControlQueryable: false }));
  const controlCorroborated = fakeCreditBanner({
    role: null,
    nestedMessage: true,
    depth: 1,
    text: "You're almost out of usage credits. Switch models."
  });

  assert.equal(classifyBlockingState(terminal.root)?.code, "usage_credits_exhausted");
  assert.equal(classifyBlockingState(terminalAfterUnrelatedNegation.root)?.code, "usage_credits_exhausted");
  for (const warning of warnings) {
    assert.equal(classifyBlockingState(warning.root), null);
  }
  assert.equal(classifyBlockingState(controlCorroborated.root)?.code, "usage_credits_exhausted");
});

test("classifyBlockingState throttles repeated full-DOM scans for quoted credit text", () => {
  const quoted = fakeCreditBanner({ excluded: true });
  const querySelectorAll = quoted.root.querySelectorAll.bind(quoted.root);
  let bodyScans = 0;
  quoted.root.querySelectorAll = (selector) => {
    if (selector === "body *") bodyScans += 1;
    return querySelectorAll(selector);
  };

  assert.equal(classifyBlockingState(quoted.root), null);
  assert.equal(classifyBlockingState(quoted.root), null);
  assert.equal(bodyScans, 1);
  assert.equal(classifyBlockingState(quoted.root, { forceScan: true }), null);
  assert.equal(bodyScans, 2);
});

test("clickSend force-scans after a cached negative before clicking", async () => {
  const banner = fakeCreditBanner();
  const querySelectorAll = banner.root.querySelectorAll.bind(banner.root);
  let mounted = false;
  banner.root.querySelectorAll = (selector) => {
    if (selector === "body *") {
      return mounted ? [banner.banner, banner.switchModels] : [];
    }
    if (selector.includes("button")) {
      return mounted ? [banner.switchModels] : [];
    }
    return querySelectorAll(selector);
  };
  const send = {
    disabled: false,
    clickCount: 0,
    getAttribute() {
      return null;
    },
    click() {
      this.clickCount += 1;
    }
  };
  banner.root.querySelector = (selector) => (
    selector === "button[aria-label='Send message']" ? send : null
  );

  assert.equal(classifyBlockingState(banner.root), null);
  mounted = true;
  await assert.rejects(
    clickSend(banner.root, { timeoutMs: 1 }),
    (error) => error?.code === "usage_credits_exhausted"
      && error?.phase === "send"
      && error?.send_committed === false
  );
  assert.equal(send.clickCount, 0);
});

test("Claude waits reclassify a persistent credit banner instead of timing out", async () => {
  const composerWait = fakeCreditBanner();
  composerWait.root.querySelector = () => null;
  await assert.rejects(
    ensureFreshChat(composerWait.root, {}, { timeoutMs: 1 }),
    (error) => error?.code === "usage_credits_exhausted"
      && error?.phase === "upload"
      && error?.send_committed === false
  );

  const sendWait = fakeCreditBanner();
  sendWait.root.querySelector = () => null;
  await assert.rejects(
    clickSend(sendWait.root, { timeoutMs: 1 }),
    (error) => error?.code === "usage_credits_exhausted"
      && error?.phase === "send"
      && error?.send_committed === false
  );
  assert.equal(sendWait.switchModels.clickCount, 0);
});

test("waitForSendAccepted fails with typed credit state without switching models", async () => {
  const banner = fakeCreditBanner();
  banner.root.querySelectorAll = (selector) => {
    if (selector === "body *") return [banner.banner, banner.switchModels];
    if (selector.includes("button") || selector.includes("[role=")) return [banner.switchModels];
    if (selector === "[data-testid='user-message']") return [];
    if (selector === "[data-is-streaming]") return [];
    return [];
  };

  await assert.rejects(
    waitForSendAccepted(banner.root, { user_count: 0, assistant_count: 0 }, {
      timeoutMs: 20,
      intervalMs: 1
    }),
    (error) => {
      assert.equal(error.code, "usage_credits_exhausted");
      assert.equal(error.state, "usage_credits_exhausted");
      assert.equal(error.requested_model, "fable-5-max");
      assert.equal(error.send_committed, true);
      return true;
    }
  );
  assert.equal(banner.switchModels.clickCount, 0);
});

test("Claude upload uses the native files setter for page-world visibility", () => {
  assert.doesNotMatch(
    claudeDomSource,
    /Object\.defineProperty\(\s*input\s*,\s*["']files["']/
  );
  assert.match(claudeDomSource, /\binput\.files\s*=\s*transfer\.files\b/);
});

test("fake Claude background turn returns the complete scoped response only after final controls", () => {
  const terminalMarker = "YOETZ TERMINAL MARKER";
  const text = `${"A complete long response. ".repeat(300)}${terminalMarker}`;
  const page = fakeClaudePage({ text, streaming: false, copy: true });

  const extraction = extractResponse(page.root);

  assert.equal(extraction.method, "assistant_dom");
  assert.equal(extraction.text.endsWith(terminalMarker), true);
  assert.equal(extraction.is_generating, false);
  assert.equal(extraction.has_copy_button, true);
  assert.equal(claudeSiteAdapter.completion.hasFinalAssistantAffordance(extraction), true);
});

test("fake Claude ProseMirror composer inserts through execCommand", async () => {
  const commands = [];
  const events = [];
  const composer = {
    isContentEditable: true,
    textContent: "",
    getAttribute: (name) => name === "contenteditable" ? "true" : null,
    focus() {
      root.activeElement = this;
    },
    dispatchEvent(event) {
      events.push(event.type);
      return true;
    }
  };
  const root = {
    activeElement: null,
    querySelector: (selector) => selector === "[data-testid='chat-input']" ? composer : null,
    execCommand(command, _showUi, value) {
      commands.push({ command, value });
      assert.equal(root.activeElement, composer);
      composer.textContent = value;
      return true;
    }
  };

  await insertPrompt(root, "Review this bundle");

  assert.deepEqual(commands, [{ command: "insertText", value: "Review this bundle" }]);
  assert.equal(composer.textContent, "Review this bundle");
  assert.deepEqual(events, ["change"]);
});

test("fake Claude upload waits for the committed chip and re-enabled send", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    let attachment = null;
    let committedAt = null;
    const send = {
      disabled: true,
      getAttribute: () => null
    };
    const input = {
      files: null,
      dispatchEvent(event) {
        assert.equal(event.type, "change");
        const name = this.files[0].name;
        attachment = {
          querySelector(selector) {
            if (selector === "h3") return { textContent: name };
            if (selector === "button[aria-label='Remove']") return {};
            return null;
          }
        };
        setTimeout(() => {
          send.disabled = false;
          committedAt = Date.now();
        }, 25);
        return true;
      }
    };
    const root = {
      querySelector(selector) {
        if (selector === "input[data-testid='file-upload']") return input;
        if (selector === "button[aria-label='Send message']") return send;
        return null;
      },
      querySelectorAll(selector) {
        return selector === "[data-testid='file-thumbnail']" && attachment ? [attachment] : [];
      }
    };
    const file = new File(["bundle"], "fixture.md", { type: "text/markdown" });

    await uploadFile(root, file, { timeoutMs: 1000 });

    assert.equal(input.files[0].name, "fixture.md");
    assert.equal(send.disabled, false);
    assert.ok(committedAt !== null && Date.now() >= committedAt);
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("fake Claude upload accepts a later duplicate thumbnail that has the remove control", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const input = {
      files: null,
      dispatchEvent() {
        return true;
      }
    };
    const thumbnail = (hasRemoveControl) => ({
      querySelector(selector) {
        if (selector === "h3") return { textContent: "fixture.md" };
        if (selector === "button[aria-label='Remove']") return hasRemoveControl ? {} : null;
        return null;
      }
    });
    const root = {
      querySelector(selector) {
        if (selector === "input[data-testid='file-upload']") return input;
        if (selector === "button[aria-label='Send message']") {
          return { disabled: false, getAttribute: () => null };
        }
        return null;
      },
      querySelectorAll(selector) {
        return selector === "[data-testid='file-thumbnail']"
          ? [thumbnail(false), thumbnail(true)]
          : [];
      }
    };

    await uploadFile(root, new File(["bundle"], "fixture.md", { type: "text/markdown" }), {
      timeoutMs: 50
    });
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("fake Claude upload keeps observing after its soft deadline without redispatching", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    let attachment = null;
    let dispatches = 0;
    const send = {
      disabled: true,
      getAttribute: () => null
    };
    const input = {
      files: null,
      dispatchEvent(event) {
        assert.equal(event.type, "change");
        dispatches += 1;
        const name = this.files[0].name;
        setTimeout(() => {
          attachment = {
            querySelector(selector) {
              if (selector === "h3") return { textContent: name };
              if (selector === "button[aria-label='Remove']") return {};
              return null;
            }
          };
          send.disabled = false;
        }, 150);
        return true;
      }
    };
    const root = {
      querySelector(selector) {
        if (selector === "input[data-testid='file-upload']") return input;
        if (selector === "button[aria-label='Send message']") return send;
        return null;
      },
      querySelectorAll(selector) {
        return selector === "[data-testid='file-thumbnail']" && attachment ? [attachment] : [];
      }
    };

    await uploadFile(root, new File(["bundle"], "fixture.md", { type: "text/markdown" }), {
      timeoutMs: 50,
      stallTimeoutMs: 300
    });

    assert.equal(dispatches, 1);
    assert.equal(send.disabled, false);
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("fake Claude upload reports a bounded attachment trace when it stalls", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    let dispatches = 0;
    const input = {
      files: null,
      dispatchEvent() {
        dispatches += 1;
        return true;
      }
    };
    const root = {
      querySelector(selector) {
        if (selector === "input[data-testid='file-upload']") return input;
        if (selector === "button[aria-label='Send message']") {
          return { disabled: true, getAttribute: () => "true" };
        }
        return null;
      },
      querySelectorAll(selector) {
        return selector === "input[data-testid='file-upload']" ? [input] : [];
      }
    };
    const finalChunkAckAtMs = Date.now();

    await assert.rejects(
      () => uploadFile(root, new File(["bundle"], "fixture.md", { type: "text/markdown" }), {
        timeoutMs: 50,
        stallTimeoutMs: 150,
        initialAttachmentTrace: { final_chunk_ack_at_ms: finalChunkAckAtMs }
      }),
      (error) => {
        assert.equal(error.code, "attachment_stalled");
        assert.equal(error.phase, "upload");
        assert.equal(error.side_effect_started, true);
        assert.match(error.message, /attachment stalled/);
        assert.equal(error.attachment_trace.final_chunk_ack_at_ms, finalChunkAckAtMs);
        assert.ok(Number.isFinite(error.attachment_trace.input_resolved_at_ms));
        assert.ok(Number.isFinite(error.attachment_trace.files_assigned_at_ms));
        assert.ok(Number.isFinite(error.attachment_trace.change_dispatched_at_ms));
        assert.ok(Number.isFinite(error.attachment_trace.soft_timeout_at_ms));
        assert.ok(Number.isFinite(error.attachment_trace.hard_timeout_at_ms));
        assert.deepEqual(error.attachment_trace.hard_timeout_pending_legs, [
          "matching_thumbnail",
          "remove_control",
          "send_enabled"
        ]);
        assert.equal(JSON.stringify(error.attachment_trace).includes("fixture.md"), false);
        return true;
      }
    );

    assert.equal(dispatches, 1);
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("fake Claude upload timeout reports each attachment readiness leg", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const input = {
      files: null,
      dispatchEvent() {
        return true;
      }
    };
    const attachment = {
      textContent: "fixture.md Processing",
      getAttribute(name) {
        if (name === "aria-busy") return "true";
        return null;
      },
      querySelector(selector) {
        if (selector === "h3") return { textContent: "fixture.md" };
        if (selector === "[role='progressbar']") return {};
        return null;
      },
      querySelectorAll() {
        return [];
      }
    };
    const root = {
      querySelector(selector) {
        if (selector === "input[data-testid='file-upload']") return input;
        if (selector === "button[aria-label='Send message']") {
          return { disabled: true, getAttribute: () => "true" };
        }
        return null;
      },
      querySelectorAll(selector) {
        if (selector === "input[data-testid='file-upload']") return [input];
        if (selector === "[data-testid='file-thumbnail']") return [attachment];
        return [];
      }
    };

    await assert.rejects(
      () => uploadFile(root, new File(["bundle"], "fixture.md", { type: "text/markdown" }), {
        timeoutMs: 1
      }),
      (error) => {
        assert.match(error.message, /file_input_count=1/);
        assert.match(error.message, /thumbnail_count=1/);
        assert.match(error.message, /thumbnail_labels=\["fixture\.md"\]/);
        assert.match(error.message, /filename_match=true/);
        assert.match(error.message, /remove_present=false/);
        assert.match(error.message, /attachment_busy=\["fixture\.md"\]/);
        assert.match(error.message, /attachment_failures=\[\]/);
        assert.match(error.message, /send_present=true/);
        assert.match(error.message, /send_disabled=true/);
        assert.match(error.message, /send_aria_disabled="true"/);
        assert.match(error.message, /timeout_stage="attachment_readiness"/);
        return true;
      }
    );
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("fake Claude upload timeout reports an absent send control", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const input = {
      files: null,
      dispatchEvent() {
        return true;
      }
    };
    const attachment = {
      textContent: "fixture.md",
      getAttribute: () => null,
      querySelector(selector) {
        if (selector === "h3") return { textContent: "fixture.md" };
        if (selector === "button[aria-label='Remove']") return {};
        return null;
      }
    };
    const root = {
      querySelector(selector) {
        return selector === "input[data-testid='file-upload']" ? input : null;
      },
      querySelectorAll(selector) {
        if (selector === "input[data-testid='file-upload']") return [input];
        if (selector === "[data-testid='file-thumbnail']") return [attachment];
        return [];
      }
    };

    await assert.rejects(
      () => uploadFile(root, new File(["bundle"], "fixture.md", { type: "text/markdown" }), {
        timeoutMs: 1
      }),
      (error) => {
        assert.match(error.message, /filename_match=true/);
        assert.match(error.message, /remove_present=true/);
        assert.match(error.message, /attachment_busy=\[\]/);
        assert.match(error.message, /attachment_failures=\[\]/);
        assert.match(error.message, /send_present=false/);
        assert.match(error.message, /send_disabled=null/);
        assert.match(error.message, /send_aria_disabled=null/);
        assert.match(error.message, /timeout_stage="attachment_readiness"/);
        return true;
      }
    );
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("fake Claude upload timeout identifies the file-input wait", async () => {
  const root = {
    querySelector: () => null,
    querySelectorAll: () => []
  };

  await assert.rejects(
    () => uploadFile(root, new File(["bundle"], "fixture.md", { type: "text/markdown" }), {
      timeoutMs: 1
    }),
    (error) => {
      assert.match(error.message, /file_input_count=0/);
      assert.match(error.message, /timeout_stage="file_input"/);
      return true;
    }
  );
});

test("fake Claude model picker drives hover-only Max then closes", async () => {
  const fixture = makeClaudeModelFixture();
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const result = await configureModelState(fixture.root, { model_selection_timeout_ms: 250 });

    assert.equal(result.status, "selected");
    assert.equal(result.model_used, "Fable 5 Max");
    assert.equal(result.modelVerified, true);
    assert.equal(result.maxVerified, true);
    assert.ok(fixture.hoverEvents >= 3, "effort submenu must be re-hovered for Max and verification");
    assert.equal(fixture.sawMousePointer, true);
    assert.equal(fixture.modelButton.getAttribute("aria-expanded"), "false");
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude model picker keeps shared menu visibility semantics for offscreen Fable", async () => {
  const fixture = makeClaudeModelFixture({ offscreenFable: true });
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const result = await configureModelState(fixture.root, { model_selection_timeout_ms: 250 });
    assert.equal(result.status, "selected");
    assert.equal(fixture.fableClicks, 1);
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude model picker reclassifies credits immediately after Fable selection", async () => {
  const fixture = makeClaudeModelFixture();
  const credits = fakeCreditBanner();
  const originalQuerySelectorAll = fixture.root.querySelectorAll.bind(fixture.root);
  fixture.root.defaultView = {
    ...fixture.root.defaultView,
    ...credits.root.defaultView
  };
  fixture.root.querySelectorAll = (selector) => (
    fixture.modelButton.innerText === "Fable 5 High" && selector === "body *"
      ? [credits.banner, credits.switchModels]
      : fixture.modelButton.innerText === "Fable 5 High" && selector.includes("button")
        ? [credits.switchModels]
        : originalQuerySelectorAll(selector)
  );
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    await assert.rejects(
      configureModelState(fixture.root, { model_selection_timeout_ms: 250 }),
      (error) => error?.code === "usage_credits_exhausted"
        && error?.phase === "model_selection"
        && error?.send_committed === false
    );
    assert.equal(fixture.fableClicks, 1);
    assert.equal(fixture.maxClicks, 0);
    assert.equal(credits.switchModels.clickCount, 0);
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude model picker accepts Fable 5 Max without a Thinking control", async () => {
  const fixture = makeClaudeModelFixture();
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const result = await configureModelState(fixture.root, { model_selection_timeout_ms: 25 });

    assert.equal(result.status, "selected");
    assert.equal(result.modelVerified, true);
    assert.equal(result.maxVerified, true);
    assert.equal(result.model_used, "Fable 5 Max");
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude model picker waits for a delayed menu close before reopening", async () => {
  const fixture = makeClaudeModelFixture({ delayedSelectionClose: true });
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const result = await configureModelState(fixture.root, { model_selection_timeout_ms: 250 });

    assert.equal(result.status, "selected");
    assert.equal(result.model_used, "Fable 5 Max");
    assert.equal(fixture.modelButton.getAttribute("aria-expanded"), "false");
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude model picker preserves an already exact selection without clicking options", async () => {
  const fixture = makeClaudeModelFixture({ initiallyConfigured: true });
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const result = await configureModelState(fixture.root, { model_selection_timeout_ms: 250 });

    assert.equal(result.status, "selected");
    assert.equal(result.model_used, "Fable 5 Max");
    assert.equal(fixture.fableClicks, 0);
    assert.equal(fixture.maxClicks, 0);
    assert.equal(fixture.modelButton.getAttribute("aria-expanded"), "false");
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude full and already-exact paths both return adapter-acceptable results", async () => {
  const fullSelection = makeClaudeModelFixture();
  const alreadyExact = makeClaudeModelFixture({ initiallyConfigured: true, ignoreEscape: true });
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const fullResult = await configureModelState(fullSelection.root, { model_selection_timeout_ms: 250 });
    const exactResult = await configureModelState(alreadyExact.root, { model_selection_timeout_ms: 250 });

    assert.equal(claudeSiteAdapter.isAcceptableModelSelection(fullResult), true);
    assert.equal(claudeSiteAdapter.isAcceptableModelSelection(exactResult), true);
    assert.deepEqual(Object.keys(exactResult).sort(), Object.keys(fullResult).sort());
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude missing Fable returns unavailable with live options", async () => {
  const fixture = makeClaudeModelFixture({ includeFable: false });
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const result = await configureModelState(fixture.root, { model_selection_timeout_ms: 1 });

    assert.equal(result.status, "unavailable");
    assert.ok(result.options.includes("Sonnet 5"));
    assert.match(result.warning, /live options: Sonnet 5/);
    assert.equal(fixture.modelButton.getAttribute("aria-expanded"), "false");
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude model picker refuses to proceed without a verifiable Max option", async () => {
  const fixture = makeClaudeModelFixture({ includeMax: false });
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    await assert.rejects(
      configureModelState(fixture.root, { model_selection_timeout_ms: 1 }),
      /did not reach the requested state/
    );
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude Thinking row keeps the response non-final until tool use and streaming stop", () => {
  const page = fakeClaudePage({
    text: "Searching the attached marker file",
    streaming: true,
    copy: true,
    thinking: true
  });

  const interim = extractResponse(page.root);
  assert.equal(interim.is_generating, true);
  assert.equal(claudeSiteAdapter.completion.hasFinalAssistantAffordance(interim), false);

  page.assistant.streaming = false;
  page.assistant.thinking = false;
  page.body.textContent = "Found marker FILE-42. Final answer complete.";
  page.body.innerText = page.body.textContent;
  const complete = extractResponse(page.root);
  assert.equal(complete.is_generating, false);
  assert.equal(complete.text, "Found marker FILE-42. Final answer complete.");
  assert.equal(claudeSiteAdapter.completion.hasFinalAssistantAffordance(complete), true);
});

test("fake Claude extraction excludes collapsed thinking status text from the answer", () => {
  const answer = "Final answer without thinking chrome.";
  const contaminated = [
    "Thinking about concerns with this request",
    "Thinking about concerns with this request",
    "⌄",
    answer
  ].join("\n");
  const page = fakeClaudePage({ text: contaminated, streaming: false, copy: true });
  page.body.cloneNode = () => {
    let text = contaminated;
    return {
      get innerText() {
        return text;
      },
      get textContent() {
        return text;
      },
      querySelectorAll(selector) {
        return selector === "button[class*='group/status']"
          ? [{ remove: () => { text = answer; } }]
          : [];
      }
    };
  };

  const extraction = extractResponse(page.root);

  assert.equal(extraction.text, answer);
});

test("fake Claude extraction excludes a duplicated collapsed thinking subtree", () => {
  const caption = "Scrutinizing verification request authenticity and token protocol";
  const answer = "YOETZ-EXT-VERIFY-20260719-QOPHET";
  const contaminated = `${caption}${answer}`;
  const page = fakeClaudePage({ text: contaminated, streaming: false, copy: true });
  page.body.cloneNode = () => fakeClaudeThinkingClone({
    statusCaption: caption,
    hiddenCaption: caption,
    answer
  });

  const extraction = extractResponse(page.root);

  assert.equal(extraction.text, answer);
});

test("fake Claude extraction excludes a thinking subtree when its hidden caption differs by punctuation", () => {
  const statusCaption = "Thinking about acknowledging a direct instruction request.";
  const hiddenCaption = "Thinking about acknowledging a direct instruction request";
  const answer = "ACK";
  const contaminated = `${hiddenCaption}${answer}`;
  const page = fakeClaudePage({ text: contaminated, streaming: false, copy: true });
  page.body.cloneNode = () => fakeClaudeThinkingClone({
    statusCaption,
    hiddenCaption,
    answer
  });

  const extraction = extractResponse(page.root);

  assert.equal(extraction.text, answer);
});

test("fake Claude extraction excludes a thinking subtree with a duration badge", () => {
  const statusCaption = "Thinking about acknowledging a direct instruction request 12s";
  const hiddenCaption = "Thinking about acknowledging a direct instruction request";
  const answer = "ACK";
  const contaminated = `${hiddenCaption}${answer}`;
  const page = fakeClaudePage({ text: contaminated, streaming: false, copy: true });
  page.body.cloneNode = () => fakeClaudeThinkingClone({
    statusCaption,
    hiddenCaption,
    answer
  });

  const extraction = extractResponse(page.root);

  assert.equal(extraction.text, answer);
});

test("fake Claude unmatched thinking layout removes only the status button and preserves answer text", () => {
  const caption = "Thinking layout changed";
  const answer = "ACK";
  const page = fakeClaudePage({ text: `${caption}${answer}`, streaming: false, copy: true });
  page.body.cloneNode = () => {
    let text = `${caption}\n${answer}`;
    const unsafeParent = {
      getAttribute() {
        return "";
      },
      remove() {
        text = "";
      }
    };
    const statusRow = {
      parentElement: unsafeParent,
      getAttribute() {
        return "group/status";
      },
      remove() {
        text = answer;
      }
    };
    return {
      get innerText() {
        return text;
      },
      get textContent() {
        return text;
      },
      querySelectorAll(selector) {
        return selector === "button[class*='group/status']" ? [statusRow] : [];
      }
    };
  };

  const extraction = extractResponse(page.root);

  assert.equal(extraction.text, answer);
});

test("fake Claude extraction fails closed when final controls are not scoped to the answer", () => {
  const page = fakeClaudePage({ text: "", streaming: false, copy: false });
  page.root.body.innerText = "possibly clipped page text";
  page.root.body.textContent = page.root.body.innerText;
  page.globalCopy = [{}];

  const extraction = extractResponse(page.root);

  assert.equal(extraction.method, "none");
  assert.equal(extraction.text, "");
  assert.equal(extraction.copy_button_count, 1);
  assert.equal(extraction.has_copy_button, false);
  assert.equal(claudeSiteAdapter.completion.hasFinalAssistantAffordance(extraction), false);
});

test("fake Claude completed scoped answer does not require a hover-only copy button", () => {
  const page = fakeClaudePage({ text: "complete without hover", streaming: false, copy: false });

  const extraction = extractResponse(page.root);

  assert.equal(extraction.method, "assistant_dom");
  assert.equal(extraction.has_copy_button, false);
  assert.equal(claudeSiteAdapter.completion.hasFinalAssistantAffordance(extraction), true);
  assert.equal(claudeSiteAdapter.completion.finalAffordanceRequiresStableIdle, true);
});

test("fake Claude extraction reports artifact cards from only the final assistant root", () => {
  const finalArtifact = fakeArtifactCard("Release plan / Document · MD / Download");
  const finalPage = fakeClaudePage({
    text: "Chat summary",
    streaming: false,
    copy: true,
    artifacts: [finalArtifact]
  });

  assert.deepEqual(extractResponse(finalPage.root).artifact_blocks, {
    count: 1,
    titles: ["Release plan / Document · MD / Download"]
  });

  const resumedPage = fakeClaudePage({
    text: "Current answer",
    streaming: false,
    copy: true
  });
  resumedPage.root.querySelectorAll = (selector) => {
    if (selector === "[data-is-streaming]") {
      return [
        fakeClaudeAssistant({ text: "Earlier answer", artifacts: [finalArtifact] }),
        resumedPage.assistant
      ];
    }
    if (selector === "[data-testid='action-bar-copy']") return resumedPage.globalCopy;
    if (selector === "button[aria-label='Stop response']") return [];
    if (selector === "button[class*='group/status']") return [];
    if (selector === "[data-testid='user-message']") return [];
    return [];
  };

  assert.deepEqual(extractResponse(resumedPage.root).artifact_blocks, {
    count: 0,
    titles: []
  });
});

test("fake Claude artifact detection survives an open side panel and stays silent without a card", () => {
  const page = fakeClaudePage({
    text: "Chat summary",
    streaming: false,
    copy: true,
    artifacts: [fakeArtifactCard("Open deliverable")]
  });
  page.root.body.innerText = "Chat summary\nOpen side panel body";
  page.root.body.textContent = page.root.body.innerText;

  assert.deepEqual(extractResponse(page.root).artifact_blocks, {
    count: 1,
    titles: ["Open deliverable"]
  });

  const noArtifact = fakeClaudePage({
    text: "Plain answer",
    streaming: false,
    copy: true
  });
  assert.deepEqual(extractResponse(noArtifact.root).artifact_blocks, {
    count: 0,
    titles: []
  });
});

test("fake Claude conversation loader preserves changed and unavailable taxonomy", async () => {
  const originalLocation = globalThis.location;
  const requested = "123e4567-e89b-12d3-a456-426614174000";
  const different = "123e4567-e89b-12d3-a456-426614174001";
  try {
    globalThis.location = {
      pathname: `/chat/${different}`,
      href: `https://claude.ai/chat/${different}`
    };
    await assert.rejects(
      ensureConversationLoaded(fakeConversationRoot(""), requested, { timeoutMs: 1 }),
      (error) => error?.code === "conversation_changed"
        && error?.requested_conversation_id === requested
        && error?.current_conversation_id === different
    );

    globalThis.location = {
      pathname: `/chat/${requested}`,
      href: `https://claude.ai/chat/${requested}`
    };
    await assert.rejects(
      ensureConversationLoaded(
        fakeConversationRoot("This conversation is unavailable. You do not have access."),
        requested,
        { timeoutMs: 1 }
      ),
      (error) => error?.code === "conversation_unavailable"
        && error?.requested_conversation_id === requested
    );
  } finally {
    globalThis.location = originalLocation;
  }
});

test("fake Claude model acceptance requires Fable 5 and Max together", () => {
  const selected = {
    status: "selected",
    requested_model: "fable-5-max",
    model_used: "Fable 5 Max",
    modelVerified: true,
    maxVerified: true
  };
  assert.equal(claudeSiteAdapter.isAcceptableModelSelection(selected), true);
  for (const field of ["modelVerified", "maxVerified"]) {
    assert.equal(
      claudeSiteAdapter.isAcceptableModelSelection({ ...selected, [field]: false }),
      false,
      field
    );
  }
});

function fakeClaudePage({ text, streaming, copy, thinking = false, artifacts = [] }) {
  const body = { innerText: text, textContent: text };
  const copyButton = {};
  const page = {
    globalCopy: copy ? [copyButton] : []
  };
  const turn = {
    querySelectorAll(selector) {
      return selector === "[data-testid='action-bar-copy']" && copy ? [copyButton] : [];
    }
  };
  const assistant = fakeClaudeAssistant({ text, streaming, thinking, artifacts, body, turn });
  const user = {
    compareDocumentPosition() {
      return 4;
    }
  };
  const root = {
    body: { innerText: text, textContent: text },
    querySelector(selector) {
      if (selector === "button[aria-label='Stop response']") return null;
      return null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-is-streaming]") return [assistant];
      if (selector === "[data-testid='action-bar-copy']") return page.globalCopy;
      if (selector === "button[aria-label='Stop response']") return [];
      if (selector === "button[class*='group/status']") return thinking ? [{}] : [];
      if (selector === "[data-testid='user-message']") return [user];
      return [];
    }
  };
  page.root = root;
  page.assistant = assistant;
  page.body = body;
  return page;
}

function fakeClaudeThinkingClone({ statusCaption, hiddenCaption, answer }) {
  let text = `${statusCaption}\n${hiddenCaption}\n${answer}`;
  const contaminated = `${hiddenCaption}${answer}`;
  const statusSubtree = {
    getAttribute(name) {
      return name === "class" ? "row-start-1 col-start-1 min-w-0" : null;
    },
    remove() {
      text = answer;
    }
  };
  const statusControl = {
    parentElement: statusSubtree,
    getAttribute() {
      return "";
    },
    remove() {
      text = contaminated;
    }
  };
  const statusRow = {
    parentElement: statusControl,
    innerText: statusCaption,
    textContent: statusCaption,
    getAttribute() {
      return "group/status";
    },
    remove() {
      text = contaminated;
    }
  };
  return {
    get innerText() {
      return text;
    },
    get textContent() {
      return text;
    },
    querySelectorAll(selector) {
      return selector === "button[class*='group/status']" ? [statusRow] : [];
    }
  };
}

function fakeClaudeAssistant({
  text,
  streaming = false,
  thinking = false,
  artifacts = [],
  body = { innerText: text, textContent: text },
  turn = { querySelectorAll: () => [] }
}) {
  return {
    streaming,
    thinking,
    getAttribute(name) {
      return name === "data-is-streaming" ? String(this.streaming) : null;
    },
    querySelector(selector) {
      if (selector === ".font-claude-response") return body;
      if (selector === "button[class*='group/status']") return this.thinking ? {} : null;
      return null;
    },
    querySelectorAll(selector) {
      return selector === "[class*='group/artifact-block']" ? artifacts : [];
    },
    closest() {
      return turn;
    }
  };
}

function fakeArtifactCard(title) {
  return { innerText: title, textContent: title };
}

class FakeDataTransfer {
  constructor() {
    this.files = [];
    this.items = { add: (file) => this.files.push(file) };
  }
}

class FakePointerEvent extends Event {
  constructor(type, init = {}) {
    super(type, init);
    this.pointerType = init.pointerType;
    this.pointerId = init.pointerId;
    this.clientX = init.clientX;
    this.clientY = init.clientY;
  }
}

class FakeMouseEvent extends Event {
  constructor(type, init = {}) {
    super(type, init);
    this.clientX = init.clientX;
    this.clientY = init.clientY;
  }
}

class FakeKeyboardEvent extends Event {
  constructor(type, init = {}) {
    super(type, init);
    this.key = init.key;
    this.code = init.code;
  }
}

function makeClaudeModelFixture({
  includeFable = true,
  includeMax = true,
  delayedSelectionClose = false,
  ignoreEscape = false,
  initiallyConfigured = false,
  offscreenFable = false
} = {}) {
  let menuOpen = false;
  let effortHovered = false;
  let hoverEvents = 0;
  let sawMousePointer = false;
  let fableClicks = 0;
  let maxClicks = 0;

  const control = (attrs, text, onClick = () => {}) => ({
    attrs: { ...attrs },
    innerText: text,
    textContent: text,
    parentElement: null,
    getAttribute(name) {
      return this.attrs[name] ?? null;
    },
    setAttribute(name, value) {
      this.attrs[name] = String(value);
    },
    getClientRects() {
      return [{}];
    },
    getBoundingClientRect() {
      return { left: 10, top: 20, width: 40, height: 20 };
    },
    closest() {
      return null;
    },
    click() {
      onClick(this);
    },
    dispatchEvent(event) {
      if (event.type.startsWith("pointer")) {
        hoverEvents += 1;
        if (event.pointerType === "mouse" && event.pointerId === 1 && event.clientX > 0 && event.clientY > 0) {
          sawMousePointer = true;
          effortHovered = true;
        }
      }
      if (event.type === "keydown" && event.key === "Escape" && !ignoreEscape) {
        menuOpen = false;
        modelButton.setAttribute("aria-expanded", "false");
      }
      return true;
    }
  });

  const modelButton = control(
    { "data-testid": "model-selector-dropdown", "aria-expanded": "false" },
    initiallyConfigured ? "Fable 5 Max" : "Sonnet 5 High",
    () => {
      if (modelButton.getAttribute("aria-expanded") === "true") {
        menuOpen = false;
        effortHovered = false;
        modelButton.setAttribute("aria-expanded", "false");
        return;
      }
      menuOpen = true;
      effortHovered = false;
      modelButton.setAttribute("aria-expanded", "true");
    }
  );
  const closeAfterSelection = () => {
    menuOpen = false;
    const markClosed = () => modelButton.setAttribute("aria-expanded", "false");
    if (delayedSelectionClose) {
      setTimeout(markClosed, 10);
    } else {
      markClosed();
    }
  };
  const fable = control({ role: "menuitemradio", "aria-checked": initiallyConfigured ? "true" : "false" }, "Fable 5", (element) => {
    fableClicks += 1;
    element.setAttribute("aria-checked", "true");
    modelButton.innerText = "Fable 5 High";
    modelButton.textContent = modelButton.innerText;
    closeAfterSelection();
  });
  if (offscreenFable) {
    fable.getBoundingClientRect = () => ({
      left: 10,
      top: 900,
      right: 50,
      bottom: 920,
      width: 40,
      height: 20
    });
  }
  const sonnet = control({ role: "menuitemradio", "aria-checked": includeFable ? "false" : "true" }, "Sonnet 5");
  const effort = control({ "data-testid": "effort-menu-trigger" }, "Effort");
  const max = control({ role: "menuitemradio", "data-testid": "effort-option-max", "aria-checked": initiallyConfigured ? "true" : "false" }, "Max", (element) => {
    maxClicks += 1;
    element.setAttribute("aria-checked", "true");
    modelButton.innerText = "Fable 5 Max";
    modelButton.textContent = modelButton.innerText;
    effortHovered = false;
    closeAfterSelection();
  });
  const root = {
    activeElement: modelButton,
    defaultView: { KeyboardEvent: FakeKeyboardEvent },
    querySelector(selector) {
      if (selector === "[data-testid='model-selector-dropdown']") return modelButton;
      if (selector === "[data-testid='effort-menu-trigger']") return menuOpen ? effort : null;
      if (selector === "[role='menuitemradio'][data-testid='effort-option-max']") {
        return includeMax && menuOpen && effortHovered ? max : null;
      }
      return null;
    },
    querySelectorAll(selector) {
      if (!menuOpen) return [];
      if (selector === "[role='menuitemradio']") {
        return includeFable ? [fable, sonnet] : [sonnet];
      }
      if (selector === "[role='menuitemradio'][aria-checked='true']") {
        return [fable, sonnet, includeMax ? max : null]
          .filter((element) => element?.getAttribute("aria-checked") === "true");
      }
      if (selector === "[role='menuitem'], [role='menuitemradio'], button, [role='switch']") {
        return [
          includeFable ? fable : null,
          sonnet,
          effort,
          includeMax && effortHovered ? max : null
        ].filter(Boolean);
      }
      return [];
    }
  };

  Object.defineProperties(root, {
    hoverEvents: { get: () => hoverEvents },
    sawMousePointer: { get: () => sawMousePointer }
  });
  return {
    root,
    modelButton,
    get fableClicks() { return fableClicks; },
    get maxClicks() { return maxClicks; },
    get hoverEvents() { return hoverEvents; },
    get sawMousePointer() { return sawMousePointer; }
  };
}

function fakeConversationRoot(text) {
  return {
    title: "Claude",
    body: { innerText: text, textContent: text },
    querySelector() {
      return null;
    }
  };
}

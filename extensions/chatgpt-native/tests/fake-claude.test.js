import assert from "node:assert/strict";
import test from "node:test";
import {
  configureModelState,
  ensureConversationLoaded,
  extractResponse,
  insertPrompt,
  isResponseGenerating,
  uploadFile
} from "../src/claude-dom.js";
import { claudeSiteAdapter } from "../src/sites/claude.js";

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

test("fake Claude model picker drives hover-only Max and Thinking then closes", async () => {
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
    assert.equal(result.thinkingChecked, true);
    assert.equal(fixture.thinking.getAttribute("aria-checked"), "true");
    assert.ok(fixture.hoverEvents >= 3, "effort submenu must be re-hovered for Max, Thinking, and verification");
    assert.equal(fixture.sawMousePointer, true);
    assert.equal(fixture.modelButton.getAttribute("aria-expanded"), "false");
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

test("fake Claude model picker waits for Thinking to render before verification", async () => {
  const fixture = makeClaudeModelFixture({ delayedThinkingRender: true });
  const previousPointerEvent = globalThis.PointerEvent;
  const previousMouseEvent = globalThis.MouseEvent;
  const previousKeyboardEvent = globalThis.KeyboardEvent;
  globalThis.PointerEvent = FakePointerEvent;
  globalThis.MouseEvent = FakeMouseEvent;
  globalThis.KeyboardEvent = FakeKeyboardEvent;
  try {
    const result = await configureModelState(fixture.root, { model_selection_timeout_ms: 250 });

    assert.equal(result.status, "selected");
    assert.equal(result.thinkingChecked, true);
  } finally {
    globalThis.PointerEvent = previousPointerEvent;
    globalThis.MouseEvent = previousMouseEvent;
    globalThis.KeyboardEvent = previousKeyboardEvent;
  }
});

test("fake Claude model picker preserves an already exact selection without clicking options", async () => {
  const fixture = makeClaudeModelFixture({ initiallyConfigured: true, delayedThinkingRender: true });
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
    assert.equal(fixture.thinkingClicks, 0);
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

test("fake Claude model acceptance requires Fable 5, Max, and Thinking together", () => {
  const selected = {
    status: "selected",
    requested_model: "fable-5-max",
    model_used: "Fable 5 Max",
    modelVerified: true,
    maxVerified: true,
    thinkingChecked: true
  };
  assert.equal(claudeSiteAdapter.isAcceptableModelSelection(selected), true);
  for (const field of ["modelVerified", "maxVerified", "thinkingChecked"]) {
    assert.equal(
      claudeSiteAdapter.isAcceptableModelSelection({ ...selected, [field]: false }),
      false,
      field
    );
  }
});

function fakeClaudePage({ text, streaming, copy, thinking = false }) {
  const body = { innerText: text, textContent: text };
  const copyButton = {};
  const page = {
    globalCopy: copy ? [copyButton] : []
  };
  const assistant = {
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
    closest() {
      return turn;
    }
  };
  const turn = {
    querySelectorAll(selector) {
      return selector === "[data-testid='action-bar-copy']" && copy ? [copyButton] : [];
    }
  };
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
  delayedSelectionClose = false,
  delayedThinkingRender = false,
  ignoreEscape = false,
  initiallyConfigured = false
} = {}) {
  let menuOpen = false;
  let effortHovered = false;
  let thinkingVisible = false;
  let hoverEvents = 0;
  let sawMousePointer = false;
  let fableClicks = 0;
  let maxClicks = 0;
  let thinkingClicks = 0;

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
          if (delayedThinkingRender) {
            setTimeout(() => { thinkingVisible = true; }, 10);
          } else {
            thinkingVisible = true;
          }
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
        thinkingVisible = false;
        modelButton.setAttribute("aria-expanded", "false");
        return;
      }
      menuOpen = true;
      effortHovered = false;
      thinkingVisible = false;
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
  const sonnet = control({ role: "menuitemradio", "aria-checked": includeFable ? "false" : "true" }, "Sonnet 5");
  const effort = control({ "data-testid": "effort-menu-trigger" }, "Effort");
  const max = control({ role: "menuitemradio", "data-testid": "effort-option-max", "aria-checked": initiallyConfigured ? "true" : "false" }, "Max", (element) => {
    maxClicks += 1;
    element.setAttribute("aria-checked", "true");
    modelButton.innerText = "Fable 5 Max";
    modelButton.textContent = modelButton.innerText;
    effortHovered = false;
    thinkingVisible = false;
    closeAfterSelection();
  });
  const thinking = control({ role: "switch", "aria-label": "Thinking", "aria-checked": initiallyConfigured ? "true" : "false" }, "Thinking", (element) => {
    thinkingClicks += 1;
    element.setAttribute("aria-checked", "true");
  });
  const root = {
    activeElement: modelButton,
    defaultView: { KeyboardEvent: FakeKeyboardEvent },
    querySelector(selector) {
      if (selector === "[data-testid='model-selector-dropdown']") return modelButton;
      if (selector === "[data-testid='effort-menu-trigger']") return menuOpen ? effort : null;
      if (selector === "[role='menuitemradio'][data-testid='effort-option-max']") {
        return menuOpen && effortHovered ? max : null;
      }
      return null;
    },
    querySelectorAll(selector) {
      if (!menuOpen) return [];
      if (selector === "[role='menuitemradio']") {
        return includeFable ? [fable, sonnet] : [sonnet];
      }
      if (selector === "[role='menuitemradio'][aria-checked='true']") {
        return [fable, sonnet, max].filter((element) => element.getAttribute("aria-checked") === "true");
      }
      if (selector === "span[role='switch'][aria-checked]") {
        return effortHovered && thinkingVisible ? [thinking] : [];
      }
      if (selector === "[role='menuitem'], [role='menuitemradio'], button, [role='switch']") {
        return [
          includeFable ? fable : null,
          sonnet,
          effort,
          effortHovered ? max : null,
          effortHovered && thinkingVisible ? thinking : null
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
    thinking,
    get fableClicks() { return fableClicks; },
    get maxClicks() { return maxClicks; },
    get thinkingClicks() { return thinkingClicks; },
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

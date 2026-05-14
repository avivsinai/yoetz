import assert from "node:assert/strict";
import test from "node:test";
import {
  clickSend,
  clickStopGenerating,
  configureModelState,
  ensureFreshChat,
  extractResponse,
  insertPrompt,
  sendAcceptanceBaseline,
  uploadFile,
  waitForSendAccepted
} from "../src/chatgpt-dom.js";

class FakeElement {
  constructor(tagName, attrs = {}, text = "") {
    this.tagName = tagName.toUpperCase();
    this.attrs = { ...attrs };
    this.children = [];
    this.parentElement = null;
    this.ownerDocument = null;
    this.disabled = Boolean(attrs.disabled);
    this.clicked = false;
    this.focused = false;
    this.events = [];
    this.value = attrs.value ?? "";
    this.files = null;
    this.textContent = text;
    this.innerText = text;
    this.onClick = attrs.onClick;
    this.hidden = Boolean(attrs.hidden);
    this.onChange = attrs.onChange;
    delete this.attrs.onClick;
    delete this.attrs.onChange;
  }

  append(...children) {
    for (const child of children) {
      child.parentElement = this;
      child.ownerDocument = this.ownerDocument;
      this.children.push(child);
    }
    return this;
  }

  setAttribute(name, value) {
    this.attrs[name] = String(value);
  }

  getAttribute(name) {
    return this.attrs[name] ?? null;
  }

  focus() {
    this.focused = true;
  }

  click() {
    this.clicked = true;
    this.attrs["aria-checked"] = "true";
    this.onClick?.();
  }

  dispatchEvent(event) {
    this.events.push(event.type);
    if (event.type === "change") {
      this.onChange?.();
    }
    return true;
  }

  getClientRects() {
    if (this.attrs.noLayout) {
      return [];
    }
    return [{}];
  }

  checkVisibility(options = {}) {
    const style = String(this.attrs.style ?? "");
    if (/display\s*:\s*none/i.test(style) || /visibility\s*:\s*hidden/i.test(style)) {
      return false;
    }
    if (options.checkOpacity && /opacity\s*:\s*0(?:\.0+)?\b/i.test(style)) {
      return false;
    }
    if (this.attrs.noLayout) {
      return false;
    }
    return true;
  }

  querySelectorAll(selector) {
    return flatten(this).filter((element) => element !== this && matchesSelector(element, selector));
  }

  closest(selector) {
    let current = this;
    while (current) {
      if (matchesSelector(current, selector)) {
        return current;
      }
      current = current.parentElement;
    }
    return null;
  }
}

class FakeDocument {
  constructor(body) {
    this.body = body;
    this.documentElement = new FakeElement("html").append(body);
    this.defaultView = {
      getComputedStyle: (element) => {
        const style = String(element.attrs.style ?? "");
        return {
          display: /display\s*:\s*none/i.test(style) ? "none" : "block",
          visibility: /visibility\s*:\s*hidden/i.test(style) ? "hidden" : "visible",
          opacity: /opacity\s*:\s*0(?:\.0+)?\b/i.test(style) ? "0" : "1",
          pointerEvents: /pointer-events\s*:\s*none/i.test(style) ? "none" : "auto"
        };
      },
      location: {
        pathname: "/"
      }
    };
    setOwner(this.documentElement, this);
  }

  querySelectorAll(selector) {
    return this.documentElement.querySelectorAll(selector);
  }
}

class FakeDataTransfer {
  constructor() {
    this.files = [];
    this.items = {
      add: (file) => {
        this.files.push(file);
      }
    };
  }
}

test("fake ChatGPT page supports prompt upload send and stable extraction", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  const previousInputEvent = globalThis.InputEvent;
  globalThis.DataTransfer = FakeDataTransfer;
  globalThis.InputEvent = class extends Event {
    constructor(type, init = {}) {
      super(type, init);
      this.inputType = init.inputType;
      this.data = init.data;
    }
  };
  try {
    const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
    const upload = new FakeElement("input", {
      type: "file",
      accept: "text/markdown",
      onChange: () => body.append(new FakeElement("div", { "data-testid": "attachment-file" }, "bundle.md"))
    });
    const send = new FakeElement("button", { "aria-label": "Send message" }, "Send");
    const stop = new FakeElement("button", { "aria-label": "Stop generating" }, "Stop");
    const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
    const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Final answer").append(copy);
    const body = new FakeElement("body", {}, "Message ChatGPT Final answer").append(composer, upload, send, stop, assistant);
    const doc = new FakeDocument(body);

    await insertPrompt(doc, "Review this bundle");
    assert.equal(composer.value, "Review this bundle");
    assert.deepEqual(composer.events, ["input", "change"]);

    const file = new File(["bundle"], "bundle.md", { type: "text/markdown" });
    await uploadFile(doc, file);
    assert.equal(upload.files[0].name, "bundle.md");
    assert.ok(upload.events.includes("change"));

    await clickSend(doc);
    assert.equal(send.clicked, true);

    assert.equal(extractResponse(doc).is_generating, true);
    stop.attrs.style = "display:none";
    body.children = body.children.filter((child) => child !== stop);
    const extraction = extractResponse(doc);
    assert.equal(extraction.method, "copy_scope_dom_fallback");
    assert.equal(extraction.text, "Final answer");
    assert.equal(extraction.is_generating, false);
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
    globalThis.InputEvent = previousInputEvent;
  }
});

test("clickStopGenerating clicks a visible Stop streaming button and reports stopped", () => {
  const stop = new FakeElement("button", { "aria-label": "Stop streaming" }, "Stop");
  const body = new FakeElement("body", {}, "Stop").append(stop);
  const doc = new FakeDocument(body);

  assert.equal(clickStopGenerating(doc), true);
  assert.equal(stop.clicked, true);
});

test("clickStopGenerating returns false when no stop control is rendered", () => {
  const body = new FakeElement("body", {}, "ChatGPT settled");
  const doc = new FakeDocument(body);

  assert.equal(clickStopGenerating(doc), false);
});

test("clickStopGenerating ignores hidden stop controls", () => {
  const stop = new FakeElement("button", { "aria-label": "Stop generating", style: "display:none" }, "Stop");
  const body = new FakeElement("body", {}, "Stop").append(stop);
  const doc = new FakeDocument(body);

  assert.equal(clickStopGenerating(doc), false);
  assert.equal(stop.clicked, false);
});

test("extractResponse ignores user prompt articles and copy controls", () => {
  const userCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const userTurn = new FakeElement("article", { "data-message-author-role": "user" }, "bundle.md\nFile\nReview the attached file and provide your analysis.")
    .append(userCopy);
  const body = new FakeElement("body", {}, "bundle.md File Review the attached file and provide your analysis.").append(userTurn);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);
  assert.equal(extraction.method, "page_text_fallback");
  assert.equal(extraction.assistant_count, 0);
  assert.notEqual(extraction.text, "bundle.md\nFile\nReview the attached file and provide your analysis.");
});

test("extractResponse treats Thinking page text as complete without a stop control", () => {
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Thinking smoke answer")
    .append(copy);
  const body = new FakeElement("body", {}, "GPT-5.4 Thinking Thinking smoke answer").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);
  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Thinking smoke answer");
  assert.equal(extraction.is_generating, false);
});

test("extractResponse prefers the newest assistant turn over an older copy button", () => {
  const oldCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const oldAssistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Old answer")
    .append(oldCopy);
  const newAssistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "New partial answer");
  const body = new FakeElement("body", {}, "Old answer New partial answer").append(oldAssistant, newAssistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "New partial answer");
  assert.equal(extraction.has_copy_button, false);
  assert.equal(extraction.copy_button_count, 1);
  assert.equal(extraction.turn_index, 1);
});

test("extractResponse can use copy-scoped turns without assistant role markers", () => {
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const assistant = new FakeElement("article", {}, "Answer from current ChatGPT DOM")
    .append(copy);
  const body = new FakeElement("body", {}, "Answer from current ChatGPT DOM").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Answer from current ChatGPT DOM");
  assert.equal(extraction.has_copy_button, true);
  assert.equal(extraction.assistant_count, 1);
});

test("extractResponse can use ChatGPT markdown assistant content without role markers", () => {
  const assistant = new FakeElement("article", { class: "agent-turn" }, "Markdown answer")
    .append(new FakeElement("div", { class: "markdown prose" }, "Markdown answer"));
  const body = new FakeElement("body", {}, "Markdown answer").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Markdown answer");
  assert.equal(extraction.has_copy_button, false);
  assert.equal(extraction.assistant_count, 1);
});

test("extractResponse promotes assistant role marker to enclosing turn content", () => {
  const roleMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "Sibling markdown answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const turn = new FakeElement("article", { "data-testid": "conversation-turn-2" }, "")
    .append(roleMarker, markdown, copy);
  const body = new FakeElement("body", {}, "Sibling markdown answer").append(turn);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Sibling markdown answer");
  assert.equal(extraction.turn_index, 0);
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse associates sibling copy controls with nested assistant content", () => {
  const markdown = new FakeElement("div", { "data-testid": "assistant-message", class: "markdown prose" }, "Nested answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const turn = new FakeElement("article", { "data-testid": "conversation-turn-2" }, "")
    .append(markdown, copy);
  const body = new FakeElement("body", {}, "Nested answer").append(turn);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Nested answer");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse does not associate user-turn copy controls with assistant content", () => {
  const prompt = new FakeElement("div", { class: "markdown prose" }, "Review bundle");
  const userCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle")
    .append(prompt, userCopy);
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Final answer")
    .append(new FakeElement("div", { class: "markdown prose" }, "Final answer"));
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer")
    .append(user, assistant);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse does not promote mixed user and assistant role turns", () => {
  const userMarker = new FakeElement("div", { "data-message-author-role": "user" }, "User prompt");
  const roleMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "Assistant marker");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "Sibling markdown answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const turn = new FakeElement("article", { "data-testid": "conversation-turn-mixed" }, "")
    .append(userMarker, roleMarker, markdown, copy);
  const body = new FakeElement("body", {}, "User prompt Sibling markdown answer").append(turn);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Assistant marker");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse ignores ChatGPT model status text when assistant content is not ready", () => {
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Pro thinking")
    .append(copy);
  const body = new FakeElement("body", {}, "Pro thinking").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
  assert.equal(extraction.assistant_count, 1);
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse prefers assistant markdown over wrapper model status text", () => {
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Pro thinking")
    .append(new FakeElement("div", { class: "markdown prose" }, "YOETZ_EXTENSION_NATIVE_SMOKE_OK"))
    .append(copy);
  const body = new FakeElement("body", {}, "Pro thinking YOETZ_EXTENSION_NATIVE_SMOKE_OK").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "YOETZ_EXTENSION_NATIVE_SMOKE_OK");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse ignores single-letter assistant fragments", () => {
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "I")
    .append(copy);
  const body = new FakeElement("body", {}, "I").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
  assert.equal(extraction.assistant_count, 1);
});

test("extractResponse returns single-letter assistant markdown with a copy affordance", () => {
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Thought for 7m 44s\n\nI")
    .append(new FakeElement("div", { class: "markdown prose" }, "I"), copy);
  const body = new FakeElement("body", {}, "Thought for 7m 44s I").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse does not reuse an earlier assistant copy button for a newer assistant text", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const oldMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const oldMarkdown = new FakeElement("div", { class: "markdown prose" }, "Old answer");
  const oldTurn = new FakeElement("div", { class: "turn-messages" }, "Old answer").append(oldMarker, oldMarkdown);
  const oldCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const oldActionRow = new FakeElement("div", { class: "agent-turn" }, "Copy").append(oldCopy);
  const newMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const newMarkdown = new FakeElement("div", { class: "markdown prose" }, "Partial answer");
  const newTurn = new FakeElement("div", { class: "turn-messages" }, "Partial answer").append(newMarker, newMarkdown);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Old answer Copy Partial answer")
    .append(user, oldTurn, oldActionRow, newTurn);
  const body = new FakeElement("body", {}, "Review bundle Old answer Copy Partial answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Partial answer");
  assert.equal(extraction.has_copy_button, false);
  assert.equal(extraction.copy_button_count, 1);
});

test("extractResponse associates a split copy button before assistant markdown within the same response", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const copy = new FakeElement("button", { "aria-label": "Copy", noLayout: true }, "Copy");
  const actionRow = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const responseShell = new FakeElement("div", { "data-testid": "conversation-turn-2", class: "thread" }, "Copy I")
    .append(marker, actionRow, markdown);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Copy I")
    .append(user, responseShell);
  const body = new FakeElement("body", {}, "Review bundle Copy I").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse does not reuse a copy button from an earlier response frame", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const oldMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const oldMarkdown = new FakeElement("div", { class: "markdown prose" }, "Old answer");
  const oldCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const oldFrame = new FakeElement("div", { "data-testid": "conversation-turn-old" }, "Old answer Copy")
    .append(oldMarker, oldMarkdown, new FakeElement("div", { class: "agent-turn" }, "Copy").append(oldCopy));
  const newMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const newMarkdown = new FakeElement("div", { class: "markdown prose" }, "Partial answer");
  const newFrame = new FakeElement("div", { "data-testid": "conversation-turn-new" }, "Partial answer")
    .append(newMarker, newMarkdown);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Old answer Copy Partial answer")
    .append(user, oldFrame, newFrame);
  const body = new FakeElement("body", {}, "Review bundle Old answer Copy Partial answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Partial answer");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse accepts one transcript copy button for one assistant response after the latest user", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "I").append(marker, markdown);
  const copy = new FakeElement("button", { "aria-label": "Copy", noLayout: true }, "Copy");
  const detachedActionRow = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle I Copy")
    .append(user, assistantTurn, detachedActionRow);
  const body = new FakeElement("body", {}, "Review bundle I Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse does not use an unscoped transcript copy button when multiple assistant responses follow the latest user", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const oldMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const oldMarkdown = new FakeElement("div", { class: "markdown prose" }, "Old answer");
  const oldTurn = new FakeElement("div", { class: "turn-messages" }, "Old answer").append(oldMarker, oldMarkdown);
  const copy = new FakeElement("button", { "aria-label": "Copy", noLayout: true }, "Copy");
  const detachedActionRow = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const newMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const newMarkdown = new FakeElement("div", { class: "markdown prose" }, "Partial answer");
  const newTurn = new FakeElement("div", { class: "turn-messages" }, "Partial answer").append(newMarker, newMarkdown);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Old answer Copy Partial answer")
    .append(user, oldTurn, detachedActionRow, newTurn);
  const body = new FakeElement("body", {}, "Review bundle Old answer Copy Partial answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Partial answer");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse associates a copy button after the assistant markdown text node", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "Thought for 7m 44s\n\nI")
    .append(marker, markdown);
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const copyTurn = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Thought for 7m 44s I Copy")
    .append(user, assistantTurn, copyTurn);
  const body = new FakeElement("body", {}, "Review bundle Thought for 7m 44s I Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
  assert.equal(extraction.copy_button_count, 1);
});

test("extractResponse associates a copy button before the assistant markdown text node", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "Thought for 7m 44s\n\nI")
    .append(marker, copy, markdown);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Copy Thought for 7m 44s I")
    .append(user, assistantTurn);
  const body = new FakeElement("body", {}, "Review bundle Copy Thought for 7m 44s I").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
  assert.equal(extraction.copy_button_count, 1);
});

test("extractResponse associates a visually hidden copy button with assistant markdown", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const copy = new FakeElement("button", { "aria-label": "Copy", style: "opacity:0; pointer-events:none" }, "Copy");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "Thought for 7m 24s\n\nI")
    .append(marker, markdown);
  const copyTurn = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Thought for 7m 24s I Copy")
    .append(user, assistantTurn, copyTurn);
  const body = new FakeElement("body", {}, "Review bundle Thought for 7m 24s I Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse associates a zero-layout hidden copy button with assistant markdown", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const copy = new FakeElement("button", { "aria-label": "Copy", noLayout: true }, "Copy");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "I")
    .append(marker, markdown);
  const copyTurn = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle I Copy")
    .append(user, assistantTurn, copyTurn);
  const body = new FakeElement("body", {}, "Review bundle I Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse returns all rendered markdown blocks from one assistant turn", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const intro = new FakeElement("div", { class: "markdown prose" }, "I will review this.");
  const findings = new FakeElement("div", { class: "markdown prose" }, "I have actionable comments.");
  const copy = new FakeElement("button", { "aria-label": "Copy", style: "opacity:0; pointer-events:none" }, "Copy");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "I will review this. I have actionable comments.")
    .append(marker, intro, findings);
  const actionRow = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle I will review this. I have actionable comments. Copy")
    .append(user, assistantTurn, actionRow);
  const body = new FakeElement("body", {}, "Review bundle I will review this. I have actionable comments. Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I will review this.\n\nI have actionable comments.");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse uses the latest user transcript scope for split action rows", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const innerThread = new FakeElement("div", { class: "thread" }, "Final answer")
    .append(marker, markdown);
  const copy = new FakeElement("button", { "aria-label": "Copy", style: "opacity:0; pointer-events:none" }, "Copy");
  const actionRow = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer Copy")
    .append(user, innerThread, actionRow);
  const body = new FakeElement("body", {}, "Review bundle Final answer Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse walks past plain thread wrappers to the transcript scope", () => {
  const userMarkdown = new FakeElement("div", { class: "markdown prose" }, "Review bundle");
  const user = new FakeElement("div", { class: "thread" }, "Review bundle")
    .append(new FakeElement("div", { class: "turn-messages", "data-message-author-role": "user" }, "Review bundle").append(userMarkdown));
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const assistant = new FakeElement("div", { class: "thread" }, "I")
    .append(new FakeElement("div", { class: "turn-messages" }, "I").append(marker, markdown));
  const copy = new FakeElement("button", { "aria-label": "Copy", noLayout: true }, "Copy");
  const actionRow = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const transcript = new FakeElement("main", { role: "main" }, "Review bundle I Copy")
    .append(user, assistant, actionRow);
  const body = new FakeElement("body", {}, "Review bundle I Copy").append(transcript);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse ignores display-none copy buttons after assistant markdown", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const copy = new FakeElement("button", { "aria-label": "Copy", style: "display:none" }, "Copy");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "Thought for 7m 24s\n\nI")
    .append(marker, markdown);
  const copyTurn = new FakeElement("div", { class: "agent-turn" }, "Copy").append(copy);
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Thought for 7m 24s I Copy")
    .append(user, assistantTurn, copyTurn);
  const body = new FakeElement("body", {}, "Review bundle Thought for 7m 24s I Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse does not associate a stale copy button before the latest user turn", () => {
  const oldAnswer = new FakeElement("div", { class: "markdown prose" }, "Old answer");
  const oldCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "I");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "Thought for 7m 44s\n\nI")
    .append(marker, markdown);
  const conversation = new FakeElement("main", { role: "main" }, "Old answer Copy Review bundle Thought for 7m 44s I")
    .append(oldAnswer, oldCopy, user, assistantTurn);
  const body = new FakeElement("body", {}, "Old answer Copy Review bundle Thought for 7m 44s I").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "I");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse ignores popover copy controls after assistant markdown", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const markdown = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const assistantTurn = new FakeElement("div", { class: "turn-messages" }, "Final answer")
    .append(marker, markdown);
  const popover = new FakeElement("div", { role: "dialog" }, "Copy link")
    .append(new FakeElement("button", { "aria-label": "Copy link" }, "Copy"));
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer Copy link")
    .append(user, assistantTurn, popover);
  const body = new FakeElement("body", {}, "Review bundle Final answer Copy link").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse ignores thought-only assistant chrome", () => {
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Thought for 9m 55s\n\nShow more");
  const body = new FakeElement("body", {}, "Question Thought for 9m 55s Show more").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
  assert.equal(extraction.assistant_count, 1);
});

test("extractResponse strips thought chrome while keeping a real assistant answer", () => {
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Thought for 9m 55s\n\nHigh - real finding remains.");
  const body = new FakeElement("body", {}, "Question High - real finding remains.").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "High - real finding remains.");
});

test("extractResponse does not treat Show more reasoning link as assistant content", () => {
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Show reasoning");
  const body = new FakeElement("body", {}, "Question Show reasoning").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
  assert.equal(extraction.assistant_count, 1);
});

test("extractResponse falls back to textContent when assistant innerText is empty", () => {
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "Answer only in textContent")
    .append(copy);
  assistant.innerText = "";
  const body = new FakeElement("body", {}, "Answer only in textContent").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Answer only in textContent");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse does not treat user markdown as assistant content", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "")
    .append(new FakeElement("div", { class: "markdown prose" }, "User prompt"));
  const body = new FakeElement("body", {}, "User prompt").append(user);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
  assert.equal(extraction.assistant_count, 0);
});

test("extractResponse uses standalone assistant markdown after an assistant role marker", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer").append(user, marker, answer, copy);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.preceding_user_count, 1);
  assert.equal(extraction.has_copy_button, true);
  assert.equal(extraction.assistant_count, 1);
});

test("extractResponse uses standalone markdown with zero-layout role markers", () => {
  const user = new FakeElement("div", { "data-message-author-role": "user", noLayout: true }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant", noLayout: true }, "");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer").append(user, marker, answer, copy);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.user_count, 1);
  assert.equal(extraction.assistant_count, 1);
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse returns all standalone markdown blocks after one assistant role marker", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const intro = new FakeElement("div", { class: "markdown prose" }, "First part.");
  const conclusion = new FakeElement("div", { class: "markdown prose" }, "Second part.");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle First part. Second part. Copy")
    .append(user, marker, intro, conclusion, copy);
  const body = new FakeElement("body", {}, "Review bundle First part. Second part. Copy").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "First part.\n\nSecond part.");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse ignores standalone markdown with copy but without assistant ownership", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer").append(user, answer, copy);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
});

test("extractResponse does not use user-turn copy controls for standalone markdown", () => {
  const prompt = new FakeElement("div", { class: "markdown prose" }, "Review bundle");
  const userCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle")
    .append(prompt, userCopy);
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer")
    .append(user, marker, answer);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.has_copy_button, false);
});

test("extractResponse does not let a turn-like user wrapper hide later standalone markdown", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", "data-testid": "conversation-turn-1" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer").append(user, marker, answer, copy);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Final answer");
});

test("extractResponse uses standalone assistant markdown with a sibling role marker", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer").append(user, marker, answer);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "assistant_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.preceding_user_count, 1);
  assert.equal(extraction.has_copy_button, false);
  assert.equal(extraction.assistant_count, 1);
});

test("extractResponse uses the latest standalone assistant markdown candidate", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const partialMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const partial = new FakeElement("div", { class: "markdown prose" }, "Partial answer");
  const partialCopy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const answerMarker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Partial answer Final answer")
    .append(user, partialMarker, partial, partialCopy, answerMarker, answer, copy);
  const body = new FakeElement("body", {}, "Review bundle Partial answer Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Final answer");
  assert.equal(extraction.has_copy_button, true);
});

test("extractResponse ignores standalone markdown when the assistant marker follows it", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Final answer");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Final answer").append(user, answer, marker);
  const body = new FakeElement("body", {}, "Review bundle Final answer").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
});

test("extractResponse preserves generating state for standalone assistant markdown", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const marker = new FakeElement("div", { "data-message-author-role": "assistant" }, "");
  const answer = new FakeElement("div", { class: "markdown prose" }, "Partial answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const stop = new FakeElement("button", { "aria-label": "Stop generating" }, "Stop");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Partial answer Stop").append(user, marker, answer, copy, stop);
  const body = new FakeElement("body", {}, "Review bundle Partial answer Stop").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "copy_scope_dom_fallback");
  assert.equal(extraction.text, "Partial answer");
  assert.equal(extraction.is_generating, true);
});

test("extractResponse ignores split user-role markdown with copy controls", () => {
  const marker = new FakeElement("div", { "data-message-author-role": "user" }, "");
  const prompt = new FakeElement("div", { class: "markdown prose" }, "User prompt");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const userTurn = new FakeElement("div", { class: "turn-messages" }, "User prompt").append(marker, prompt, copy);
  const conversation = new FakeElement("main", { role: "main" }, "User prompt").append(userTurn);
  const body = new FakeElement("body", {}, "User prompt").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
  assert.equal(extraction.assistant_count, 0);
});

test("extractResponse ignores standalone markdown without assistant evidence", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const sidebar = new FakeElement("div", { class: "markdown prose" }, "Sidebar note");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle").append(user);
  const body = new FakeElement("body", {}, "Review bundle Sidebar note").append(conversation, sidebar);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
});

test("extractResponse ignores sidebar thread markdown with a copy button", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle").append(user);
  const sidebar = new FakeElement("aside", {}, "Sidebar note")
    .append(new FakeElement("div", { class: "thread" }, "Sidebar note")
      .append(new FakeElement("div", { class: "markdown prose" }, "Sidebar note"), new FakeElement("button", { "aria-label": "Copy" }, "Copy")));
  const body = new FakeElement("body", {}, "Review bundle Sidebar note").append(conversation, sidebar);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
});

test("extractResponse ignores sidebar markdown with a copy button", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle").append(user);
  const sidebar = new FakeElement("aside", {}, "Sidebar note")
    .append(new FakeElement("div", { class: "markdown prose" }, "Sidebar note"), new FakeElement("button", { "aria-label": "Copy" }, "Copy"));
  const body = new FakeElement("body", {}, "Review bundle Sidebar note").append(conversation, sidebar);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
});

test("extractResponse ignores sidebar markdown inside the conversation landmark", () => {
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "Review bundle");
  const sidebar = new FakeElement("aside", {}, "Sidebar note")
    .append(new FakeElement("div", { class: "markdown prose" }, "Sidebar note"), new FakeElement("button", { "aria-label": "Copy" }, "Copy"));
  const conversation = new FakeElement("main", { role: "main" }, "Review bundle Sidebar note").append(user, sidebar);
  const body = new FakeElement("body", {}, "Review bundle Sidebar note").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
});

test("extractResponse does not reuse standalone markdown before the latest user turn", () => {
  const oldAnswer = new FakeElement("div", { class: "markdown prose" }, "Old answer");
  const copy = new FakeElement("button", { "aria-label": "Copy" }, "Copy");
  const user = new FakeElement("article", { "data-message-author-role": "user", class: "user-turn" }, "New prompt");
  const conversation = new FakeElement("main", { role: "main" }, "Old answer New prompt").append(oldAnswer, copy, user);
  const body = new FakeElement("body", {}, "Old answer New prompt").append(conversation);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.method, "page_text_fallback");
});

test("ensureFreshChat rejects dirty composers and existing attachments", async () => {
  const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT", value: "old draft" });
  const attachment = new FakeElement("div", { "data-testid": "attachment-file" }, "old.md");
  const body = new FakeElement("body", {}, "old draft old.md").append(composer, attachment);
  const doc = new FakeDocument(body);

  await assert.rejects(
    () => ensureFreshChat(doc, { run_id: "run_dirty" }, { timeoutMs: 30, intervalMs: 10 }),
    /not a clean fresh chat/
  );
});

test("ensureFreshChat clicks New Chat before accepting an existing conversation", async () => {
  const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
  const body = new FakeElement("body", {}, "").append(composer);
  const doc = new FakeDocument(body);
  doc.defaultView.location.pathname = "/c/existing";
  const newChat = new FakeElement("a", {
    href: "/",
    "aria-label": "New chat",
    onClick: () => {
      doc.defaultView.location.pathname = "/";
    }
  }, "New chat");
  body.append(newChat);

  const fresh = await ensureFreshChat(doc, { run_id: "run_fresh" }, { timeoutMs: 100, intervalMs: 10 });

  assert.equal(newChat.clicked, true);
  assert.equal(fresh.status, "fresh");
  assert.equal(fresh.pathname, "/");
});

test("ensureFreshChat rejects old transcript residue on a fresh-looking path", async () => {
  const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
  const user = new FakeElement("article", { "data-message-author-role": "user" }, "old prompt");
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "old answer");
  const body = new FakeElement("body", {}, "Message ChatGPT old prompt old answer").append(composer, user, assistant);
  const doc = new FakeDocument(body);

  await assert.rejects(
    () => ensureFreshChat(doc, { run_id: "run_dirty" }, { timeoutMs: 30, intervalMs: 10 }),
    /old conversation transcript did not clear|not a clean fresh chat/
  );
});

test("uploadFile accepts hidden composer-scoped file inputs", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
    const upload = new FakeElement("input", {
      type: "file",
      accept: "text/markdown",
      style: "display:none",
      onChange: () => body.append(new FakeElement("div", { "data-testid": "attachment-file" }, "hidden.md"))
    });
    const body = new FakeElement("body", {}, "").append(composer, upload);
    const doc = new FakeDocument(body);

    const file = new File(["bundle"], "hidden.md", { type: "text/markdown" });
    await uploadFile(doc, file, { timeoutMs: 100, intervalMs: 10 });

    assert.equal(upload.files[0].name, "hidden.md");
    assert.ok(upload.events.includes("change"));
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("uploadFile opens attachment UI when the file input is lazy", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
    const body = new FakeElement("body", {}, "lazy.md").append(composer);
    const attach = new FakeElement("button", {
      "aria-label": "Attach files",
      onClick: () => {
        body.append(new FakeElement("input", {
          type: "file",
          accept: "text/markdown",
          style: "display:none",
          onChange: () => body.append(new FakeElement("div", { "data-testid": "attachment-file" }, "lazy.md"))
        }));
      }
    }, "Attach");
    body.append(attach);
    const doc = new FakeDocument(body);

    const file = new File(["bundle"], "lazy.md", { type: "text/markdown" });
    await uploadFile(doc, file, { timeoutMs: 100, intervalMs: 10, attachmentMenuDelayMs: 0 });

    const upload = body.children.find((child) => child.tagName === "INPUT");
    assert.equal(attach.clicked, true);
    assert.equal(upload.files[0].name, "lazy.md");
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("fake ChatGPT controls can mount after the tab reports loaded", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  const previousInputEvent = globalThis.InputEvent;
  globalThis.DataTransfer = FakeDataTransfer;
  globalThis.InputEvent = class extends Event {
    constructor(type, init = {}) {
      super(type, init);
      this.inputType = init.inputType;
      this.data = init.data;
    }
  };
  try {
    const body = new FakeElement("body", {}, "");
    const doc = new FakeDocument(body);
    const composer = new FakeElement("textarea", { id: "prompt-textarea", placeholder: "Message ChatGPT" });
    const upload = new FakeElement("input", {
      type: "file",
      accept: "text/markdown",
      onChange: () => body.append(new FakeElement("div", { "data-testid": "attachment-file" }, "bundle.md"))
    });
    const send = new FakeElement("button", { "data-testid": "send-button", "aria-label": "Send prompt" }, "Send");
    send.disabled = true;

    setTimeout(() => body.append(composer), 20);
    await insertPrompt(doc, "late prompt", { timeoutMs: 250, intervalMs: 10 });
    assert.equal(composer.value, "late prompt");

    setTimeout(() => body.append(upload), 20);
    const file = new File(["bundle"], "bundle.md", { type: "text/markdown" });
    await uploadFile(doc, file, { timeoutMs: 250, intervalMs: 10 });
    assert.equal(upload.files[0].name, "bundle.md");

    body.append(send);
    setTimeout(() => {
      send.disabled = false;
    }, 20);
    await clickSend(doc, { timeoutMs: 250, intervalMs: 10 });
    assert.equal(send.clicked, true);
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
    globalThis.InputEvent = previousInputEvent;
  }
});

test("uploadFile waits for a new composer-scoped attachment tile", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
    const upload = new FakeElement("input", { type: "file", accept: "text/markdown", style: "display:none" });
    const staleSidebar = new FakeElement("div", { "data-testid": "attachment-sidebar" }, "bundle.md");
    const body = new FakeElement("body", {}, "bundle.md").append(composer, upload, staleSidebar);
    const doc = new FakeDocument(body);

    const started = uploadFile(doc, new File(["bundle"], "bundle.md", { type: "text/markdown" }), {
      timeoutMs: 250,
      intervalMs: 10
    });
    await new Promise((resolve) => setTimeout(resolve, 25));
    assert.equal(upload.files[0].name, "bundle.md");
    assert.equal(staleSidebar.parentElement, body);

    const composerTile = new FakeElement("div", { "data-testid": "attachment-file" }, "bundle.md");
    composer.parentElement.append(composerTile);
    await started;
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("uploadFile rejects a pre-existing composer-scoped node bearing only the filename", async () => {
  // Locks the regression behind H6: a composer-scoped node whose text
  // contains the bundle filename but which lacks any attachment-tile
  // selector (no [class*="file-tile"], no [data-testid*="attachment"]) must
  // not satisfy hasAttachmentNamed on the first poll. Pre-fix the broad
  // hasAttachmentNamed fallback matched such nodes while the narrow
  // baseline selector skipped them during capture, so uploadFile completed
  // without an actual file being attached. Aligning baseline capture to
  // the same broad candidate selector closes the gap.
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
    const upload = new FakeElement("input", { type: "file", accept: "text/markdown", style: "display:none" });
    const stalePlaceholder = new FakeElement("div", {}, "Drop bundle.md here to attach");
    const body = new FakeElement("body", {}, "Drop bundle.md here to attach")
      .append(composer, upload, stalePlaceholder);
    const doc = new FakeDocument(body);

    await assert.rejects(
      () => uploadFile(doc, new File(["bundle"], "bundle.md", { type: "text/markdown" }), {
        timeoutMs: 40,
        intervalMs: 10
      }),
      /did not complete/
    );
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("uploadFile does not accept a stale pre-upload composer attachment", async () => {
  const previousDataTransfer = globalThis.DataTransfer;
  globalThis.DataTransfer = FakeDataTransfer;
  try {
    const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
    const upload = new FakeElement("input", { type: "file", accept: "text/markdown", style: "display:none" });
    const staleTile = new FakeElement("div", { "data-testid": "attachment-file" }, "bundle.md");
    const body = new FakeElement("body", {}, "bundle.md").append(composer, upload, staleTile);
    const doc = new FakeDocument(body);

    await assert.rejects(
      () => uploadFile(doc, new File(["bundle"], "bundle.md", { type: "text/markdown" }), {
        timeoutMs: 40,
        intervalMs: 10
      }),
      /did not complete/
    );
  } finally {
    globalThis.DataTransfer = previousDataTransfer;
  }
});

test("extractResponse ignores code-copy buttons as final assistant affordances", () => {
  const codeCopy = new FakeElement("button", { "aria-label": "Copy code" }, "Copy");
  const pre = new FakeElement("pre", {}, "const x = 1;").append(codeCopy);
  const assistant = new FakeElement("article", { "data-message-author-role": "assistant" }, "const x = 1;")
    .append(pre);
  const body = new FakeElement("body", {}, "const x = 1;").append(assistant);
  const doc = new FakeDocument(body);

  const extraction = extractResponse(doc);

  assert.equal(extraction.has_copy_button, false);
  assert.equal(extraction.copy_button_count, 0);
});

test("hidden ancestors make controls invisible", async () => {
  const send = new FakeElement("button", { "aria-label": "Send message" }, "Send");
  const hiddenForm = new FakeElement("form", { "aria-hidden": "true" }, "").append(send);
  const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT" });
  const body = new FakeElement("body", {}, "").append(composer, hiddenForm);
  const doc = new FakeDocument(body);

  await assert.rejects(
    () => clickSend(doc, { timeoutMs: 40, intervalMs: 10, minTimeoutMs: 0 }),
    /send button not found/
  );
});

test("clickSend waits for a visible ChatGPT send control to become enabled", async () => {
  const composer = new FakeElement("div", { id: "prompt-textarea", role: "textbox" }, "Review this");
  const send = new FakeElement("button", { "aria-label": "Send prompt", "aria-disabled": "true" }, "");
  const body = new FakeElement("body", {}, "Review this").append(composer, send);
  const doc = new FakeDocument(body);

  setTimeout(() => {
    delete send.attrs["aria-disabled"];
  }, 20);

  await clickSend(doc, { timeoutMs: 250, intervalMs: 10 });
  assert.equal(send.clicked, true);
});

test("waitForSendAccepted requires a post-click conversation signal", async () => {
  const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT", value: "Review this" });
  const send = new FakeElement("button", { "aria-label": "Send prompt" }, "Send");
  const body = new FakeElement("body", {}, "Review this").append(composer, send);
  const doc = new FakeDocument(body);
  const baseline = sendAcceptanceBaseline(doc);

  setTimeout(() => {
    body.append(new FakeElement("article", { "data-message-author-role": "user" }, "Review this"));
  }, 20);

  await clickSend(doc, { timeoutMs: 250, intervalMs: 10 });
  const accepted = await waitForSendAccepted(doc, baseline, { timeoutMs: 250, intervalMs: 10 });

  assert.equal(send.clicked, true);
  assert.equal(accepted.send_acceptance_signal, "user_turn");
});

test("waitForSendAccepted rejects a click that leaves ChatGPT idle", async () => {
  const composer = new FakeElement("textarea", { placeholder: "Message ChatGPT", value: "Review this" });
  const send = new FakeElement("button", { "aria-label": "Send prompt" }, "Send");
  const body = new FakeElement("body", {}, "Review this").append(composer, send);
  const doc = new FakeDocument(body);
  const baseline = sendAcceptanceBaseline(doc);

  await clickSend(doc, { timeoutMs: 250, intervalMs: 10 });

  await assert.rejects(
    () => waitForSendAccepted(doc, baseline, { timeoutMs: 30, intervalMs: 10 }),
    /did not accept the prompt/
  );
});

test("clickSend reports disabled send controls distinctly from missing controls", async () => {
  const composer = new FakeElement("div", { id: "prompt-textarea", role: "textbox" }, "Review this");
  const send = new FakeElement("button", { "aria-label": "Send prompt", "aria-disabled": "true" }, "");
  const body = new FakeElement("body", {}, "Review this").append(composer, send);
  const doc = new FakeDocument(body);

  await assert.rejects(
    () => clickSend(doc, { timeoutMs: 30, intervalMs: 10, minTimeoutMs: 0 }),
    (error) => {
      assert.match(error.message, /send button remained disabled/);
      assert.match(error.message, /composer_text_chars/);
      assert.match(error.message, /attachment_tiles/);
      assert.match(error.message, /alerts/);
      return true;
    }
  );
  assert.equal(send.clicked, false);
});

test("fake ChatGPT model controls select model and disable Extended", async () => {
  const extended = new FakeElement("button", { "aria-label": "click to remove Extended" }, "Extended");
  const modelButton = new FakeElement("button", {
    "data-testid": "model-switcher-dropdown-button",
    "aria-haspopup": "menu"
  }, "ChatGPT");
  const proOption = new FakeElement("div", { role: "menuitemradio" }, "GPT-5.4 Pro");
  const selected = new FakeElement("span", { "data-testid": "model-switcher-selected-model" }, "GPT-5.4 Pro");
  const body = new FakeElement("body", {}, "ChatGPT GPT-5.4 Pro").append(extended, modelButton, proOption, selected);
  const doc = new FakeDocument(body);

  const result = await configureModelState(doc, {
    model: "pro",
    disable_extended: true
  });
  assert.equal(extended.clicked, true);
  assert.equal(modelButton.clicked, true);
  assert.equal(proOption.clicked, true);
  assert.equal(result.status, "selected");
  assert.equal(result.extended_status, "disabled");
});

test("fake ChatGPT Extended disable falls back to visible chip text", async () => {
  const composer = new FakeElement("form", { "data-testid": "composer" }, "");
  const extended = new FakeElement("button", {}, "Extended Pro");
  composer.append(extended);
  const body = new FakeElement("body", {}, "ChatGPT Extended Pro").append(composer);
  const doc = new FakeDocument(body);

  const result = await configureModelState(doc, {
    model: "current",
    disable_extended: true
  });

  assert.equal(extended.clicked, true);
  assert.equal(result.status, "kept_current");
  assert.equal(result.extended_status, "disabled");
});

test("fake ChatGPT Extended disable clicks a nested composer chip", async () => {
  const composer = new FakeElement("form", { "data-testid": "composer" }, "");
  const extended = new FakeElement("div", { "data-testid": "extended-thinking-chip", class: "composer-chip" }, "Extended Pro");
  composer.append(extended);
  const body = new FakeElement("body", {}, "ChatGPT Extended Pro").append(composer);
  const doc = new FakeDocument(body);

  const result = await configureModelState(doc, {
    model: "current",
    disable_extended: true
  });

  assert.equal(extended.clicked, true);
  assert.equal(result.status, "kept_current");
  assert.equal(result.extended_status, "disabled");
});

test("fake ChatGPT Extended disable ignores unrelated controls with extended text", async () => {
  const send = new FakeElement("button", { "aria-label": "Send extended prompt" }, "Send");
  const body = new FakeElement("body", {}, "ChatGPT").append(send);
  const doc = new FakeDocument(body);

  const result = await configureModelState(doc, {
    model: "current",
    disable_extended: true
  });

  assert.equal(send.clicked, false);
  assert.equal(result.status, "kept_current");
  assert.equal(result.extended_status, "unavailable");
  assert.match(result.warning, /Extended toggle was not found/);
});

test("fake ChatGPT model controls can select data-testid only model items", async () => {
  const modelButton = new FakeElement("button", {
    "data-testid": "model-switcher-dropdown-button",
    "aria-haspopup": "menu"
  }, "ChatGPT");
  const proOption = new FakeElement("div", { "data-testid": "model-switcher-gpt-5-4-pro" }, "");
  const selected = new FakeElement("span", { "data-testid": "model-switcher-selected-model" }, "GPT-5.4 Pro");
  const body = new FakeElement("body", {}, "ChatGPT").append(modelButton, proOption, selected);
  const doc = new FakeDocument(body);

  const result = await configureModelState(doc, {
    model: "auto",
    disable_extended: false
  });

  assert.equal(modelButton.clicked, true);
  assert.equal(proOption.clicked, true);
  assert.equal(result.status, "selected");
  assert.equal(result.model_used, "GPT-5.4 Pro");
  assert.deepEqual(result.available_options, ["model-switcher-gpt-5-4-pro"]);
});

test("fake ChatGPT model controls do not match gpt-5 by gpt-5.4 substring", async () => {
  const modelButton = new FakeElement("button", {
    "data-testid": "model-switcher-dropdown-button",
    "aria-haspopup": "menu"
  }, "ChatGPT");
  const proOption = new FakeElement("div", { role: "menuitemradio" }, "GPT-5.4 Pro");
  const gpt5Option = new FakeElement("div", { role: "menuitemradio" }, "GPT-5");
  const selected = new FakeElement("span", { "data-testid": "model-switcher-selected-model" }, "GPT-5");
  const body = new FakeElement("body", {}, "ChatGPT GPT-5.4 Pro GPT-5").append(modelButton, proOption, gpt5Option, selected);
  const doc = new FakeDocument(body);

  const result = await configureModelState(doc, {
    model: "gpt-5",
    disable_extended: false
  });

  assert.equal(modelButton.clicked, true);
  assert.equal(proOption.clicked, false);
  assert.equal(gpt5Option.clicked, true);
  assert.equal(result.status, "selected");
  assert.equal(result.model_used, "GPT-5");
});

test("fake ChatGPT model controls keep hyphenated Pro slugs as Pro", async () => {
  const modelButton = new FakeElement("button", {
    "data-testid": "model-switcher-dropdown-button",
    "aria-haspopup": "menu"
  }, "ChatGPT");
  const baseOption = new FakeElement("div", { role: "menuitemradio" }, "GPT-5.4");
  const proOption = new FakeElement("div", { role: "menuitemradio" }, "GPT-5.4 Pro");
  const selected = new FakeElement("span", { "data-testid": "model-switcher-selected-model" }, "GPT-5.4 Pro");
  const body = new FakeElement("body", {}, "ChatGPT GPT-5.4 GPT-5.4 Pro").append(modelButton, baseOption, proOption, selected);
  const doc = new FakeDocument(body);

  const result = await configureModelState(doc, {
    model: "gpt-5.4-pro",
    disable_extended: false
  });

  assert.equal(modelButton.clicked, true);
  assert.equal(baseOption.clicked, false);
  assert.equal(proOption.clicked, true);
  assert.equal(result.status, "selected");
  assert.equal(result.model_used, "GPT-5.4 Pro");
});

function flatten(root) {
  return [root, ...root.children.flatMap(flatten)];
}

function setOwner(element, doc) {
  element.ownerDocument = doc;
  for (const child of element.children) {
    setOwner(child, doc);
  }
}

function matchesSelector(element, selector) {
  return selector
    .split(",")
    .some((part) => matchesSimpleSelector(element, part.trim()));
}

function matchesSimpleSelector(element, selector) {
  if (!selector) return false;
  const tag = element.tagName.toLowerCase();
  const attr = (name) => element.attrs[name];
  const text = String(element.innerText ?? element.textContent ?? "");

  if (selector === '[data-message-author-role="assistant"] [class*="markdown"]') {
    return String(attr("class") ?? "").includes("markdown")
      && Boolean(element.closest('[data-message-author-role="assistant"]'));
  }
  if (selector === 'article:not([data-message-author-role="user"]) [class*="markdown"]') {
    const article = element.closest("article");
    return String(attr("class") ?? "").includes("markdown")
      && article
      && article.getAttribute("data-message-author-role") !== "user";
  }
  if (selector === '[data-testid*="conversation-turn"] [class*="markdown"]') {
    return String(attr("class") ?? "").includes("markdown")
      && Boolean(element.closest('[data-testid*="conversation-turn"]'));
  }
  if (selector === '[class*="agent-turn"] [class*="markdown"]') {
    return String(attr("class") ?? "").includes("markdown")
      && Boolean(element.closest('[class*="agent-turn"]'));
  }
  if (selector === "form") return tag === "form";
  if (selector === "main") return tag === "main";
  if (selector === "#prompt-textarea") return attr("id") === "prompt-textarea";
  if (selector === "button") return tag === "button";
  if (selector === "div") return tag === "div";
  if (selector === "article") return tag === "article";
  if (selector === "pre") return tag === "pre";
  if (selector === "code") return tag === "code";
  if (selector.startsWith("textarea")) return tag === "textarea";
  if (selector.includes('[role="textbox"]')) {
    return attr("role") === "textbox";
  }
  if (selector === 'input[type="file"]') return tag === "input" && attr("type") === "file";
  if (selector.startsWith('input[type="file"][accept*=')) {
    return tag === "input" && attr("type") === "file" && String(attr("accept") ?? "").includes("text");
  }
  if (selector.includes('[data-testid*="composer"]')) {
    return String(attr("data-testid") ?? "").includes("composer");
  }
  if (selector.includes('[data-testid*="attach"]')) {
    return tag === "button" && String(attr("data-testid") ?? "").includes("attach");
  }
  if (selector.includes('[data-testid*="upload"][data-state*="loading"]')) {
    return String(attr("data-testid") ?? "").includes("upload")
      && String(attr("data-state") ?? "").includes("loading");
  }
  if (selector.includes('[data-testid*="attachment"][data-state*="loading"]')) {
    return String(attr("data-testid") ?? "").includes("attachment")
      && String(attr("data-state") ?? "").includes("loading");
  }
  if (selector.includes('[data-testid*="attachment"]')) {
    return String(attr("data-testid") ?? "").includes("attachment");
  }
  if (selector.includes('[class*="code"]')) {
    return String(attr("class") ?? "").includes("code");
  }
  if (selector.includes('[class*="file-tile"]')) {
    return String(attr("class") ?? "").includes("file-tile");
  }
  if (selector.includes('[class*="markdown"]')) {
    if (!String(attr("class") ?? "").includes("markdown")) {
      return false;
    }
    if (selector.includes('[data-message-author-role="assistant"]')) {
      return Boolean(element.closest('[data-message-author-role="assistant"]'));
    }
    if (selector.includes('article:not([data-message-author-role="user"])')) {
      const article = element.closest("article");
      return article && article.getAttribute("data-message-author-role") !== "user";
    }
    return true;
  }
  if (selector.includes('[data-testid*="conversation-turn"]')) {
    return String(attr("data-testid") ?? "").includes("conversation-turn");
  }
  if (selector.includes('[class*="agent-turn"]')) {
    return String(attr("class") ?? "").includes("agent-turn");
  }
  if (selector.includes('[class*="turn-messages"]')) {
    return String(attr("class") ?? "").includes("turn-messages");
  }
  if (selector.includes('[class*="user-turn"]')) {
    return String(attr("class") ?? "").includes("user-turn");
  }
  if (selector.includes('[class*="composer"]')) {
    return String(attr("class") ?? "").includes("composer");
  }
  if (selector.includes('[data-testid*="assistant-message"]')) {
    return String(attr("data-testid") ?? "").includes("assistant-message");
  }
  if (selector.includes('[data-testid*="assistant-response"]')) {
    return String(attr("data-testid") ?? "").includes("assistant-response");
  }
  if (selector.includes('[role="main"]')) {
    return attr("role") === "main";
  }
  if (selector.includes('[data-testid="model-switcher-dropdown-button"]')) {
    return attr("data-testid") === "model-switcher-dropdown-button";
  }
  if (selector.includes('[data-testid^="model-switcher-"]')) {
    const testId = String(attr("data-testid") ?? "");
    return testId.startsWith("model-switcher-") && testId !== "model-switcher-selected-model";
  }
  if (selector.includes('[data-testid="model-switcher-selected-model"]')) {
    return attr("data-testid") === "model-switcher-selected-model";
  }
  if (selector.includes("[aria-haspopup=")) {
    return tag === "button" && Boolean(attr("aria-haspopup"));
  }
  if (selector.includes('[aria-label*="click to remove"]')) {
    return tag === "button" && /click to remove/.test(attr("aria-label") ?? "") && /Extended/.test(attr("aria-label") ?? "");
  }
  if (selector.includes('[data-testid*="extended"')) {
    return /extended/i.test(attr("data-testid") ?? "");
  }
  if (selector.includes('[aria-label*="Send"]')) {
    return tag === "button" && /Send/.test(attr("aria-label") ?? "") && !element.disabled;
  }
  if (selector.includes('[aria-label*="Attach"]')) {
    return tag === "button" && /Attach/i.test(attr("aria-label") ?? "");
  }
  if (selector.includes('[aria-label*="Upload"]')) {
    return tag === "button" && /Upload/i.test(attr("aria-label") ?? "");
  }
  if (selector.includes('[aria-label*="New chat"]')) {
    return /New chat/i.test(attr("aria-label") ?? "");
  }
  if (selector === 'a[href="/"]') {
    return tag === "a" && attr("href") === "/";
  }
  if (selector.includes('[data-testid="send-button"]')) {
    return tag === "button" && attr("data-testid") === "send-button" && !element.disabled;
  }
  if (selector.includes('[data-testid="fruitjuice-send-button"]')) {
    return tag === "button" && attr("data-testid") === "fruitjuice-send-button" && !element.disabled;
  }
  if (selector.includes('button[type="submit"]')) {
    return tag === "button" && attr("type") === "submit" && !element.disabled;
  }
  if (selector.includes('aria-label*="Stop generating"')) {
    return tag === "button" && /Stop generating/i.test(attr("aria-label") ?? "");
  }
  if (selector.includes('aria-label*="Stop streaming"')) {
    return tag === "button" && /Stop streaming/i.test(attr("aria-label") ?? "");
  }
  if (selector.includes('aria-label*="Stop"')) {
    return tag === "button" && /Stop generating|Stop streaming/.test(attr("aria-label") ?? "");
  }
  if (selector.includes('[aria-label*="Copy"]')) {
    return tag === "button" && /Copy/.test(attr("aria-label") ?? "");
  }
  if (selector.includes('[data-message-author-role="assistant"]')) {
    return attr("data-message-author-role") === "assistant";
  }
  if (selector.includes('[data-message-author-role="user"]')) {
    return attr("data-message-author-role") === "user";
  }
  if (selector.includes('[role="menuitem"]')) {
    return attr("role") === "menuitem";
  }
  if (selector.includes('[role="menuitemradio"]')) {
    return attr("role") === "menuitemradio";
  }
  if (selector.includes('[role="option"]')) {
    return attr("role") === "option";
  }
  if (selector.includes('[aria-checked="true"]')) {
    return attr("aria-checked") === "true";
  }
  if (selector.includes('[aria-selected="true"]')) {
    return attr("aria-selected") === "true";
  }
  return text && selector === "*";
}

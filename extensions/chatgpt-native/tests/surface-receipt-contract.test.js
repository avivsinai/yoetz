import assert from "node:assert/strict";
import test from "node:test";
import { verifyChatSurface } from "../src/chatgpt-dom.js";
import { validateChatgptSurfaceProofReceipt } from "../src/surface-receipt.js";

// Minimal fake DOM (same subset as fake-chatgpt.test.js) for the receipt
// contract without pulling the whole fake suite.
class FakeElement {
  constructor(tagName, attrs = {}, text = "") {
    this.tagName = tagName.toUpperCase();
    this.attrs = { ...attrs };
    this.children = [];
    this.parentElement = null;
    this.ownerDocument = null;
    this.textContent = text;
    this.innerText = text;
    this.hidden = false;
    this.disabled = false;
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

  getClientRects() {
    return [{}];
  }

  getBoundingClientRect() {
    return { left: 10, top: 20, width: 200, height: 20, right: 210, bottom: 40 };
  }

  checkVisibility() {
    return true;
  }

  querySelectorAll(selector) {
    return flatten(this).filter((el) => el !== this && matches(el, selector));
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] ?? null;
  }

  closest() {
    return null;
  }
}

class FakeDocument {
  constructor(body) {
    this.body = body;
    this.documentElement = body;
    body.ownerDocument = this;
    for (const child of flatten(body)) child.ownerDocument = this;
  }

  querySelectorAll(selector) {
    return this.body.querySelectorAll(selector);
  }

  querySelector(selector) {
    return this.body.querySelector(selector);
  }
}

function flatten(node, out = []) {
  out.push(node);
  for (const child of node.children ?? []) flatten(child, out);
  return out;
}

function matches(el, selector) {
  const parts = selector.split(",").map((part) => part.trim()).filter(Boolean);
  if (parts.length > 1) {
    return parts.some((part) => matches(el, part));
  }
  if (selector === '[role="group"][aria-label="Composer mode"]') {
    return el.getAttribute("role") === "group" && el.getAttribute("aria-label") === "Composer mode";
  }
  if (selector === '[role="radiogroup"][aria-label="Select chat surface"]') {
    return el.getAttribute("role") === "radiogroup"
      && el.getAttribute("aria-label") === "Select chat surface";
  }
  if (selector === "button") return el.tagName === "BUTTON";
  if (selector.includes('[role="radio"]')) return false;
  if (selector.includes("data-tpp-toggle-value")) return false;
  if (selector.includes(".ProseMirror") || selector.includes('[contenteditable="true"]')) {
    return Boolean(el.getAttribute("class")?.includes("ProseMirror")
      || el.getAttribute("contenteditable") === "true");
  }
  return false;
}

function appendComposerMode(body, {
  chatPressed = "true",
  workPressed = "false",
  foreign = null
} = {}) {
  const chat = new FakeElement("button", { "aria-pressed": chatPressed }, "Chat");
  const work = new FakeElement("button", { "aria-pressed": workPressed }, "Work");
  const group = new FakeElement("div", {
    role: "group",
    "aria-label": "Composer mode"
  }).append(chat, work);
  if (foreign) {
    group.append(new FakeElement("button", { "aria-pressed": "true" }, foreign));
  }
  body.append(group);
  return { chat, work, group };
}

function receiptFromSurface(surface) {
  return {
    surface_evidence_seen: surface.surface_evidence_seen === true,
    surface_proof_kind: surface.surface_proof_kind,
    surface_chat_state: surface.surface_chat_state,
    surface_work_state: surface.surface_work_state,
    surface_visible_toggle_count: surface.surface_visible_toggle_count,
    surface_composer_aria: surface.surface_composer_aria,
    surface_observed_values: surface.observed_values ?? [],
    surface_observed_labels: surface.surface_observed_labels,
    surface_foreign_pressed: surface.surface_foreign_pressed
  };
}

test("yz-c1l contract: Composer mode verifyChatSurface receipt passes SW surface validator", () => {
  const body = new FakeElement("body", {}, "Ask ChatGPT");
  appendComposerMode(body);
  const surface = verifyChatSurface(new FakeDocument(body));
  assert.equal(surface.ok, true, JSON.stringify(surface));
  assert.equal(surface.surface_proof_kind, "explicit_composer_mode_buttons");

  const error = validateChatgptSurfaceProofReceipt(receiptFromSurface(surface));
  assert.equal(error, null, error);
});

test("yz-c1l contract: radio-era proof kind still fails when only aria_pressed is set", () => {
  const body = new FakeElement("body", {}, "Ask ChatGPT");
  appendComposerMode(body);
  const surface = verifyChatSurface(new FakeDocument(body));
  const receipt = receiptFromSurface(surface);
  // Simulate b25007f bug: page emits composer states under the radio proof kind.
  receipt.surface_proof_kind = "explicit_chat_work_radios";
  receipt.surface_observed_labels = null;
  receipt.surface_foreign_pressed = null;
  receipt.surface_observed_values = [];

  const error = validateChatgptSurfaceProofReceipt(receipt);
  assert.match(error ?? "", /explicit Chat\/Work surface proof is incomplete/);
});

for (const [name, mutate, expectedSnippet] of [
  ["Work pressed", (r) => {
    r.surface_work_state = { ...r.surface_work_state, aria_pressed: "true" };
  }, "surface_work_state.aria_pressed"],
  ["both pressed", (r) => {
    r.surface_chat_state = { ...r.surface_chat_state, aria_pressed: "true" };
    r.surface_work_state = { ...r.surface_work_state, aria_pressed: "true" };
  }, "surface_work_state.aria_pressed"],
  ["foreign pressed", (r) => {
    r.surface_foreign_pressed = true;
  }, "surface_foreign_pressed"],
  ["aria_pressed missing", (r) => {
    r.surface_chat_state = { ...r.surface_chat_state, aria_pressed: null };
  }, "surface_chat_state.aria_pressed"],
  // 197f58a live failure: content-script omitted these when packing the receipt.
  ["labels omitted from packed receipt", (r) => {
    r.surface_observed_labels = null;
    r.surface_foreign_pressed = null;
  }, "surface_observed_labels"]
]) {
  test(`yz-c1l contract: Composer mode receipt rejects ${name}`, () => {
    const body = new FakeElement("body", {}, "Ask ChatGPT");
    appendComposerMode(body);
    const receipt = receiptFromSurface(verifyChatSurface(new FakeDocument(body)));
    mutate(receipt);
    const error = validateChatgptSurfaceProofReceipt(receipt);
    assert.match(error ?? "", /explicit Composer mode surface proof is incomplete/);
    assert.match(error ?? "", new RegExp(expectedSnippet));
  });
}

// Live capture 2026-09-29 (chatgpt.com Composer mode): Chat aria-pressed=true,
// Work aria-pressed=false, aria_checked null, exactly two buttons. The packed
// send receipt must carry labels + foreign_pressed or SW/CLI reject it.
test("yz-c1l contract: live Composer mode packed receipt shape validates", () => {
  const livePackedReceipt = {
    surface_evidence_seen: true,
    surface_proof_kind: "explicit_composer_mode_buttons",
    surface_chat_state: {
      aria_checked: null,
      aria_pressed: "true",
      data_state: null
    },
    surface_work_state: {
      aria_checked: null,
      aria_pressed: "false",
      data_state: null
    },
    surface_visible_toggle_count: 2,
    surface_composer_aria: null,
    surface_observed_values: [],
    surface_observed_labels: ["Chat", "Work"],
    surface_foreign_pressed: false
  };
  assert.equal(validateChatgptSurfaceProofReceipt(livePackedReceipt), null);

  const omittedLike197f58a = {
    ...livePackedReceipt,
    surface_observed_labels: null,
    surface_foreign_pressed: null
  };
  const error = validateChatgptSurfaceProofReceipt(omittedLike197f58a);
  assert.match(error ?? "", /surface_observed_labels/);
  assert.match(error ?? "", /surface_foreign_pressed/);
});

test("yz-c1l contract: fixture 2026-09-29-composer-mode-receipt.json validates", async () => {
  const fs = await import("node:fs/promises");
  const raw = await fs.readFile(
    new URL("./fixtures/chatgpt-surface/2026-09-29-composer-mode-receipt.json", import.meta.url),
    "utf8"
  );
  const fixture = JSON.parse(raw);
  delete fixture._provenance;
  assert.equal(validateChatgptSurfaceProofReceipt(fixture), null);
});

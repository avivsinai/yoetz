import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const shimSource = await readFile(new URL("../src/page-visibility-shim.js", import.meta.url), "utf8");

// Runs the shim against a stubbed browser realm: a Document.prototype with a
// genuine visibilityState accessor (so the shim's native-getter capture is
// exercised for real), a stub IntersectionObserver whose native delivery is
// controlled by the test, and a stub requestIdleCallback that fires on a timer
// the test can wait for.
function loadShim({ hidden }) {
  const state = { visibility: hidden ? "hidden" : "visible", nativeIdleFires: [] };
  class FakeDocumentBase {}
  Object.defineProperty(FakeDocumentBase.prototype, "visibilityState", {
    get() { return state.visibility; },
    configurable: true,
    enumerable: true
  });
  Object.defineProperty(FakeDocumentBase.prototype, "hidden", {
    get() { return state.visibility !== "visible"; },
    configurable: true,
    enumerable: true
  });
  const html = { attrs: {}, setAttribute(k, v) { this.attrs[k] = v; }, getAttribute(k) { return this.attrs[k] ?? null; } };
  const doc = Object.create(FakeDocumentBase.prototype);
  doc.documentElement = html;
  doc.addEventListener = () => {};
  doc.querySelector = () => null;

  class NativeIO {
    constructor(callback) { this.callback = callback; this.observed = new Set(); this.disconnected = false; }
    observe(target) { this.observed.add(target); }
    unobserve(target) { this.observed.delete(target); }
    disconnect() { this.observed.clear(); this.disconnected = true; }
  }
  let nativeIdleSeq = 0;
  const nativeIdleTimers = new Map();
  const win = {
    addEventListener: () => {},
    innerWidth: 1280,
    innerHeight: 800,
    requestAnimationFrame: (cb) => setTimeout(() => cb(performance.now()), 16),
    cancelAnimationFrame: (id) => clearTimeout(id),
    IntersectionObserver: NativeIO,
    requestIdleCallback: (cb) => {
      nativeIdleSeq += 1;
      const id = nativeIdleSeq;
      nativeIdleTimers.set(id, setTimeout(() => {
        nativeIdleTimers.delete(id);
        state.nativeIdleFires.push(id);
        cb({ didTimeout: false, timeRemaining: () => 50 });
      }, 300));
      return id;
    },
    cancelIdleCallback: (id) => {
      clearTimeout(nativeIdleTimers.get(id));
      nativeIdleTimers.delete(id);
    }
  };
  const realm = {
    window: win,
    document: doc,
    Document: FakeDocumentBase,
    MessageChannel: class { constructor() { this.port1 = {}; this.port2 = { postMessage: () => { this.port1.onmessage?.(); } }; } },
    performance,
    // Unref'd so the shim's long-lived hydration poll cannot hold the test
    // runner open.
    setTimeout: (fn, ms, ...args) => { const t = setTimeout(fn, ms, ...args); t.unref?.(); return t; },
    clearTimeout,
    setInterval: (fn, ms, ...args) => { const t = setInterval(fn, ms, ...args); t.unref?.(); return t; },
    clearInterval,
    Object
  };
  new Function(...Object.keys(realm), shimSource)(...Object.values(realm));
  return { win, doc, html, state };
}

const tick = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const target = () => ({ getBoundingClientRect: () => ({ x: 0, y: 0, width: 10, height: 10, top: 0, left: 0, right: 10, bottom: 10 }) });

test("shim presents a hidden tab as visible and stamps the presence marker", () => {
  const { doc, html } = loadShim({ hidden: true });
  assert.equal(doc.visibilityState, "visible");
  assert.equal(doc.hidden, false);
  assert.equal(html.getAttribute("data-yoetz-shim"), "1");
});

test("hidden tab: observe delivers exactly one synthetic intersecting entry with viewport rootBounds", async () => {
  const { win } = loadShim({ hidden: true });
  const calls = [];
  const io = new win.IntersectionObserver((entries) => calls.push(entries));
  io.observe(target());
  await tick(20);
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0].isIntersecting, true);
  assert.equal(calls[0][0].intersectionRatio, 1);
  assert.deepEqual([calls[0][0].rootBounds.width, calls[0][0].rootBounds.height], [1280, 800]);
  assert.equal(Object.keys(io).includes("__yoetzCallback"), false);
});

test("hidden tab: repeated observe of one target delivers once, with the observer as this", async () => {
  const { win } = loadShim({ hidden: true });
  const calls = [];
  const io = new win.IntersectionObserver(function (entries, observer) { calls.push([this === io, observer === io, entries.length]); });
  const t = target();
  io.observe(t);
  io.observe(t);
  io.observe(t);
  await tick(20);
  assert.deepEqual(calls, [[true, true, 1]]);
});

test("hidden tab: requestIdleCallback fallback slice shrinks toward zero", async () => {
  const { win } = loadShim({ hidden: true });
  let first = null;
  let later = null;
  win.requestIdleCallback((deadline) => {
    first = deadline.timeRemaining();
    const spin = performance.now() + 20;
    while (performance.now() < spin) { /* burn the slice */ }
    later = deadline.timeRemaining();
  });
  await tick(300);
  assert.ok(first > 0 && first <= 16, `first=${first}`);
  assert.equal(later, 0);
});

test("hidden tab: unobserve or disconnect before delivery drops the synthetic entry", async () => {
  const { win } = loadShim({ hidden: true });
  let calls = 0;
  const first = new win.IntersectionObserver(() => { calls += 1; });
  const t1 = target();
  first.observe(t1);
  first.unobserve(t1);
  const second = new win.IntersectionObserver(() => { calls += 1; });
  second.observe(target());
  second.disconnect();
  await tick(20);
  assert.equal(calls, 0);
});

test("visible tab: observe delivers no synthetic entry and still observes natively", async () => {
  const { win } = loadShim({ hidden: false });
  let calls = 0;
  const io = new win.IntersectionObserver(() => { calls += 1; });
  const t = target();
  io.observe(t);
  await tick(20);
  assert.equal(calls, 0);
  assert.equal(io.observed.has(t), true);
});

test("hidden tab: requestIdleCallback fires once from the fallback timer as a normal idle slice", async () => {
  const { win, state } = loadShim({ hidden: true });
  const deadlines = [];
  win.requestIdleCallback((deadline) => deadlines.push({ didTimeout: deadline.didTimeout, remaining: deadline.timeRemaining() }), { timeout: 5000 });
  await tick(400);
  assert.equal(deadlines.length, 1);
  assert.equal(deadlines[0].didTimeout, false);
  assert.ok(deadlines[0].remaining > 0);
  assert.deepEqual(state.nativeIdleFires, [], "native delivery must be cancelled after the fallback fired");
});

test("visible tab: requestIdleCallback still delivers the native callback exactly once", async () => {
  const { win } = loadShim({ hidden: false });
  let calls = 0;
  win.requestIdleCallback(() => { calls += 1; });
  await tick(400);
  assert.equal(calls, 1);
});

test("requestIdleCallback registered while visible still delivers if the tab hides before native fires", async () => {
  const { win, state } = loadShim({ hidden: false });
  let calls = 0;
  win.requestIdleCallback(() => { calls += 1; });
  state.visibility = "hidden";
  await tick(400);
  assert.equal(calls, 1);
});

test("cancelIdleCallback suppresses both fallback and native delivery", async () => {
  const { win } = loadShim({ hidden: true });
  let calls = 0;
  const id = win.requestIdleCallback(() => { calls += 1; });
  win.cancelIdleCallback(id);
  await tick(400);
  assert.equal(calls, 0);
});

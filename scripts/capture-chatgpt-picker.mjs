#!/usr/bin/env node
// scripts/capture-chatgpt-picker.mjs — dev-only ChatGPT picker DOM capture.
//
// Drives a raw CDP (Chrome DevTools Protocol) session against a foreground
// Chrome tab that already has the model picker open, serializes the open
// [role="menu"] to a self-contained HTML file, and writes it to --out. The
// output is a snapshot fixture consumed by tests/chatgpt-picker-reader.test.js
// (jsdom) — never by the extension, the native host, or inspect_run.
//
// Usage:
//   node scripts/capture-chatgpt-picker.mjs \
//     --cdp-url http://127.0.0.1:9222 \          # or set $YOETZ_CDP_URL
//     --tab-url-contains chatgpt.com \           # first matching page target
//     --out tests/fixtures/chatgpt-picker/2026-09-03-shape-name.html
//
// The CDP endpoint may be an http:// base (preferred) or a ws:// browser
// debugger URL; the host:port is reused for the HTTP /json/list probe. No npm
// dependencies — Node 24 globals only (fetch, WebSocket, fs). Exits non-zero
// with a clear message if no [role=menu] is open in the matched tab.

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

function parseArgs(argv) {
  const args = {
    cdpUrl: process.env.YOETZ_CDP_URL ?? "",
    browserWs: process.env.YOETZ_BROWSER_WS ?? "",
    tabUrlContains: "",
    out: "",
  };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--cdp-url") args.cdpUrl = argv[++i];
    else if (a === "--browser-ws") args.browserWs = argv[++i];
    else if (a === "--tab-url-contains") args.tabUrlContains = argv[++i];
    else if (a === "--out") args.out = argv[++i];
    else if (a === "-h" || a === "--help") { usage(); process.exit(0); }
    else die(`unknown argument: ${a}`);
  }
  // A ws://.../devtools/browser/... URL passed as --cdp-url doubles as the
  // browser WS (Chrome 152 builds where HTTP /json/* is 404 but the browser
  // WS from DevToolsActivePort still listens).
  if (!args.browserWs && isBrowserWsUrl(args.cdpUrl)) args.browserWs = args.cdpUrl;
  if (!args.cdpUrl && args.browserWs) args.cdpUrl = args.browserWs;
  if (!args.cdpUrl) die("--cdp-url is required (or set $YOETZ_CDP_URL)");
  if (!args.tabUrlContains) die("--tab-url-contains is required");
  if (!args.out) die("--out is required");
  return args;
}

function usage() {
  console.error(`usage: capture-chatgpt-picker.mjs
  --cdp-url <url>            CDP base, e.g. http://127.0.0.1:9222 (or $YOETZ_CDP_URL).
                             A ws://.../devtools/browser/... URL also works and
                             selects the browser-WS path automatically.
  --browser-ws <wsurl>       browser debugger WS, e.g. ws://127.0.0.1:9222/devtools/browser/<id>
                             (or $YOETZ_BROWSER_WS). Uses Target.getTargets +
                             flattened attach; kept alongside /json/list fallback.
  --tab-url-contains <frag>  substring matched against page target URLs (first wins)
  --out <path>               destination HTML file`);
}

function isBrowserWsUrl(url) {
  return typeof url === "string" && /^wss?:\/\//i.test(url) && url.includes("/devtools/browser/");
}

function die(msg) { usage(); console.error(`error: ${msg}`); process.exit(1); }

function httpBaseFrom(url) {
  // Accept http://host:port, ws://host:port/..., or bare host:port.
  const m = url.match(/^(?:https?:\/\/|wss?:\/\/)?([^/]+)/i);
  if (!m) die(`could not parse host:port from cdp-url: ${url}`);
  return `http://${m[1]}`;
}

async function findPageTarget(httpBase, fragment) {
  const res = await fetch(`${httpBase}/json/list`);
  if (!res.ok) die(`/json/list returned ${res.status} ${res.statusText}`);
  const targets = await res.json();
  const pages = targets.filter((t) => t.type === "page");
  const match = pages.find((t) => typeof t.url === "string" && t.url.includes(fragment));
  if (!match) {
    die(`no page target with url containing "${fragment}" among ${pages.length} page target(s)`);
  }
  if (!match.webSocketDebuggerUrl) die(`matched target has no webSocketDebuggerUrl: ${match.url}`);
  return match;
}

function cdpEvaluate(wsUrl, expression) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl);
    const id = 1;
    const timeout = setTimeout(() => {
      ws.close();
      reject(new Error("Runtime.evaluate timed out after 15s"));
    }, 15000);
    ws.onopen = () => {
      ws.send(JSON.stringify({
        id,
        method: "Runtime.evaluate",
        params: { expression, returnByValue: true, awaitPromise: false },
      }));
    };
    ws.onmessage = (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id !== id) return;
      clearTimeout(timeout);
      ws.close();
      if (msg.error) reject(new Error(`CDP error: ${JSON.stringify(msg.error)}`));
      else if (msg.result?.exceptionDetails) {
        reject(new Error(`page exception: ${msg.result.exceptionDetails.text}`));
      } else resolve(msg.result?.result?.value);
    };
    ws.onerror = () => { clearTimeout(timeout); reject(new Error(`websocket error to ${wsUrl}`)); };
  });
}

// In-page serializer. Runs as a pure function in the page via Runtime.evaluate.
// Returns the outerHTML of the open picker menu, or throws if none is open.
const SERIALIZER = String.raw`
(function () {
  var live = document.querySelector('[role="menu"][data-state="open"]')
          || document.querySelector('[role="menu"]');
  if (!live) throw new Error('no [role="menu"] found in the page');
  var clone = live.cloneNode(true);

  function effectivelyInert(el) {
    for (var node = el; node && node.nodeType === 1; node = node.parentElement) {
      if (node.hasAttribute && node.hasAttribute('inert')) return true;
    }
    return false;
  }

  // Walk live and clone in parallel (same tree order) to copy live state onto
  // the clone: computed inert as an attribute, data-state/aria-* verbatim, and
  // computed display/visibility written inline when none/hidden (jsdom has no
  // layout engine, so the reader's attribute+inline-style predicate cannot see
  // stylesheet-driven display:none/visibility:hidden unless we bake them in).
  function sync(liveEl, cloneEl) {
    if (!liveEl || !cloneEl || cloneEl.nodeType !== 1) return;
    if (effectivelyInert(liveEl)) cloneEl.setAttribute('inert', '');
    else cloneEl.removeAttribute('inert');
    // data-state and aria-* are already attributes on the clone (it was cloned
    // from live), but re-copy to guarantee they survive any later mutation.
    if (liveEl.hasAttribute('data-state')) {
      cloneEl.setAttribute('data-state', liveEl.getAttribute('data-state'));
    }
    var ariaNames = [];
    for (var i = 0; i < liveEl.attributes.length; i++) {
      var name = liveEl.attributes[i].name;
      if (name === 'data-state' || name.indexOf('aria-') === 0) ariaNames.push(name);
    }
    for (var j = 0; j < ariaNames.length; j++) {
      cloneEl.setAttribute(ariaNames[j], liveEl.getAttribute(ariaNames[j]));
    }
    // Bake computed display/visibility inline so jsdom's attribute+inline-style
    // readability predicate sees the same hidden state Chrome does. Only write
    // when the computed value hides the node; never overwrite an existing
    // inline value that already expresses the same intent.
    var computed = liveEl.ownerDocument && liveEl.ownerDocument.defaultView
      ? liveEl.ownerDocument.defaultView.getComputedStyle(liveEl) : null;
    if (computed) {
      if (computed.display === 'none') cloneEl.style.setProperty('display', 'none', 'important');
      if (computed.visibility === 'hidden') cloneEl.style.setProperty('visibility', 'hidden', 'important');
    }
    var liveKids = liveEl.children, cloneKids = cloneEl.children;
    var ci = 0;
    for (var li = 0; li < liveKids.length && ci < cloneKids.length; li++) {
      // Index parity holds only because cloneNode(true) preserves child order,
      // so liveKids[i] corresponds to cloneKids[i] one-to-one.
      sync(liveKids[li], cloneKids[ci]);
      ci++;
    }
  }
  sync(live, clone);

  // Strip the bodies of <script>, <svg>, <use>, <canvas> in the clone. The
  // element tag is kept so tree structure (and thus selector parity with live)
  // is preserved; only their heavy/sensitive content is removed.
  var strip = clone.querySelectorAll('script, svg, use, canvas');
  for (var k = 0; k < strip.length; k++) {
    while (strip[k].firstChild) strip[k].removeChild(strip[k].firstChild);
  }

  return clone.outerHTML;
})
`;

// Serializer summary: clones the open menu, copies computed inert +
// data-state/aria-* + computed display/visibility (when none/hidden) from the
// live node onto the clone, strips the bodies of <script>/<svg>/<use>/<canvas>,
// and returns outerHTML. Computed styles are baked in because jsdom has no
// layout engine (see docs/design/chatgpt-picker-reader.md "jsdom boundary").

// Browser-WS path: for Chrome builds where HTTP /json/* is 404 but the
// browser WS from DevToolsActivePort still listens. Never calls
// Target.activateTarget (it wedges WS upgrades); flattened attach only.
function cdpSession(wsUrl, { timeoutMs = 15000 } = {}) {
  const ws = new WebSocket(wsUrl);
  let nextId = 0;
  const pending = new Map();
  let openError = null;
  const ready = new Promise((resolve, reject) => {
    ws.onopen = () => resolve();
    ws.onerror = () => { openError = new Error(`websocket error to ${wsUrl}`); };
    ws.onclose = () => {
      if (nextId === 0 && openError) reject(openError);
    };
    setTimeout(() => {
      if (nextId === 0) reject(new Error(`websocket open timed out after 15s: ${wsUrl}`));
    }, timeoutMs);
  });
  ws.onmessage = (event) => {
    let msg;
    try { msg = JSON.parse(event.data); } catch { return; }
    if (msg.id == null || !pending.has(msg.id)) return;
    const { resolve, reject, timer } = pending.get(msg.id);
    pending.delete(msg.id);
    clearTimeout(timer);
    if (msg.error) reject(new Error(`CDP error: ${JSON.stringify(msg.error)}`));
    else resolve(msg.result ?? {});
  };
  async function send(method, params = {}, ms = timeoutMs) {
    await ready;
    const id = ++nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`${method} timed out after ${Math.round(ms / 1000)}s`));
      }, ms);
      pending.set(id, { resolve, reject, timer });
      const payload = { id, method, params };
      if (params.sessionId) {
        // Flattened sessions ride on the browser WS connection.
        payload.sessionId = params.sessionId;
        delete payload.params.sessionId;
      }
      ws.send(JSON.stringify(payload));
    });
  }
  function close() { try { ws.close(); } catch { /* ignore */ } }
  return { send, close };
}

async function findPageTargetViaBrowserWs(browserWsUrl, fragment) {
  const { send, close } = cdpSession(browserWsUrl);
  try {
    const { targetInfos } = await send("Target.getTargets");
    const pages = (targetInfos ?? []).filter((t) => t.type === "page");
    const match = pages.find((t) => typeof t.url === "string" && t.url.includes(fragment));
    if (!match) {
      throw new Error(`no page target with url containing "${fragment}" among ${pages.length} page target(s) via browser WS`);
    }
    return match;
  } finally {
    close();
  }
}

async function evaluateViaBrowserWs(browserWsUrl, targetId, expression) {
  const { send, close } = cdpSession(browserWsUrl);
  try {
    const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
    if (!sessionId) throw new Error("Target.attachToTarget returned no sessionId");
    try {
      const res = await send(
        "Runtime.evaluate",
        { sessionId, expression, returnByValue: true, awaitPromise: false },
      );
      if (res?.exceptionDetails) {
        const d = res.exceptionDetails;
        const detail = d.exception?.description ?? d.text ?? JSON.stringify(d);
        throw new Error(`page exception: ${String(detail).split("\n")[0]}`);
      }
      return res?.result?.value;
    } finally {
      try { await send("Target.detachFromTarget", { sessionId }); } catch { /* ignore */ }
    }
  } finally {
    close();
  }
}

async function main() {
  const args = parseArgs(process.argv);
  // Prefer the browser-WS path when available; keep /json/list as fallback.
  if (args.browserWs) {
    try {
      const match = await findPageTargetViaBrowserWs(args.browserWs, args.tabUrlContains);
      console.error(`matched target (browser WS): ${match.url}`);
      const html = await evaluateViaBrowserWs(args.browserWs, match.targetId, `${SERIALIZER}()`);
      if (!html) die("serializer returned an empty value");
      mkdirSync(dirname(args.out), { recursive: true });
      writeFileSync(args.out, html);
      console.error(`wrote ${args.out} (${html.length} bytes)`);
      return;
    } catch (err) {
      console.error(`browser-WS path failed (${err.message}); falling back to /json/list`);
    }
  }
  const httpBase = httpBaseFrom(args.cdpUrl);
  const target = await findPageTarget(httpBase, args.tabUrlContains);
  console.error(`matched target: ${target.url}`);
  const html = await cdpEvaluate(target.webSocketDebuggerUrl, `${SERIALIZER}()`);
  if (!html) die("serializer returned an empty value");
  mkdirSync(dirname(args.out), { recursive: true });
  writeFileSync(args.out, html);
  console.error(`wrote ${args.out} (${html.length} bytes)`);
}

main().catch((err) => { console.error(`error: ${err.message}`); process.exit(1); });

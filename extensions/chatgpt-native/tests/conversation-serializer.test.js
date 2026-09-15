import assert from "node:assert/strict";
import { test } from "node:test";
import { JSDOM } from "jsdom";
import { serializeConversation } from "../src/conversation-serializer.js";
import { serializePickerMenu } from "../src/picker-serializer.js";

// yz-7iu findings: NEITHER capture surface may carry session secrets to disk.
// Fixture: one real assistant turn / picker rows next to a hidden input
// holding a JWT, a <template> holding a JWT, secret-bearing attribute
// values, an onclick handler, a token-bearing href, and a <style> block.
// Both serializers must keep the recoverable content (turn text, extractor
// data-* markers, unknown data-* evidence, picker data-state/aria-expanded)
// and emit no "eyJ".
const JWT = [
  Buffer.from(JSON.stringify({ alg: "HS256", typ: "JWT" })).toString("base64url"),
  Buffer.from(JSON.stringify({ sub: "test-subject-1234567890" })).toString("base64url"),
  "s".repeat(43)
].join(".");
const JWT_SHAPE = /eyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\./;

function secretLadenDocument(surfaceMarkup) {
  return new JSDOM(`<!doctype html><html><body>
    ${surfaceMarkup}
    <!-- Channels the sanitizer strips at the source: -->
    <input type="hidden" name="state" value="${JWT}">
    <template id="t"><span data-session="${JWT}">jwt in template</span></template>
    <div data-auth="${JWT}" onclick="steal()">tokened</div>
    <a href="https://chatgpt.com/api/auth/session?access_token=${JWT}">session link</a>
    <style>.markdown { color: red; content: "${JWT}"; }</style>
    <img src="https://chatgpt.com/track?token=${JWT}">
    <!-- A channel only the final JWT-shape guard can catch: text inside a
         kept element. Inside the captured surface — the serializers only
         serialize their root, so bait outside <main>/<menu> is never
         captured: -->
  </body></html>`);
}

function assertSanitized(html, label, redactions) {
  // The final guard caught any surviving JWT-shaped string.
  assert.equal(
    JWT_SHAPE.test(html),
    false,
    `${label}: JWT shape survived the capture: ${html.slice(0, 400)}`
  );
  assert.ok(redactions >= 1, `${label}: redactions count must report JWT replacements`);

  // Attribute/value channels are stripped at the source, not just the guard.
  // (An empty value="" attribute is still emitted by serialization; what must
  // not survive is a non-empty value or the raw token text.)
  assert.ok(!html.includes(`value="${JWT}"`), `${label}: input value must not survive`);
  assert.ok(!html.includes("data-auth="), `${label}: secret-bearing data-* value must not survive`);
  assert.ok(!html.includes("onclick="), `${label}: inline event handlers must not survive`);
  assert.ok(!html.includes("access_token="), `${label}: token-bearing href must not survive`);
  assert.ok(!html.includes("?token="), `${label}: token-bearing src must not survive`);
  assert.ok(!html.includes("jwt in template"), `${label}: template content must not survive`);
}

// yz-y5p: the value denylist must not eat benign prose. `=` always assigns, but a
// colon also separates ordinary label text, so "Session: today" in an aria-label
// is not a secret; dropping it costs the diagnostic evidence a capture exists to
// preserve. A colon counts only when a serialized value follows.
test("yz-beb: <template> contents do not survive serialization", () => {
  // template children live in template.content, which neither the childNodes
  // strip nor querySelectorAll("*") reached; the fragment serialized intact.
  const dom = secretLadenDocument(`
    <main>
      <template><input value="TEMPLATE_SENTINEL"><span data-x="access_token=OPAQUE_SENTINEL">inside template</span></template>
      <div data-testid="conversation-turn">answer</div>
    </main>
  `);
  const html = serializeConversation(dom.window.document);
  assert.ok(!html.includes("TEMPLATE_SENTINEL"), "template input value must not survive");
  assert.ok(!html.includes("inside template"), "template body text must not survive");
  assert.ok(!html.includes("<template"), "the template element itself must not survive");
  assert.ok(html.includes("answer"), "the kept turn must survive");
});

test("yz-beb: textarea default (child text) does not survive serialization", () => {
  // .value="" clears the live value only; outerHTML renders the child text.
  // The assertion is on the SERIALIZED HTML, not element.value.
  const dom = secretLadenDocument(`
    <main>
      <textarea>TEXTAREA_SENTINEL</textarea>
    </main>
  `);
  const html = serializeConversation(dom.window.document);
  assert.ok(!html.includes("TEXTAREA_SENTINEL"), `textarea child text survived: ${html.slice(0, 400)}`);
  assert.ok(html.includes("<textarea"), "the textarea element keeps its tree position");
});

test("yz-djy: short-header and full-token JWTs are redacted whole", () => {
  // (a) the 19-char HS256 header escaped the old {20,} guard entirely;
  // (b) the old match ended at the second period, leaving the signature
  //     segment appended after [REDACTED_JWT].
  const shortHeaderJwt = `eyJhbGciOiJIUzI1NiJ9.${"A".repeat(20)}.${"S".repeat(20)}`;
  const dom = secretLadenDocument(`
    <main>
      <div>${shortHeaderJwt} tail</div>
    </main>
  `);
  const html = serializeConversation(dom.window.document);
  assert.ok(!html.includes("eyJ"), `a short-header JWT survived: ${html.slice(0, 300)}`);
  assert.ok(html.includes("[REDACTED_JWT] tail"), `signature must not trail the redaction: ${html.slice(0, 300)}`);
});

test("yz-y5p: benign 'Session: today' prose survives while a real colon assignment is dropped", () => {
  const dom = secretLadenDocument(`
    <main>
      <div aria-label="Session: today" data-testid="session-list" title="Secret: hidden">history</div>
      <div data-auth='session: "abc123"'>bearer</div>
      <div data-q="?access_token=zzz">query</div>
    </main>
  `);
  const html = serializeConversation(dom.window.document);

  assert.ok(html.includes('aria-label="Session: today"'), "a prose colon is not a secret");
  assert.ok(html.includes('data-testid="session-list"'), "a benign testid value survives");
  assert.ok(html.includes('title="Secret: hidden"'), "a prose colon in a title is not a secret");
  assert.ok(!html.includes("abc123"), "a quoted assignment after the colon IS a secret");
  assert.ok(!html.includes("zzz"), "an access_token= query value is still dropped");
});

test("conversation serializer redacts secrets but keeps the assistant turn and unknown data-*", () => {
  const dom = secretLadenDocument(`
    <main>
      <div data-testid="conversation-turn" data-message-author-role="assistant">
        <div class="markdown">the recovered answer</div>
      </div>
      <div data-unknown-shape="advanced-view">evidence jwt-bait ${JWT}</div>
      <div data-yoetz-ownership-nonce="nonce-abc">anchored</div>
    </main>
  `);
  const html = serializeConversation(dom.window.document);
  const redactions = serializeConversation.lastRedactions ?? 0;

  // The recovery payload survives: turn text + extractor-readable markers.
  assert.ok(html.includes("the recovered answer"), "assistant turn text must survive");
  assert.ok(
    html.includes('data-message-author-role="assistant"'),
    "data-message-author-role must survive"
  );
  assert.ok(html.includes('data-testid="conversation-turn"'), "data-testid must survive");
  // Unknown data-* is evidence a drift capture exists to preserve — kept.
  assert.ok(
    html.includes('data-unknown-shape="advanced-view"'),
    "unknown data-* names must survive (value denylist, not name allowlist)"
  );
  // The ownership nonce is a per-run capability: redacted, structure kept.
  assert.ok(
    html.includes('data-yoetz-ownership-nonce="[REDACTED]"'),
    "ownership nonce must read [REDACTED]"
  );
  assert.ok(!html.includes("nonce-abc"), "raw nonce value must not survive");
  // Style bodies are stripped (the tag may remain for tree structure).
  const styleIndex = html.indexOf("<style");
  if (styleIndex >= 0) {
    assert.ok(!/>[^<]/.test(html.slice(styleIndex)), "style body must not survive");
  }

  assertSanitized(html, "conversation", redactions);
});

test("picker serializer redacts secrets but keeps the picker rows", () => {
  const dom = secretLadenDocument(`
    <div role="menu" data-state="open" aria-expanded="true">
      <div role="menuitemradio" data-state="checked" aria-checked="true" data-testid="picker-item-latest">Latest ${JWT}</div>
    </div>
  `);
  const html = serializePickerMenu(dom.window.document);
  const redactions = serializePickerMenu.lastRedactions ?? 0;

  // Picker fixture parity survives: rows keep their state attributes.
  assert.ok(html.includes("Latest"), "picker row text must survive");
  assert.ok(html.includes('data-state="open"'), "menu data-state must survive");
  assert.ok(html.includes('aria-expanded="true"'), "aria-expanded must survive");
  assert.ok(html.includes('aria-checked="true"'), "aria-checked must survive");
  assert.ok(html.includes('data-testid="picker-item-latest"'), "picker data-testid must survive");

  assertSanitized(html, "picker", redactions);
});

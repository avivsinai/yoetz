// capture-sanitizer.js — shared redaction pass for Yoetz DOM captures
// (yz-7iu findings). Both picker-serializer.js and conversation-serializer.js
// call sanitizeCaptureClone(clone) on their clone, then redactSecrets() on
// the serialized markup. They differ only in the root they pick.
//
// A capture file can carry session secrets to disk (hidden input values,
// template fragments, style blocks, secret-bearing attribute values,
// JWT-shaped strings). This module strips those channels at the source and
// adds a final JWT-shape guard, so neither surface can leak a credential
// into a fixture, a bead attachment, or a chat.
//
// Attribute policy is a VALUE denylist, not a name allowlist: values leak,
// names do not. The tracked picker fixtures carry 34 distinct data-*
// attributes and the readers parse 3 — an allowlist would strip the other
// 31 (data-has-advanced-view, data-model-selection-view, data-max-effort…),
// which is exactly the evidence a drift capture exists to preserve.

const REDACTED_JWT = "[REDACTED_JWT]";
// yz-djy: a JWT is three base64url segments; the old pattern quantified 20+
// chars into the FIRST segment only, so the 19-char HS256 header
// ("eyJhbGciOiJIUzI1NiJ9") escaped the guard entirely, and it stopped at the
// second period, leaving the signature segment appended after [REDACTED_JWT].
// The signature alone is not the original credential, but whole-token
// redaction is what this guard claims to do. A trailing negative lookahead
// anchors the match at the token end (a real token is followed by a
// non-base64url char or the end of the string), so trailing prose survives.
const JWT_SHAPE = /eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*(?![A-Za-z0-9_-])/g;
// Any attribute VALUE carrying a JWT prefix or a secret-bearing key is dropped.
// `=` always assigns, so it needs no qualification. `:` is ambiguous - it also
// separates ordinary prose ("Session: today" in an aria-label, "Secret: hidden"
// in a chat title) - so a colon counts only when what follows looks like a
// serialized value: a quote, a brace, or a JWT prefix. Without that qualifier the
// denylist drops benign labels, which costs exactly the diagnostic evidence a
// capture exists to preserve. (`yz-y5p`, folded in from the yz-7iu review.)
const SECRET_KEYS = "token|access_token|id_token|refresh_token|authorization|session|secret|api[_-]?key";
const SECRET_VALUE_SHAPE = new RegExp(
  `eyJ[A-Za-z0-9_-]{20,}\\.`
  + `|(^|[?&;\\s])(${SECRET_KEYS})\\s*=`
  + `|(^|[?&;\\s])(${SECRET_KEYS})\\s*:\\s*["'{[]`
  + `|(^|[?&;\\s])(${SECRET_KEYS})\\s*:\\s*eyJ`,
  "i"
);
// Form controls keep their element (tree structure) but lose their value.
const STRIP_VALUE_SELECTOR = "input, textarea, select";
// Bodies stripped wholesale: heavy, sensitive, or non-serializable content.
// <template> fragments are removed entirely — their parsed .content subtree
// does not serialize through outerHTML edits.
const STRIP_BODY_SELECTOR =
  "script, style, template, noscript, iframe, object, embed, svg, use, canvas";

// The per-run ownership nonce is a capability, not structure: blank it so the
// attribute (and the tree it anchors) survives without the value.
const OWNERSHIP_NONCE_ATTRIBUTE = "data-yoetz-ownership-nonce";

function attributeIsSafe(name, value) {
  const lower = name.toLowerCase();
  // Inline event handlers and iframe inline documents never survive.
  if (/^on[a-z]+$/.test(lower) || lower === "srcdoc") return false;
  // A value attribute is dropped: form-control values are stripped in full.
  if (lower === "value") return false;
  if (lower === OWNERSHIP_NONCE_ATTRIBUTE) return false;
  // VALUE denylist: a secret-shaped value poisons any attribute name.
  return !SECRET_VALUE_SHAPE.test(value);
}

// Mutates the clone in place: strips sensitive element bodies, drops
// secret-bearing attribute values, and empties form controls.
export function sanitizeCaptureClone(clone) {
  for (const node of clone.querySelectorAll(STRIP_BODY_SELECTOR)) {
    // yz-beb: <template> children live in node.content, NOT childNodes —
    // emptying childNodes left the parsed fragment intact and it serializes
    // through outerHTML edits. Remove the element entirely, as the selector
    // comment always claimed. (querySelectorAll("*") does not descend into
    // template.content either.)
    if (node.tagName === "TEMPLATE") {
      node.remove();
      continue;
    }
    while (node.firstChild) node.removeChild(node.firstChild);
  }
  for (const element of [clone, ...clone.querySelectorAll("*")]) {
    for (const name of [...element.getAttributeNames()]) {
      if (name === OWNERSHIP_NONCE_ATTRIBUTE) {
        element.setAttribute(name, "[REDACTED]");
      } else if (!attributeIsSafe(name, element.getAttribute(name) ?? "")) {
        element.removeAttribute(name);
      }
    }
    if (element.matches?.(STRIP_VALUE_SELECTOR)) {
      element.removeAttribute("value");
      // yz-beb: .value is the live value; serialization renders the CHILD
      // TEXT of a textarea (its default value). Clear both.
      if (element.tagName === "TEXTAREA") {
        while (element.firstChild) element.removeChild(element.firstChild);
      }
      try {
        element.value = "";
      } catch {
        // A control without a settable value still loses its attribute above.
      }
    }
  }
  return clone;
}

// Final guard (d): replace any surviving JWT-shaped string in the serialized
// markup and report how many replacements were made so the CLI can surface
// the redaction count.
export function redactSecrets(html) {
  let redactions = 0;
  const redacted = html.replace(JWT_SHAPE, () => {
    redactions++;
    return REDACTED_JWT;
  });
  return { html: redacted, redactions };
}

// conversation-serializer.js — shared ChatGPT conversation-container
// serializer for the dump-conversation capture command (yz-7iu).
//
// Sibling of picker-serializer.js: clones the main conversation container,
// bakes computed inert/display:none/visibility:hidden state inline, and
// returns sanitized outerHTML. Read-only: it never mutates the live page
// (all edits happen on the clone).
//
// Sanitization lives in capture-sanitizer.js and is shared with the picker
// serializer: a conversation dump serializes <main> (or body on a degraded
// page) and can carry session secrets — hidden input values, template
// fragments, style blocks, data-* token attributes, JWT-shaped strings.
// The output is both an operator recovery artifact (a rendered answer on a
// preserved tab that the extractor under-reads) and an extractor-drift
// fixture: the same page is reported through the current extractor next to
// the raw innerText length so the two can be compared.

import { redactSecrets, sanitizeCaptureClone } from "./capture-sanitizer.js";

export function serializeConversation(root = document) {
  // The conversation surface: ChatGPT renders the transcript inside <main>
  // (falling back to the article container, then body so a degraded page
  // still produces a capture instead of throwing).
  const live = root.querySelector("main")
    || root.querySelector("article")
    || root.body;
  if (!live) {
    throw new Error("no conversation container found in the page");
  }
  const clone = live.cloneNode(true);

  function effectivelyInert(el) {
    for (let node = el; node && node.nodeType === 1; node = node.parentElement) {
      if (node.hasAttribute && node.hasAttribute("inert")) return true;
    }
    return false;
  }

  function computedStyleFor(el) {
    const view = el?.ownerDocument?.defaultView;
    return view && typeof view.getComputedStyle === "function"
      ? view.getComputedStyle(el)
      : null;
  }

  // Walk live and clone in parallel (same tree order) to copy live state onto
  // the clone: computed inert as an attribute, data-state/aria-* verbatim, and
  // computed display/visibility written inline when none/hidden. Identical
  // treatment to picker-serializer.js so the two fixtures stay comparable.
  function sync(liveEl, cloneEl) {
    if (!liveEl || !cloneEl || cloneEl.nodeType !== 1) return;
    if (effectivelyInert(liveEl)) cloneEl.setAttribute("inert", "");
    else cloneEl.removeAttribute("inert");
    if (liveEl.hasAttribute("data-state")) {
      cloneEl.setAttribute("data-state", liveEl.getAttribute("data-state"));
    }
    const ariaNames = [];
    for (const attr of liveEl.attributes) {
      if (attr.name === "data-state" || attr.name.startsWith("aria-")) {
        ariaNames.push(attr.name);
      }
    }
    for (const name of ariaNames) {
      cloneEl.setAttribute(name, liveEl.getAttribute(name));
    }
    const computed = computedStyleFor(liveEl);
    if (computed) {
      if (computed.display === "none") cloneEl.style.setProperty("display", "none", "important");
      if (computed.visibility === "hidden") cloneEl.style.setProperty("visibility", "hidden", "important");
    }
    const liveKids = liveEl.children;
    const cloneKids = cloneEl.children;
    let cloneIndex = 0;
    for (let liveIndex = 0; liveIndex < liveKids.length && cloneIndex < cloneKids.length; liveIndex++) {
      sync(liveKids[liveIndex], cloneKids[cloneIndex]);
      cloneIndex++;
    }
  }
  sync(live, clone);

  // Shared redaction pass (see capture-sanitizer.js): strip sensitive bodies,
  // secret-shaped attribute values, and form-control values, then replace any
  // surviving JWT-shaped string and report the count.
  sanitizeCaptureClone(clone);
  const { html, redactions } = redactSecrets(clone.outerHTML);
  serializeConversation.lastRedactions = redactions;
  return html;
}

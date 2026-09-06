// picker-serializer.js — shared ChatGPT model-picker menu serializer.
//
// Clones the open [role="menu"] (falling back to the first [role="menu"]),
// bakes computed inert/display:none/visibility:hidden state inline, strips
// script/svg/use/canvas bodies, and returns outerHTML. The output is a
// snapshot fixture consumed by tests/chatgpt-picker-reader.test.js (jsdom) —
// never by the extension, the native host, or inspect_run.
//
// One serializer, two callers:
//   - scripts/capture-chatgpt-picker.mjs (raw CDP Runtime.evaluate)
//   - src/content-script.js dump_picker_html (native-messaging channel)
//
// Computed styles are baked in because jsdom has no layout engine — the
// reader's attribute+inline-style visibility predicate cannot see
// stylesheet-driven hiding. See docs/design/chatgpt-picker-reader.md,
// "Snapshot fixtures replace hand-built fakes" and the "jsdom boundary".

export function serializePickerMenu(root = document) {
  const live = root.querySelector('[role="menu"][data-state="open"]')
    || root.querySelector('[role="menu"]');
  if (!live) {
    throw new Error('no [role="menu"] found in the page');
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
  // computed display/visibility written inline when none/hidden.
  function sync(liveEl, cloneEl) {
    if (!liveEl || !cloneEl || cloneEl.nodeType !== 1) return;
    if (effectivelyInert(liveEl)) cloneEl.setAttribute("inert", "");
    else cloneEl.removeAttribute("inert");
    // data-state and aria-* are already attributes on the clone (it was cloned
    // from live), but re-copy to guarantee they survive any later mutation.
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
    // Bake computed display/visibility inline so jsdom's attribute+inline-style
    // readability predicate sees the same hidden state Chrome does. Only write
    // when the computed value hides the node; never overwrite an existing
    // inline value that already expresses the same intent.
    const computed = computedStyleFor(liveEl);
    if (computed) {
      if (computed.display === "none") cloneEl.style.setProperty("display", "none", "important");
      if (computed.visibility === "hidden") cloneEl.style.setProperty("visibility", "hidden", "important");
    }
    const liveKids = liveEl.children;
    const cloneKids = cloneEl.children;
    let cloneIndex = 0;
    for (let liveIndex = 0; liveIndex < liveKids.length && cloneIndex < cloneKids.length; liveIndex++) {
      // Index parity holds only because cloneNode(true) preserves child order,
      // so liveKids[i] corresponds to cloneKids[i] one-to-one.
      sync(liveKids[liveIndex], cloneKids[cloneIndex]);
      cloneIndex++;
    }
  }
  sync(live, clone);

  // Strip the bodies of <script>, <svg>, <use>, <canvas> in the clone. The
  // element tag is kept so tree structure (and thus selector parity with live)
  // is preserved; only their heavy/sensitive content is removed.
  const strip = clone.querySelectorAll("script, svg, use, canvas");
  for (const node of strip) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  return clone.outerHTML;
}

#!/usr/bin/env node

// DevTools helper for collecting small, structural Gemini web fixtures.
// Keep this function self-contained so the CLI can print a complete snippet.
function captureGeminiSurface(root, surface) {
  if (location.origin !== "https://gemini.google.com" || window.top !== window) {
    throw new Error("Use the top-level Gemini web tab at https://gemini.google.com.");
  }

  const pageRegions =
    "html, body, main, nav, aside, header, footer, article, [role=main], [role=log], [role=feed], [role=navigation]";
  const omitted =
    "script, style, template, noscript, iframe, object, embed, svg, img, picture, canvas, video, audio, input[type=hidden]";
  if (!(root instanceof Element) || root.ownerDocument !== document || !root.isConnected
      || root.matches(pageRegions) || root.matches(omitted)) {
    throw new Error("Select the narrow chooser or empty composer element in DevTools Elements first.");
  }

  const tags = new Set([
    "div", "span", "p", "button", "input", "textarea", "label", "fieldset", "legend",
    "ul", "ol", "li", "section", "form"
  ]);
  const booleanValues = ["true", "false"];
  const attributes = {
    role: [
      "button", "checkbox", "combobox", "dialog", "group", "listbox", "listitem",
      "menu", "menuitem", "menuitemcheckbox", "menuitemradio", "none", "option",
      "presentation", "radio", "radiogroup", "separator", "textbox", "toolbar"
    ],
    type: ["button", "checkbox", "file", "radio", "reset", "search", "submit", "text"],
    contenteditable: ["", "true", "false", "plaintext-only"],
    "aria-checked": [...booleanValues, "mixed"],
    "aria-pressed": [...booleanValues, "mixed"],
    "aria-expanded": booleanValues,
    "aria-selected": booleanValues,
    "aria-disabled": booleanValues,
    "aria-hidden": booleanValues,
    "aria-multiline": booleanValues,
    "aria-haspopup": [...booleanValues, "menu", "listbox", "tree", "grid", "dialog"],
    "data-state": ["open", "closed", "checked", "unchecked", "on", "off"]
  };
  const stateFlags = ["disabled", "hidden", "inert", "readonly"];
  const labelFlags = ["aria-label", "aria-labelledby", "aria-controls", "data-testid", "data-test-id"];
  let visited = 0;

  function project(element, depth) {
    if (++visited > 128 || depth > 12 || element.matches(pageRegions)) {
      throw new Error("Capture scope is too broad. Select a smaller chooser or composer element.");
    }
    if (element.matches(omitted)) return null;

    // Project known structural values. Never serialize the element, its text,
    // input values, arbitrary attributes, IDs, or the current URL.
    const result = { tag: tags.has(element.localName) ? element.localName : "custom" };
    for (const [name, allowed] of Object.entries(attributes)) {
      const value = element.getAttribute(name);
      if (allowed.includes(value)) result[name] = value;
    }
    for (const name of stateFlags) {
      if (element.hasAttribute(name)) result[name] = true;
    }
    const present = labelFlags.filter((name) => element.hasAttribute(name));
    if (present.length) result.attributes_present = present;
    if (element.localName === "input" && ["checkbox", "radio"].includes(element.type)) {
      result.checked = element.checked;
    }
    const children = [];
    for (const child of element.children) {
      const entry = project(child, depth + 1);
      if (entry) children.push(entry);
    }
    if (children.length) result.children = children;
    return result;
  }

  return { surface, root: project(root, 0) };
}

const usage = "Run node scripts/capture-gemini-surface.mjs chooser|composer\n";
const [, , surface, ...extra] = process.argv;
if ((surface === "--help" || surface === "-h") && extra.length === 0) {
  process.stdout.write(usage);
} else if (!["chooser", "composer"].includes(surface) || extra.length) {
  process.stderr.write(usage);
  process.exitCode = 2;
} else {
  const snippet = "copy(JSON.stringify((" + captureGeminiSurface.toString() + ")("
    + "typeof $0 === 'undefined' ? null : $0, " + JSON.stringify(surface) + "), null, 2));\n";
  process.stdout.write(snippet);
}

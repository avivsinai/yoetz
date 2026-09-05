// src/chatgpt-picker-reader.js
//
// The pure ChatGPT picker reader. Owns every DOM heuristic for reading the
// model picker surface and produces one plain value (PickerRead) consumed by
// the driver in chatgpt-dom.js. See docs/design/chatgpt-picker-reader.md.
//
// Purity contract: same DOM -> same value. No awaits, no dispatched events, no
// attribute writes. Never throws; an unrecognized DOM yields shape:null with
// diagnostics populated.
//
// jsdom boundary (docs/design/chatgpt-picker-reader.md "jsdom boundary"):
// jsdom 30 has no layout engine. This reader therefore NEVER calls any
// layout-dependent visibility API (no computed-style, no client-rect, no
// visibility-check method, no driver-level visibility helper). Its readability
// predicate (structurallyReadable) walks ancestors and reads attributes + inline
// style only: hidden, aria-hidden="true", inert, data-state="closed", and inline
// display:none / visibility:hidden. The capture script bakes computed
// display/visibility inline onto fixtures so this predicate sees the same
// hidden state Chrome does.
//
// The reader never locates anything on the page. The driver locates the pill
// (a layout-dependent model-button finder) and any leftover composer triggers
// and PASSES them in via { pill, leftoverTriggers }. The reader imports nothing
// from chatgpt-dom.js; chatgpt-dom.js imports from here.

const CHATGPT_SOL_FAMILY_LABEL = "GPT-5.6 Sol";
// Live probe 2026-09-05 (yz-a8c.1): the Chat family radio checked on a Pro
// account is "Latest" (GPT-5.6 Sol and GPT-5.5 mounted unchecked). "Latest" is
// the selection target; Sol is recognized only to refuse it (never select Sol).
const CHATGPT_TARGET_FAMILY_LABEL = "Latest";

/**
 * @typedef {Object} PickerRead
 * @property {"menu"|"slider"|"personal"|null} shape   // null = no picker surface found
 * @property {Element|null} surface                   // the open picker root, for the driver to act on
 * @property {"aria_controls_structural"|"visible"|null} trust
 * @property {boolean} ready       // finished mounting: family readable AND (effort control | tier rows | disabled ladder)
 * @property {{label:string|null, checked:boolean, options:string[], latestOption:Element|null, checkedCount:number}} family
 * @property {{label:string|null, options:string[], items:Element[], disabled:boolean, disabledReason:string|null, control:Element|null, kind:"slider"|"rows"|"row"|null}} effort

 * @property {{familyTrigger:Element|null, viewToggle:Element|null, expanded:boolean}} nav   // how to reach the family view if it is collapsed
 * @property {Object} diagnostics  // {advanced_rows, effort_control, family_menu_probe} verbatim shapes
 */

// ---------------------------------------------------------------------------
// Text + label helpers (moved from chatgpt-dom.js, unchanged in behavior).
// ---------------------------------------------------------------------------

function normalizeText(value) {
  return String(value ?? "")
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function textOf(node) {
  const inner = normalizeText(node?.innerText ?? "");
  return inner || normalizeText(node?.textContent ?? "");
}

function foldedModelText(value) {
  return normalizeText(value).toLowerCase();
}

function foldedFamilyLabel(value) {
  return foldedModelText(value).replace(/\s+/g, " ").replace(/^gpt[\s-]*/, "");
}

function isFamilyOptionLabel(value) {
  // "Latest" is the GPT-6 Chat family radio (live probe 2026-09-05); without
  // this the reader drops it and reports checked_count=0 on a Latest-checked
  // picker. Sol/5.5 rows stay family options so they are never read as effort.
  return /^gpt\b|^o3$|^latest$/i.test(normalizeText(value));
}

// The unified September 2026 picker labels its effort tier rows through
// aria-label with no text content; label reads must accept either.
function optionLabel(item) {
  return normalizeText(textOf(item)) || normalizeText(item?.getAttribute?.("aria-label") ?? "");
}

function effortOptionDisabled(option) {
  return option?.getAttribute?.("aria-disabled") === "true"
    || option?.getAttribute?.("data-disabled") != null;
}

function itemIsChecked(item) {
  return item?.getAttribute?.("aria-checked") === "true" || item?.getAttribute?.("data-state") === "checked";
}

function familyIsSol(value) {
  return foldedModelText(value) === foldedModelText(CHATGPT_SOL_FAMILY_LABEL);
}

function familyIsLatest(value) {
  return foldedModelText(value) === foldedModelText(CHATGPT_TARGET_FAMILY_LABEL);
}

function descendantDepth(node, ancestor) {
  let depth = 0;
  for (let current = node; current && current !== ancestor; current = current.parentElement) depth += 1;
  return depth;
}

function labeledRowValue(row, label) {
  const text = normalizeText(textOf(row)).replace(/\s+/g, " ");
  const match = text.match(new RegExp(`^${label}\\s+(.+)$`, "i"));
  return normalizeText(match?.[1] ?? "");
}

// ---------------------------------------------------------------------------
// structurallyReadable — the jsdom-safe readability predicate.
//
// Replaces every layout-dependent visibility call that the moved functions
// used to make on picker-surface elements. Walks ancestors to
// `stopAt` (the surface root, or null for the document root) and reads
// attributes + inline style only. Mirrors today's structurallyReadablePickerItem
// minus its computed-style call (which jsdom cannot honor for cascaded
// display:none). The capture script bakes computed display/visibility inline so
// this sees the same hidden state.
// ---------------------------------------------------------------------------

function structurallyReadable(node, stopAt = null) {
  let current = node;
  while (current) {
    if (current.hidden
      || current.getAttribute?.("hidden") != null
      || current.getAttribute?.("aria-hidden") === "true"
      || current.getAttribute?.("inert") != null) {
      return false;
    }
    // Inline style only — jsdom cannot cascade computed display/visibility from
    // an ancestor. The capture script writes these inline when none/hidden.
    // Read the style attribute string (works for jsdom fixtures, real Chrome,
    // and the FakeElement test harness which stores style as an attrs string).
    const styleText = current.getAttribute?.("style") ?? "";
    if (/display\s*:\s*none/i.test(styleText) || /visibility\s*:\s*hidden/i.test(styleText)) {
      return false;
    }
    if (current === stopAt) break;
    current = current.parentElement;
  }
  return true;
}

// The original attribute-only walker (kept verbatim for callers that pass a
// surface to stop at and expect the legacy semantics minus the computed-style call).
// structurallyReadable above is the generalized form; this name is preserved so
// moved call sites that referenced it stay byte-for-byte equivalent.
function structurallyReadablePickerItem(item, surface) {
  return structurallyReadable(item, surface);
}

// Inline-style visibility for the "visible-trust" surface finders. Mirrors the
// original driver visibility check minus layout/computed-style: walks ancestors and reads
// attributes + inline style only, INCLUDING opacity:0 (which demotes a surface
// from "visible" trust to structural trust — invariant 4 says opacity never
// gates WHETHER we read, but the original code used it to pick WHICH trust
// path found the surface). The capture script bakes computed styles inline.
function isVisibleInline(node, stopAt = null) {
  let current = node;
  while (current) {
    if (current.hidden
      || current.getAttribute?.("hidden") != null
      || current.getAttribute?.("aria-hidden") === "true"
      || current.getAttribute?.("inert") != null) {
      return false;
    }
    const styleText = current.getAttribute?.("style") ?? "";
    if (/display\s*:\s*none/i.test(styleText)
      || /visibility\s*:\s*hidden/i.test(styleText)
      || /opacity\s*:\s*0(?:\.0+)?\b/i.test(styleText)) {
      return false;
    }
    if (current === stopAt) break;
    current = current.parentElement;
  }
  return true;
}

// ---------------------------------------------------------------------------
// Trigger / surface open predicates (moved, unchanged).
// ---------------------------------------------------------------------------

function modelPickerTriggerIsOpen(modelButton) {
  return modelButton?.getAttribute?.("aria-expanded") === "true"
    || modelButton?.getAttribute?.("data-state") === "open";
}

function pickerSurfaceIsOpen(node, stopAt = null) {
  let current = node;
  while (current) {
    const state = current.getAttribute?.("data-state");
    if (state === "closed") return false;
    if (current === stopAt) break;
    current = current.parentElement;
  }
  return true;
}

function personalPickerSurfaceIsOpen(node) {
  const state = node?.getAttribute?.("data-state");
  if (state === "open") return true;
  if (state === "closed") return false;
  return Array.from(node.querySelectorAll?.('[role="menu"], [role="dialog"]') ?? [])
    .some((child) => child.getAttribute?.("data-state") === "open");
}

// ---------------------------------------------------------------------------
// Slider shape helpers (moved, unchanged).
// ---------------------------------------------------------------------------

function sliderLooksLikePowerControl(slider, surface) {
  const direct = normalizeText([
    slider?.getAttribute?.("aria-label"),
    slider?.getAttribute?.("title"),
    textOf(slider)
  ].filter(Boolean).join(" "));
  if (/\b(?:faster|smarter|speed|instant)\b/i.test(direct)) return true;
  let ancestor = slider?.parentElement;
  for (let depth = 0; ancestor && ancestor !== surface && depth < 4; depth += 1, ancestor = ancestor.parentElement) {
    const text = normalizeText(textOf(ancestor));
    if (/\beffort\b/i.test(text)) return false;
    if (/\bspeed\b/i.test(text) || (/\bfaster\b/i.test(text) && /\bsmarter\b/i.test(text))) return true;
  }
  return false;
}

function effortLabelNearSlider(slider, surface) {
  let scope = slider?.parentElement;
  for (let depth = 0; scope && depth < 8; depth += 1, scope = scope.parentElement) {
    const label = Array.from(scope.querySelectorAll?.("span, div") ?? [])
      .map((node) => normalizeText(textOf(node)))
      .find((text) => /^[A-Z][A-Za-z ]{1,24},\s*\d+\s+of\s+\d+\s*\.?$/.test(text));
    if (label) return label;
    if (scope === surface) break;
  }
  return "";
}

function sliderEffortSnapshot(slider, surface = null) {
  if (!slider) return null;
  const valueText = normalizeText(slider.getAttribute?.("aria-valuetext") ?? "");
  const nearbyLabel = valueText || effortLabelNearSlider(slider, surface);
  const match = nearbyLabel.match(/^([A-Z][A-Za-z ]{1,24}),\s*(\d+)\s+of\s+(\d+)\s*\.?\s*$/);
  const now = Number(slider.getAttribute?.("aria-valuenow"));
  const min = Number(slider.getAttribute?.("aria-valuemin"));
  const max = Number(slider.getAttribute?.("aria-valuemax"));
  const ordinal = now - min + 1;
  const total = max - min + 1;
  if (!match || !Number.isFinite(now) || !Number.isFinite(min) || !Number.isFinite(max)
    || max <= min || now < min || now > max || Number(match[2]) !== ordinal || Number(match[3]) !== total) {
    return null;
  }
  // "Instant, n of m." is the speed control on the September 2026 list
  // picker, not an effort tier; it must never be read as the effort slider.
  if (foldedModelText(match[1]) === "instant") return null;
  return { label: foldedModelText(match[1]), display_label: match[1], now, min, max, value_text: nearbyLabel };
}

function sliderIsEffortControl(slider, surface) {
  const directLabel = normalizeText([
    slider?.getAttribute?.("aria-label"),
    slider?.getAttribute?.("title")
  ].filter(Boolean).join(" "));
  if (/\b(?:faster|smarter)\b/i.test(directLabel) || sliderLooksLikePowerControl(slider, surface)) return false;
  if (/\beffort\b/i.test(directLabel)) return true;
  if (sliderEffortSnapshot(slider, surface)) return true;

  const labelledBy = normalizeText(slider?.getAttribute?.("aria-labelledby") ?? "")
    .split(" ")
    .map((id) => slider?.ownerDocument?.getElementById?.(id))
    .filter(Boolean)
    .map((node) => textOf(node))
    .join(" ");
  if (/\beffort\b/i.test(labelledBy)) return true;

  let ancestor = slider?.parentElement;
  while (ancestor && ancestor !== surface) {
    const text = normalizeText(textOf(ancestor));
    if (/\beffort\b/i.test(text) && !/\b(?:faster|smarter)\b/i.test(text)) return true;
    ancestor = ancestor.parentElement;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Family evidence (moved, unchanged).
// ---------------------------------------------------------------------------

function structuralFamilyEvidence(control) {
  const empty = { label: "", labels: [], source: null, ambiguous: false };
  if (!control || control.getAttribute?.("aria-haspopup") !== "menu") return empty;
  if (!/\bModel\b/i.test(textOf(control))) return empty;
  const matches = Array.from(control.querySelectorAll?.("*") ?? [])
    .map((node) => ({ node, label: normalizeText(textOf(node)) }))
    .filter(({ label }) => /^(?:gpt|o\d|latest)\b/i.test(label));
  const labelsByFold = new Map();
  for (const match of matches) {
    const folded = foldedModelText(match.label);
    if (!labelsByFold.has(folded)) labelsByFold.set(folded, match.label);
  }
  const labels = [...labelsByFold.values()];
  if (labels.length > 1) {
    return { label: "", labels, source: null, ambiguous: true };
  }
  if (matches.length === 0) return empty;
  const explicit = matches.find(({ node }) => {
    const ariaCurrent = normalizeText(node.getAttribute?.("aria-current") ?? "").toLowerCase();
    return (ariaCurrent && ariaCurrent !== "false") || node.getAttribute?.("data-state") === "checked";
  });
  const selected = explicit ?? matches.reduce((deepest, match) => (
    descendantDepth(match.node, control) >= descendantDepth(deepest.node, control) ? match : deepest
  ));
  const source = explicit
    ? (selected.node.getAttribute?.("data-state") === "checked" ? "data_state_checked" : "aria_current")
    : "deepest_unique";
  return { label: selected.label, labels, source, ambiguous: false };
}

// ---------------------------------------------------------------------------
// Menu item + radio collectors (moved; layout-dependent visibility calls replaced by
// structurallyReadable).
// ---------------------------------------------------------------------------

function menuRadioItems(menu, structurallyTrusted = false) {
  return Array.from(menu?.querySelectorAll?.('[role="menuitemradio"]') ?? [])
    .filter((item) => pickerSurfaceIsOpen(item, menu)
      && (structurallyReadable(item, menu) || (structurallyTrusted && structurallyReadablePickerItem(item, menu))));
}

function familyMenuRadios(menu, structurallyTrusted = false) {
  return menuRadioItems(menu, structurallyTrusted).filter((item) => isFamilyOptionLabel(textOf(item)));
}

// When the account's effort quota is exhausted, ChatGPT keeps the tier rows
// mounted but disables them (aria-disabled + data-disabled) — no selection
// path exists until the limit resets. Detect that state so the failure names
// the real cause instead of a generic open/move failure.
function disabledProEffortOption(surface) {
  const pro = Array.from(surface?.querySelectorAll?.('[role="menuitemradio"], [role="menuitem"]') ?? [])
    .find((item) => !isFamilyOptionLabel(optionLabel(item))
      && foldedModelText(optionLabel(item)).replace(/\s+/g, " ") === "pro");
  if (!pro || !effortOptionDisabled(pro)) return null;
  const describedBy = pro.getAttribute?.("aria-describedby");
  const described = describedBy
    ? normalizeText(textOf(pro.ownerDocument?.getElementById?.(describedBy)))
    : "";
  return {
    option: pro,
    reason: described || normalizeText(pro.getAttribute?.("title") ?? "") || null
  };
}

// ---------------------------------------------------------------------------
// "Select model" view toggle (moved, unchanged).
// ---------------------------------------------------------------------------

function isSelectModelViewToggle(node) {
  return node?.getAttribute?.("role") === "menuitem"
    && normalizeText(node.getAttribute?.("aria-label") ?? "").toLowerCase() === "select model";
}

// The hybrid picker keeps the family radios mounted but collapsed: the
// advanced view carries `inert` plus an opacity-0 wrapper whose reveal
// animation is rAF-driven and never settles in background tabs. The trigger's
// aria-expanded="true" combined with the view shedding `inert` is the
// structural open signal; opacity must not gate the read.
function expandedSelectModelView(trigger, view) {
  return isSelectModelViewToggle(trigger)
    && trigger?.getAttribute?.("aria-expanded") === "true"
    && Boolean(view)
    && structurallyReadablePickerItem(view, view);
}

function activeFamilyView(root, mainMenu, trigger, pill, leftoverTriggers) {
  if (!isSelectModelViewToggle(trigger)) return null;
  const surface = mainMenu ?? findPickerState(root, { pill, leftoverTriggers })?.surface;
  const advancedViews = Array.from(surface?.querySelectorAll?.(
    '[data-testid="composer-model-picker-slider-advanced-view"]'
  ) ?? []);
  const activeAdvancedView = advancedViews.find((view) => familyMenuRadios(view, true).length > 0);
  if (activeAdvancedView) return activeAdvancedView;
  return familyMenuRadios(surface, true).length > 0 ? surface : null;
}

// ---------------------------------------------------------------------------
// Surface finders (moved; layout-dependent visibility calls replaced by structurallyReadable).
// ---------------------------------------------------------------------------

function visibleMenus(root) {
  return Array.from(root.querySelectorAll('[role="menu"]'))
    .filter((menu) => isVisibleInline(menu, null) && pickerSurfaceIsOpen(menu));
}

function findMainModelMenu(root) {
  return visibleMenus(root).find((menu) => isMainModelMenu(menu)) ?? null;
}

function isEffortMenuLabels(labels) {
  return labels.includes("medium") && labels.includes("high") && labels.includes("pro");
}

function isMainModelMenu(menu) {
  const labels = menuRadioItems(menu).map((item) => foldedModelText(optionLabel(item)));
  return isEffortMenuLabels(labels);
}

function findFamilySubmenu(root, mainMenu) {
  return visibleMenus(root).find((menu) => menu !== mainMenu && familyMenuRadios(menu).some((item) => familyIsLatest(textOf(item)))) ?? null;
}

function looksLikeLegacyAdvancedPicker(surface) {
  const text = normalizeText(textOf(surface));
  return /\bAdvanced\b/i.test(text)
    && /\bEffort\b/i.test(text)
    && (surface.querySelectorAll?.('[role="slider"]')?.length ?? 0) > 0;
}

function looksLikePersonalPicker(node) {
  const text = normalizeText(textOf(node));
  return /\bFaster\b/i.test(text)
    && /\bSmarter\b/i.test(text)
    && /\bModel\b/i.test(text)
    && /\bEffort\b/i.test(text)
    && /\bSpeed\b/i.test(text)
    && !/\bAdvanced\b/i.test(text);
}

function findPersonalPickerSurface(root) {
  const candidates = Array.from(root.querySelectorAll('div, [role="menu"], [role="dialog"]'))
    .filter((node) => pickerSurfaceIsOpen(node)
      && personalPickerSurfaceIsOpen(node)
      && isVisibleInline(node, null) && looksLikePersonalPicker(node));
  return candidates.sort((left, right) => (
    left.querySelectorAll?.("*")?.length ?? 0
  ) - (
    right.querySelectorAll?.("*")?.length ?? 0
  ))[0] ?? null;
}

function surfaceHasParsableEffortSlider(surface) {
  return Array.from(surface?.querySelectorAll?.('[role="slider"]') ?? [])
    .some((slider) => !sliderLooksLikePowerControl(slider, surface) && sliderEffortSnapshot(slider, surface));
}

function hasSelectModelViewToggle(surface) {
  return Array.from(surface?.querySelectorAll?.('[role="menuitem"]') ?? [])
    .some((item) => isSelectModelViewToggle(item)
      && (isVisibleInline(item, surface) || structurallyReadablePickerItem(item, surface)));
}

function findAdvancedPickerSurface(root) {
  const candidates = Array.from(root.querySelectorAll('div, [role="dialog"]'))
    .filter((node) => {
      if (!isVisibleInline(node, null) || !pickerSurfaceIsOpen(node)) return false;
      const text = normalizeText(textOf(node));
      return /\bAdvanced\b/i.test(text)
        && /\bFaster\b/i.test(text)
        && /\bSmarter\b/i.test(text)
        && /\bModel\b/i.test(text)
        && /\bEffort\b/i.test(text)
        && /\b(?:GPT|o\d|Latest)\b/i.test(text)
        && Array.from(node.querySelectorAll?.('[role="slider"]') ?? [])
          .some((slider) => isVisibleInline(slider, node) || Boolean(sliderEffortSnapshot(slider, node)));
    });
  return candidates.sort((left, right) => (
    left.querySelectorAll?.("*")?.length ?? 0
  ) - (
    right.querySelectorAll?.("*")?.length ?? 0
  ))[0] ?? null;
}

// The hybrid picker with its family view already expanded: inline family
// radios alongside the (possibly degenerate) slider or the advanced-view
// container. A bare family submenu (menu-shape or personal-shape pickers)
// carries neither hallmark and must not classify as a slider surface.
function hybridFamilyView(surface, structurallyTrusted = false) {
  if (familyMenuRadios(surface, structurallyTrusted).length === 0) return false;
  // A bare [role=slider] is not a hallmark: the personal picker's Faster/
  // Smarter power slider would qualify and then be dragged as "effort".
  return surfaceHasParsableEffortSlider(surface)
    || Array.from(surface.querySelectorAll?.("*") ?? [])
      .some((node) => node.getAttribute?.("data-testid") === "composer-model-picker-slider-advanced-view");
}

function findSliderPickerSurface(root) {
  const advanced = findAdvancedPickerSurface(root);
  if (advanced) return advanced;
  // Anchors for the hybrid slider shape, in order of specificity: a parsable
  // effort slider, the collapsed "Select model" toggle, or — once that view
  // is expanded and the simple view has gone inert — the inline family radios
  // themselves.
  return visibleMenus(root).find((menu) => (
    (surfaceHasParsableEffortSlider(menu)
      || hasSelectModelViewToggle(menu)
      || hybridFamilyView(menu, false))
      && !looksLikePersonalPicker(menu)
  )) ?? null;
}

// ---------------------------------------------------------------------------
// Structurally-trusted controlled surface (moved; the trigger is now an INPUT
// passed by the driver, not located here).
// ---------------------------------------------------------------------------

function structurallyOpenControlledSurfaceForTrigger(root, trigger) {
  if (!modelPickerTriggerIsOpen(trigger)) return null;
  const controlledId = trigger?.getAttribute?.("aria-controls");
  if (!controlledId) return null;
  const surface = root.getElementById?.(controlledId);
  if (!surface) return null;
  if (!pickerSurfaceIsOpen(surface)) return null;
  if (surface.getAttribute?.("data-state") === "open") return surface;
  const openChild = Array.from(surface.querySelectorAll?.('[role="menu"], [role="dialog"]') ?? [])
    .find((node) => pickerSurfaceIsOpen(node) && node.getAttribute?.("data-state") === "open");
  return openChild ? surface : null;
}

// ---------------------------------------------------------------------------
// State readers (moved; layout-dependent visibility / computed-style calls replaced by
// structurallyReadable).
// ---------------------------------------------------------------------------

function readMenuPickerState(menu, structurallyTrusted = false) {
  // The September 2026 picker can put family radios (Latest / GPT-5.6 Sol /
  // GPT-5.5) inline next to the effort tiers in one menu; effort verification
  // must only look at the non-family radios.
  const effortItems = menuRadioItems(menu, structurallyTrusted)
    .filter((item) => !isFamilyOptionLabel(optionLabel(item)));
  const familyTrigger = Array.from(menu.querySelectorAll('[role="menuitem"]'))
    .find((item) => {
      const label = normalizeText(textOf(item));
      return item.getAttribute?.("aria-haspopup") === "menu"
        && /^(?:gpt|o\d|latest)\b/i.test(label);
    });
  return {
    shape: "menu",
    menu,
    surface: menu,
    family_trigger: familyTrigger ?? null,
    family_label: textOf(familyTrigger),
    effort_items: effortItems,
    effort_slider: null,
    effort_move_method: null,
    surface_trust: structurallyTrusted ? "aria_controls_structural" : "visible"
  };
}

function readSliderPickerState(root, structurallyTrustedSurface = null) {
  const surface = structurallyTrustedSurface ?? findSliderPickerSurface(root);
  if (!surface) return null;
  const structurallyTrusted = Boolean(structurallyTrustedSurface);
  const inlineFamily = familyMenuRadios(surface, true);
  if (inlineFamily.length > 0) {
    const checked = inlineFamily.find((item) => itemIsChecked(item));
    const effortSlider = Array.from(surface.querySelectorAll('[role="slider"]'))
      .find((slider) => sliderIsEffortControl(slider, surface) && Boolean(sliderEffortSnapshot(slider, surface))) ?? null;
    return {
      shape: "slider",
      menu: surface.getAttribute?.("role") === "menu" ? surface : null,
      surface,
      family_trigger: null,
      family_label: checked ? textOf(checked) : "",
      family_label_candidates: inlineFamily.map((item) => textOf(item)).filter(Boolean),
      family_label_source: checked ? "inline_family_radio" : null,
      family_label_ambiguous: false,
      effort_items: [],
      effort_slider: effortSlider,
      effort_move_method: null,
      surface_trust: structurallyTrusted ? "aria_controls_structural" : "visible"
    };
  }
  let familyEvidence = null;
  const familyTrigger = Array.from(surface.querySelectorAll('[role="menuitem"], button'))
    .find((item) => isSelectModelViewToggle(item)
      && (structurallyReadable(item, surface)
        || structurallyReadablePickerItem(item, surface)))
    ?? Array.from(surface.querySelectorAll('[role="menuitem"], button'))
    .find((item) => {
      const evidence = structurallyTrusted ? structuralFamilyEvidence(item) : null;
      const label = evidence?.label ?? normalizeText(textOf(item));
      if (evidence?.label || evidence?.ambiguous) familyEvidence = evidence;
      if (item.getAttribute?.("aria-haspopup") === "menu" && /\bModel\b/i.test(textOf(item))) {
        return structurallyTrusted || structurallyReadable(item, surface);
      }
      return (evidence?.ambiguous || (Boolean(label) && /^(?:gpt|o\d|latest)\b/i.test(label)))
        && (structurallyTrusted || structurallyReadable(item, surface));
    });
  const effortSlider = Array.from(surface.querySelectorAll('[role="slider"]'))
    .filter((slider) => structurallyTrusted || structurallyReadable(slider, surface) || Boolean(sliderEffortSnapshot(slider, surface)))
    .find((slider) => sliderIsEffortControl(slider, surface) && Boolean(sliderEffortSnapshot(slider, surface))) ?? null;
  return {
    shape: "slider",
    menu: null,
    surface,
    family_trigger: familyTrigger ?? null,
    family_label: structurallyTrusted ? familyEvidence?.label ?? "" : textOf(familyTrigger),
    family_label_candidates: structurallyTrusted ? familyEvidence?.labels ?? [] : [],
    family_label_source: structurallyTrusted ? familyEvidence?.source ?? null : null,
    family_label_ambiguous: structurallyTrusted ? familyEvidence?.ambiguous ?? false : false,
    effort_items: [],
    effort_slider: effortSlider,
    effort_move_method: null,
    surface_trust: structurallyTrusted ? "aria_controls_structural" : "visible"
  };
}

function readPersonalPickerState(surface, structurallyTrusted = false) {
  const controls = Array.from(surface.querySelectorAll?.('[role="menuitem"], button') ?? []);
  let familyEvidence = null;
  const familyTrigger = controls.find((item) => {
    const evidence = structuralFamilyEvidence(item);
    if (evidence.label || evidence.ambiguous) familyEvidence = evidence;
    return Boolean(evidence.label || evidence.ambiguous);
  }) ?? controls.find((item) => /\bModel\b/i.test(textOf(item)) && item.getAttribute?.("aria-haspopup") === "menu") ?? null;
  const effortRow = controls.find((item) => /\bEffort\b/i.test(textOf(item))) ?? null;
  const effortLabel = labeledRowValue(effortRow, "Effort");
  const familyLabel = familyEvidence?.label || labeledRowValue(familyTrigger, "Model");
  if (!familyTrigger || !effortRow || !effortLabel || (!familyLabel && !familyEvidence?.ambiguous)) return null;
  return {
    shape: "personal",
    menu: null,
    surface,
    family_trigger: familyTrigger,
    family_label: familyLabel,
    family_label_candidates: familyEvidence?.labels ?? (familyLabel ? [familyLabel] : []),
    family_label_source: familyEvidence?.source ?? (familyLabel ? "labeled_row" : null),
    family_label_ambiguous: familyEvidence?.ambiguous ?? false,
    effort_row: effortRow,
    effort_label: effortLabel,
    effort_items: [],
    effort_slider: null,
    effort_move_method: null,
    surface_trust: structurallyTrusted ? "aria_controls_structural" : "visible"
  };
}

// ---------------------------------------------------------------------------
// Classification + readiness (moved; layout-dependent visibility / computed-style calls replaced
// by structurallyReadable).
// ---------------------------------------------------------------------------

function readStructurallyTrustedPickerState(surface) {
  if (!pickerSurfaceIsOpen(surface)) return null;
  const direct = classifyPickerSurface(surface, true);
  if (direct) return direct;
  for (const nested of Array.from(surface.querySelectorAll?.('[role="menu"], [role="dialog"]') ?? [])
    .filter((node) => pickerSurfaceIsOpen(node))) {
    const classified = classifyPickerSurface(nested, true);
    if (classified) return classified;
  }
  return null;
}

function classifyPickerSurface(surface, structurallyTrusted) {
  if (!surface) return null;
  const labels = menuRadioItems(surface, structurallyTrusted).map((item) => foldedModelText(optionLabel(item)));
  if (isEffortMenuLabels(labels)) {
    return readMenuPickerState(surface, structurallyTrusted);
  }
  if (looksLikePersonalPicker(surface)) {
    return readPersonalPickerState(surface, structurallyTrusted);
  }
  if (surfaceHasParsableEffortSlider(surface)
    || looksLikeLegacyAdvancedPicker(surface)
    || hasSelectModelViewToggle(surface)
    || hybridFamilyView(surface, structurallyTrusted)) {
    return readSliderPickerState(surface.ownerDocument ?? surface, structurallyTrusted ? surface : null);
  }
  return null;
}

function surfaceHasEffortRows(surface) {
  return menuRadioItems(surface, true).some((item) => !isFamilyOptionLabel(optionLabel(item)))
    || Boolean(disabledProEffortOption(surface));
}

function pickerStateIsReady(state) {
  const surface = state?.surface ?? state?.menu;
  if (!surface) return false;
  if (state?.shape === "slider") {
    const familyItems = familyMenuRadios(surface, state.surface_trust === "aria_controls_structural");
    const familyTrigger = state.family_trigger
      && (structurallyReadable(state.family_trigger, surface)
        || (state.surface_trust === "aria_controls_structural"
          && structurallyReadablePickerItem(state.family_trigger, surface)))
      ? state.family_trigger
      : null;
    if (familyItems.length === 0 && !familyTrigger) return false;
    // A family view can mount before its effort controls; without an effort
    // slider, tier rows, or the disabled ladder the surface is still
    // hydrating and must keep the open/retry budget alive.
    if (!state.effort_slider && !surfaceHasEffortRows(surface)) return false;
  }
  // jsdom boundary: the original read computed style opacity === "0" to gate
  // an opacity-0 wrapper. jsdom has no layout/computed opacity, and invariant
  // 4 says opacity never gates a read. structurally-trusted surfaces are
  // readable by content; non-trusted surfaces with no surface_trust fall back
  // to the family evidence below.
  if (state?.surface_trust !== "aria_controls_structural") return true;
  const familyItems = familyMenuRadios(surface, true);
  const familyTrigger = state.family_trigger
    && structurallyReadablePickerItem(state.family_trigger, surface)
    ? state.family_trigger
    : null;
  return familyItems.length > 0 || Boolean(familyTrigger);
}

// ---------------------------------------------------------------------------
// findPickerState (moved; the locator calls are replaced by the passed-in
// { pill, leftoverTriggers } inputs from the driver).
// ---------------------------------------------------------------------------

function findPickerState(root, { pill = null, leftoverTriggers = [] } = {}) {
  const menu = findMainModelMenu(root);
  if (menu) return readMenuPickerState(menu, false);
  const slider = readSliderPickerState(root);
  if (slider) return slider;
  const personal = findPersonalPickerSurface(root);
  if (personal) return readPersonalPickerState(personal, false);
  // The controlled-surface branch uses the pill the driver located (the
  // layout-dependent locator stays in chatgpt-dom.js). If no pill was passed,
  // this branch is skipped.
  if (pill) {
    const controlledSurface = structurallyOpenControlledSurfaceForTrigger(root, pill);
    if (controlledSurface) {
      const classified = readStructurallyTrustedPickerState(controlledSurface);
      if (classified) return classified;
    }
  }
  // The leftovers branch iterates the composer triggers the driver located.
  for (const trigger of leftoverTriggers) {
    const surface = structurallyOpenControlledSurfaceForTrigger(root, trigger);
    if (!surface) continue;
    const classified = readStructurallyTrustedPickerState(surface);
    if (classified) return classified;
  }
  // Last resort: a menu that mounted open with real picker content but whose
  // pill wiring (aria-controls) or CSS visibility has not settled — common in
  // a throttled hidden tab mid-open-animation. Trust it structurally by its
  // content, not by the pill or by opacity.
  const mountedMenus = Array.from(root.querySelectorAll?.('[role="menu"]') ?? [])
    .filter((menu) => menu.getAttribute?.("data-state") !== "closed"
      && pickerSurfaceIsOpen(menu)
      && structurallyReadablePickerItem(menu, null)
      && (hasSelectModelViewToggle(menu)
        || familyMenuRadios(menu, true).length > 0
        || menuRadioItems(menu, true).length > 0));
  for (const menu of mountedMenus) {
    const classified = readStructurallyTrustedPickerState(menu);
    if (classified) return classified;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Diagnostics builders (moved from chatgpt-dom.js; pure, structure/text only).
// ---------------------------------------------------------------------------

function advancedViewRows(surface) {
  const advanced = Array.from(surface?.querySelectorAll?.("*") ?? [])
    .find((node) => node.getAttribute?.("data-testid") === "composer-model-picker-slider-advanced-view");
  return Array.from(advanced?.querySelectorAll?.("*") ?? [])
    .filter((row) => ["menuitem", "menuitemcheckbox", "menuitemradio"].includes(row.getAttribute?.("role")))
    .slice(0, 20)
    .map((row) => {
      const parts = Array.from(row.querySelectorAll?.("span, div") ?? [])
        .map((node) => normalizeText(textOf(node)))
        .filter((text, index, all) => text && all.indexOf(text) === index)
        .slice(0, 2);
      return {
        role: row.getAttribute?.("role") ?? null,
        checked: row.getAttribute?.("aria-checked") ?? null,
        disabled: effortOptionDisabled(row),
        label: row.getAttribute?.("aria-label") ?? parts[0] ?? normalizeText(textOf(row)),
        value: parts[1] ?? null
      };
    });
}

function sliderEffortDiagnostics(slider, surface = null) {
  const snapshot = sliderEffortSnapshot(slider, surface);
  return snapshot ? {
    role: "slider",
    label: snapshot.label,
    value_text: snapshot.value_text,
    value_now: snapshot.now,
    value_min: snapshot.min,
    value_max: snapshot.max
  } : null;
}

function personalEffortDiagnostics(state) {
  if (!state?.effort_label) return null;
  return {
    role: "menuitem",
    label: foldedModelText(state.effort_label).replace(/\s+/g, " "),
    value_text: state.effort_label
  };
}

function effortControlDiagnostics(state) {
  if (state?.shape === "personal") return personalEffortDiagnostics(state);
  if (state?.shape === "slider") return sliderEffortDiagnostics(state.effort_slider, state.surface);
  return null;
}

function effortDiagnostics(items) {
  return items.map((item) => ({
    label: optionLabel(item),
    checked: itemIsChecked(item),
    disabled: effortOptionDisabled(item)
  }));
}

// ---------------------------------------------------------------------------
// Tier/family predicates (moved, unchanged).
// ---------------------------------------------------------------------------

function isSupportedPickerShape(state) {
  return state?.shape === "menu" || state?.shape === "slider" || state?.shape === "personal";
}

function effortIsChatProTier(state) {
  if (state?.shape === "personal") {
    const label = foldedModelText(state.effort_label).replace(/\s+/g, " ");
    return label === "pro";
  }
  if (state?.shape === "slider") {
    return sliderEffortSnapshot(state.effort_slider, state.surface)?.label === "pro";
  }
  const items = state?.effort_items ?? [];
  const checked = items.find((item) => itemIsChecked(item));
  return foldedModelText(optionLabel(checked)) === "pro";
}

// ---------------------------------------------------------------------------
// readPicker — the one exported pure reader. Composes the moved functions into
// the PickerRead value. Never throws.
//
// Re-exported helpers (named exports) are consumed by chatgpt-dom.js so its
// behavior is byte-for-byte unchanged; the driver does not call readPicker yet
// (Wave 2).
// ---------------------------------------------------------------------------

export function readPicker(root, { pill = null, leftoverTriggers = [], familySurface = null } = {}) {
  try {
    const state = findPickerState(root, { pill, leftoverTriggers });
    if (!state) {
      return {
        shape: null,
        surface: null,
        trust: null,
        ready: false,
        family: { label: null, checked: false, options: [], latestOption: null, checkedCount: 0 },
        effort: { label: null, options: [], items: [], disabled: false, disabledReason: null, control: null, kind: null },

        nav: { familyTrigger: null, viewToggle: null, expanded: false },
        diagnostics: {
          advanced_rows: [],
          effort_control: null,
          family_menu_probe: null
        }
      };
    }

    const surface = state.surface ?? state.menu;
    const trust = state.surface_trust ?? null;
    const ready = pickerStateIsReady(state);

    // Family value. On the main surface the reader reads inline family
    // radios. When the family lives in a Radix submenu (menu/personal shapes)
    // the main surface has no inline radios; the driver reveals the submenu
    // (revealFamily, clicks only) and passes it as familySurface. The reader
    // then reads family from familySurface with the same three-leg trust rule
    // readCheckedSolFamily used: controlled-by-trigger, aria_controls_structural
    // + activeView, or expandedSelectModelView.
    let familyRadios = state.shape === "menu" || state.shape === "slider"
      ? familyMenuRadios(surface, true)
      : [];
    let familyMenuProbe = null;
    if (familyRadios.length === 0 && familySurface) {
      const trigger = state.family_trigger ?? null;
      const controlledSurface = structurallyOpenControlledSurfaceForTrigger(root, trigger);
      const activeView = activeFamilyView(root, surface, trigger, pill, leftoverTriggers);
      const familyTrusted = familySurface === controlledSurface
        || (trust === "aria_controls_structural" && familySurface === activeView)
        || (familySurface === activeView && expandedSelectModelView(trigger, familySurface));
      familyRadios = familyMenuRadios(familySurface, familyTrusted);
      familyMenuProbe = {
        trigger_found: Boolean(trigger),
        trigger_is_select_model_toggle: isSelectModelViewToggle(trigger),
        trigger_expanded: trigger?.getAttribute?.("aria-expanded") ?? null,
        menu_found: Boolean(familySurface),
        menu_structurally_trusted: familyTrusted,
        radio_count: familyRadios.length,
        checked_count: familyRadios.filter((item) => item?.getAttribute?.("aria-checked") === "true").length
      };
    }
    // When reading from a submenu (familySurface), the checked predicate is
    // strict aria-checked === "true" (not itemIsChecked, which also accepts
    // data-state=checked) — matching the driver's submenu re-read. The test
    // fixture sets aria-checked=false + data-state=checked to exercise the
    // strict requirement.
    const familyChecked = familyMenuProbe
      ? (item) => item?.getAttribute?.("aria-checked") === "true"
      : itemIsChecked;
    const familyOptions = familyRadios.map((item) => optionLabel(item)).filter(Boolean);
    const checkedFamily = familyRadios.find((item) => familyChecked(item)) ?? null;
    const latestOption = familyRadios.find((item) => familyIsLatest(optionLabel(item))) ?? null;
    // When family was read from a submenu (familySurface), the label comes
    // from the checked radio, not state.family_label (which is the trigger
    // text like "Model\nLatest").
    const familyLabel = familyMenuProbe
      ? (checkedFamily ? optionLabel(checkedFamily) : null)
      : (state.family_label
        ? (state.family_label || null)
        : (checkedFamily ? optionLabel(checkedFamily) : null));

    // Effort value.
    let effortLabel = null;
    let effortControl = null;
    let effortKind = null;
    let effortOptions = [];
    let effortItems = [];
    let effortDisabled = false;
    let effortDisabledReason = null;
    if (state.shape === "personal") {
      effortLabel = state.effort_label ?? null;
      effortKind = "row";
      effortControl = state.effort_row ?? null;
      effortItems = [];
    } else if (state.shape === "slider") {
      const snapshot = sliderEffortSnapshot(state.effort_slider, surface);
      if (snapshot) {
        effortLabel = snapshot.display_label ?? null;
        effortControl = state.effort_slider ?? null;
        effortKind = "slider";
      } else {
        // A slider-shaped surface can carry tier rows (menuitemradio) instead
        // of a parsable effort slider — e.g. the quota-locked unified picker
        // whose only slider is the degenerate power control. Read those rows
        // directly (bypassing menuRadioItems' tooltip-wrapper rejection: the
        // tier rows are wrapped in tooltip <span data-state=closed> but are
        // still the active effort options of an open surface).
        const effortItems = Array.from(surface?.querySelectorAll?.('[role="menuitemradio"]') ?? [])
          .filter((item) => !isFamilyOptionLabel(optionLabel(item))
            && item.getAttribute?.("data-state") !== "closed"
            && structurallyReadable(item, surface));
        effortOptions = effortItems.map((item) => optionLabel(item)).filter(Boolean);
        const checkedEffort = effortItems.find((item) => itemIsChecked(item)) ?? null;
        effortLabel = checkedEffort ? optionLabel(checkedEffort) : null;
        effortControl = checkedEffort ?? (effortItems[0] ?? null);
        effortKind = "rows";
      }
    } else if (state.shape === "menu") {
      effortItems = (state.effort_items ?? [])
        .filter((item) => !isFamilyOptionLabel(optionLabel(item))
          && structurallyReadable(item, surface));
      effortOptions = effortItems.map((item) => optionLabel(item)).filter(Boolean);
      const checkedEffort = effortItems.find((item) => itemIsChecked(item)) ?? null;
      effortLabel = checkedEffort ? optionLabel(checkedEffort) : null;
      effortControl = checkedEffort ?? null;
      effortKind = "rows";
    }
    // The quota-locked ladder disables the Pro tier row.
    const disabled = disabledProEffortOption(surface);
    if (disabled) {
      effortDisabled = true;
      effortDisabledReason = disabled.reason;
    }

    // Nav value: how to reach the family view if it is collapsed.
    const viewToggle = state.shape === "slider"
      ? Array.from(surface?.querySelectorAll?.('[role="menuitem"]') ?? [])
          .find((item) => isSelectModelViewToggle(item)) ?? null
      : null;
    const expanded = viewToggle ? expandedSelectModelView(viewToggle, surface) : false;

    const family_menu_probe = familyMenuProbe ?? state.family_menu_probe ?? null;

    return {
      shape: state.shape,
      surface,
      trust,
      ready,
      family: {
        label: familyLabel || null,
        checked: Boolean(checkedFamily),
        options: familyOptions,
        latestOption,
        checkedCount: familyRadios.filter((item) => familyChecked(item)).length
      },
      effort: {
        label: effortLabel,
        options: effortOptions,
        items: effortItems,
        disabled: effortDisabled,
        disabledReason: effortDisabledReason,
        control: effortControl,
        kind: effortKind
      },
      nav: {
        familyTrigger: state.family_trigger ?? null,
        viewToggle,
        expanded
      },
      diagnostics: {
        advanced_rows: advancedViewRows(surface),
        effort_control: effortControlDiagnostics(state),
        family_menu_probe
      }
    };
  } catch (error) {
    // Total: never throws. An unrecognized DOM yields shape:null with
    // diagnostics populated.
    return {
      shape: null,
      surface: null,
      trust: null,
      ready: false,
      family: { label: null, checked: false, options: [], latestOption: null, checkedCount: 0 },
      effort: { label: null, options: [], items: [], disabled: false, disabledReason: null, control: null, kind: null },
      nav: { familyTrigger: null, viewToggle: null, expanded: false },
      diagnostics: {
        advanced_rows: [],
        effort_control: null,
        family_menu_probe: null,
        reader_error: error?.message ?? String(error)
      }
    };
  }
}

// ---------------------------------------------------------------------------
// Named re-exports for chatgpt-dom.js callers (behavior unchanged). The driver
// and diagnostics still call these by name; they now live here.
// ---------------------------------------------------------------------------

export {
  normalizeText,
  textOf,
  foldedModelText,
  foldedFamilyLabel,
  optionLabel,
  itemIsChecked,
  familyIsSol,
  familyIsLatest,
  modelPickerTriggerIsOpen,
  pickerSurfaceIsOpen,
  visibleMenus,
  familyMenuRadios,
  disabledProEffortOption,
  isSelectModelViewToggle,
  expandedSelectModelView,
  activeFamilyView,
  findFamilySubmenu,
  sliderEffortSnapshot,
  structurallyOpenControlledSurfaceForTrigger,
  pickerStateIsReady,
  findPickerState,
  isSupportedPickerShape,
  effortIsChatProTier,
  advancedViewRows,
  sliderEffortDiagnostics,
  effortControlDiagnostics,
  effortDiagnostics
};

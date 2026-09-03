# ChatGPT picker: separate the reader from the driver

Status: ratified design, ready to build. Owner: Aviv. Reviewer: Claude (lead).
Implementer: amit-pi, one wave at a time, gated by review between waves.

## Why

`extensions/chatgpt-native/src/chatgpt-dom.js` is 4,681 lines / 275 functions
and grew +922 lines across five releases in two days (v0.5.62..v0.5.67). Every
ChatGPT picker drift was absorbed by OR-ing a new heuristic onto the last:

```js
// chatgpt-dom.js:1928 classifyPickerSurface
if (surfaceHasParsableEffortSlider(surface)
  || looksLikeLegacyAdvancedPicker(surface)
  || hasSelectModelViewToggle(surface)
  || hybridFamilyView(surface, structurallyTrusted)) { ... }
```

The thing the recipe needs to know — *which model family is checked, which
effort tier is checked, are the tiers disabled, is the surface done mounting* —
is braided with *how ChatGPT rendered it this week* and *how Chrome throttles
hidden tabs*. Today that knowledge is read in six places that must agree:

| Site | Line | What it re-derives |
|---|---|---|
| `findPickerState` | 1863 | which surface is the picker, what shape |
| `pickerStateIsReady` | 1835 | whether it has finished mounting |
| `readCheckedSolFamily` | 1439 | which family is checked (3 trust legs) |
| `effortIsChatProTier` | 2347 | which tier is checked (3 shapes) |
| `disabledProEffortOption` | 2328 | whether the ladder is quota-locked |
| `pickerMenuMounted` | 1346 | "is a picker open at all" for the click-abort |

Three reviewers spent most of their effort proving those six agree. The next
drift will cost the same day and the same three reviewers. This design makes
the agreement structural: one pure reader produces one value; everything else
consumes the value.

## Invariants (unchanged, non-negotiable)

1. Never guess family or tier. Selection is proven only by re-reading the DOM.
2. A successful click is never proof.
3. Fail closed with the cause named (`failure_reason` vocabulary is frozen —
   see "Compatibility").
4. Opacity never gates a read; structural readability (`hidden`, `aria-hidden`,
   `inert`, `display:none`, `visibility:hidden`, up to the document root) does.
5. User tabs never receive the shim; visible yoetz tabs get only the visibility
   spoof and the rAF race.

## The value

One plain object, produced by one pure function, consumed by everyone.

```js
// src/chatgpt-picker-reader.js
/**
 * @typedef {Object} PickerRead
 * @property {"menu"|"slider"|"personal"|null} shape   // null = no picker surface found
 * @property {Element|null} surface                   // the open picker root, for the driver to act on
 * @property {"aria_controls_structural"|"visible"|"content_structural"|null} trust
 * @property {boolean} ready       // finished mounting: family readable AND (effort control | tier rows | disabled ladder)
 * @property {{label:string|null, checked:boolean, options:string[], solOption:Element|null, checkedCount:number}} family
 * @property {{label:string|null, options:string[], disabled:boolean, disabledReason:string|null, control:Element|null, kind:"slider"|"rows"|"row"|null}} effort
 * @property {{familyTrigger:Element|null, viewToggle:Element|null, expanded:boolean}} nav   // how to reach the family view if it is collapsed
 * @property {Object} diagnostics  // the current advanced_rows / effort_control / family_menu_probe shapes, verbatim
 */
export function readPicker(root, { pill } = {}) → PickerRead
```

Rules for `readPicker`:

- **Pure.** No awaits, no dispatched events, no attribute writes. Same DOM →
  same value. This is what makes it snapshot-testable.
- **Total.** Never throws; an unrecognized DOM yields `shape: null` with
  `diagnostics` populated. The driver decides what `null` means.
- **Owns every DOM heuristic on the picker surface — and locates nothing.**
  Every function in the table below moves under it (or is deleted). Nothing
  outside the reader may call `querySelectorAll` on a picker surface. The
  converse holds too: the reader never locates page-level controls. The
  composer pill and any leftover picker triggers are **inputs** —
  `readPicker(root, { pill, leftoverTriggers })` — found by the driver's
  layout-dependent locators (`findModelButton` 152, `openComposerPickerLeftovers`
  1026, `composerMenuTriggers`, `modelControlScopes`, `findComposer`), which
  stay in `chatgpt-dom.js`. The reader imports nothing from `chatgpt-dom.js`;
  a text-level test asserts none of those identifiers (nor `isVisible`,
  `checkVisibility`, `getBoundingClientRect`, `getComputedStyle`) appear in
  the reader module. (Ruling on amit-pi's Wave 1 question, 2026-09-03.)
- `family.label` is derived from **text** of `menuitemradio` items matching
  `isFamilyOptionLabel` (`^gpt\b|^o3$`), never from aria-label (the tier rows
  are aria-label-only; keeping the asymmetry is what stops a tier row from
  being read as a family). This is today's `familyMenuRadios` rule and it is
  preserved on purpose.
- `effort.label` reads the checked tier row by `optionLabel` (text || aria-label),
  or the parsable slider snapshot (`Label, n of m.`), rejecting `instant`,
  `faster`, `smarter`, `speed` labels as power controls (today's
  `sliderIsEffortControl` + `sliderEffortSnapshot`, lines 2207/2388).
- `ready` is the one place mount-completeness is decided (today split across
  `pickerStateIsReady` 1835 and `surfaceHasEffortRows` 1830).

## The driver

`selectSolChatProModel` (1052) keeps its shape — open, prove family, prove
effort, close, reverify — but every `if` on DOM shape becomes an `if` on the
value:

```js
let read = await openAndRead(root, pill, options);           // open loop, polls readPicker until read.ready
if (!read.shape)            return fail("model_picker_open_failed", read);
if (!read.family.checked)   read = await ensureFamilyVisible(root, read, options); // clicks nav.viewToggle, re-reads
if (read.family.label && !familyIsSol(read.family.label)) { click(read.family.solOption); read = await reopenAndRead(...); }
if (!familyIsSol(read.family.label)) return fail(read.family.checkedCount === 1 ? "model_family_not_found" : "model_family_menu_unverified", read);
if (read.effort.disabled)   return fail("effort_options_disabled", read);
if (read.effort.label !== "Pro") read = await moveEffortToPro(root, read, options);  // dispatches on read.effort.kind
if (read.effort.label !== "Pro") return fail(read.effort.control ? "effort_slider_move_failed" : "effort_control_not_found", read);
const closed = await closeAndVerify(root, pill, read, options);                     // pill must corroborate Pro
const again  = await reopenAndRead(root, pill, options);                             // post-close reverify: same predicates, no new logic
```

The three activation-safety mechanisms stay, but as driver concerns expressed
against the value: the gesture aborts when `readPicker(root).surface` is
non-null (replaces `pickerMenuMounted`); Escape-recovery fires only when a
menu is mounted with `shape: null` and `diagnostics.hallmarks` empty; the
hidden-tab hydration gate is untouched.

## What is deleted

From `chatgpt-dom.js`, after the reader lands: `findPickerState`,
`readStructurallyTrustedPickerState`, `classifyPickerSurface`,
`readMenuPickerState`, `readSliderPickerState`, `readPersonalPickerState`,
`findSliderPickerSurface`, `findAdvancedPickerSurface`,
`findPersonalPickerSurface`, `findMainModelMenu`, `findFamilySubmenu`,
`hybridFamilyView`, `hasSelectModelViewToggle`, `looksLikeLegacyAdvancedPicker`,
`looksLikePersonalPicker`, `surfaceHasParsableEffortSlider`,
`surfaceHasEffortRows`, `pickerStateIsReady`, `pickerMenuMounted`,
`readCheckedSolFamily` (the read half), `effortIsChatProTier`,
`disabledProEffortOption`, `isSupportedPickerShape`, `activeFamilyView`,
`expandedSelectModelView`, `structuralFamilyEvidence`,
`familyTriggerForPicker`, `familySurfaceForPicker`. Roughly 900 lines.
The generated-JS mirror in `crates/yoetz-cli/src/chatgpt_web.rs` (3,764
lines, CDP transports) is **out of scope** for this design; it is tracked by
yoetz#462 and should be redone by generating from the same reader module once
this lands, not by hand-porting.

## Snapshot fixtures replace hand-built fakes

The 5,632-line `fake-chatgpt.test.js` (7 fixture builders, 203 tests) encodes
the DOM as JS builder calls, and its fake's `matchesSimpleSelector` (5414)
lacks attribute selectors — which is why production code has a
`querySelectorAll("*")` scan (2676). The fake is shaping the real thing.

New: `tests/fixtures/chatgpt-picker/*.html` — serialized real picker DOM,
captured from live tabs. **Capture mechanism: a dev-only script, not a
protocol change.** `scripts/capture-chatgpt-picker.mjs` drives the existing
chrome-devtools MCP / CDP session against a foreground tab: open the pill,
expand the family view, then serialize the open `[role="menu"]` with computed
`inert`/`aria-*`/`data-state` written back as attributes and all
`<script>`/`<svg>` bodies stripped. It never touches the extension, the
native host, or `inspect_run` (Rust→SW→CS, three layers — deliberately out of
scope for the implementer). One file per observed shape, named by date and
shape:

```
2026-08-27-personal-picker-pro.html
2026-08-30-hybrid-slider-inline-family.html
2026-09-01-collapsed-select-model-inert.html
2026-09-01-thinking-effort-list-aria-label-rows.html
2026-09-02-unified-quota-locked-instant-slider.html
2026-09-02-pre-expanded-family-view-empty-effort.html
```

`tests/chatgpt-picker-reader.test.js` loads each into a real DOM
(`jsdom` as a **devDependency** in `extensions/chatgpt-native/package.json`,
which today has none — `node_modules/` must be added to `.gitignore` and
must NOT enter `package_file_paths` in `crates/yoetz-cli/build.rs:38` or the
zip in `scripts/build-chatgpt-native-extension.sh`; CI installs it in the
test job only. jsdom gives real `querySelector` semantics, `inert`,
`getComputedStyle` for inline/`display:none` styles) and
asserts the **value**: `{shape, ready, family.label, effort.label,
effort.disabled}` per fixture. A drift is then caught by capturing one new
HTML file and watching which assertion moves — not by a night of live runs.

The driver keeps a *small* fake-DOM suite (the FakeElement harness stays) only
for interaction sequencing — open/abort/Escape/close — where the fake's
event dispatch is the point. Target: fake suite shrinks to ~40 tests; reader
suite is fixture-driven.

## jsdom boundary (verified 2026-09-03, jsdom 30.0.1)

jsdom has **no layout engine**: `getBoundingClientRect()` is always 0×0,
`Element.checkVisibility` is undefined, `Element.inert` (the property) is
undefined, and `getComputedStyle(child).display` does **not** cascade from a
`display:none` ancestor. Today's `isVisible` (chatgpt-dom.js:4354) relies on
`checkVisibility` and layout, so it cannot be the reader's visibility test.

Consequences for the reader — these are requirements, not suggestions:

- The reader's readability predicate walks ancestors itself and reads
  **attributes and inline style only**: `hidden`, `aria-hidden="true"`,
  the `inert` attribute, `data-state="closed"`, and inline
  `display:none` / `visibility:hidden`. This is exactly what
  `structurallyReadablePickerItem` (2288) does today, minus its
  `getComputedStyle` call, and it matches invariant 4 (opacity never gates).
- The reader never calls `isVisible`, `checkVisibility`, or
  `getBoundingClientRect`. Anything needing geometry stays in the driver.
- The capture script must therefore write **computed** `display`/`visibility`
  onto the clone as inline style when they differ from the stylesheet
  default (Wave 1 amends `scripts/capture-chatgpt-picker.mjs`: for each
  element, if `getComputedStyle(live).display === "none"` or
  `visibility === "hidden"`, set it inline on the clone). `inert` is already
  copied as an attribute by the Wave 0 script.
- Fixture assertions are therefore about structure and text, never about
  geometry — which is the point: geometry is Chrome's business, the picker's
  *meaning* is ours.

## Compatibility (what must not change)

- `failure_reason` strings, all 26 of them (`grep -oE '"[a-z_]+_(failed|unverified|not_found|disabled|mismatch|detected)"'`), and the `status ∈ {selected,current,unavailable}` contract consumed by
  `sites/chatgpt.js:60 isAcceptableModelSelection` and `service-worker.js`
  (`modelSelectionFailureDiagnostics` key list at ~745).
- Diagnostics keys (`advanced_rows`, `effort_control`, `family_menu_probe`,
  `hydration_signal`, `post_close_*`, …) keep their shapes; the reader's
  `diagnostics` sub-object is spread into the result exactly as today.
- `configureModelState(root, job)`, `verifyChatgptModelSelectionBeforeSend`,
  `resetModelSelectionState` signatures unchanged (`content-script.js:306-361`).
- No Rust changes. No manifest changes. Shim untouched.

## Waves (one implementer, review gate between each)

Each wave is one PR, one worktree, ≤400 lines net, and must leave the full
suite green and the live check unchanged (`effort_options_disabled`, family
verified — the quota lock is our oracle until Oct 1).

**Wave 0 — capture (½ day).** amit-pi: add `jsdom` devDependency +
`.gitignore`/fingerprint exclusions, a CI step that runs `npm ci` in the
extension dir before `node --test`, and `scripts/capture-chatgpt-picker.mjs`
(CDP `Runtime.evaluate` against a given tab; output = one HTML file). Claude:
run the capture against live tabs for the six shapes (some need the account
in a specific state — the quota-locked ladder is available now, the
personal picker needs the personal account). Land fixtures + a
`chatgpt-picker-reader.test.js` that only asserts each file parses in jsdom
and contains one `[role="menu"]`. Done: 6 files in
`tests/fixtures/chatgpt-picker/`, suite green locally and in CI.

**Wave 1 — reader, additive (1 day).** Create `src/chatgpt-picker-reader.js`
by *moving* the table's functions under it, unexported except `readPicker`.
Do not change behavior. Write the fixture assertions for all six files.
`chatgpt-dom.js` imports `readPicker` but does not use it yet (keeps its own
paths). Done: reader tests pass on all fixtures; `chatgpt-dom.js` unchanged
in behavior; suite green.

**Wave 2 — driver consumes the value (1 day).** Rewrite
`selectSolChatProModel` + `reverifyModelSelectionAfterClose` +
`openModelPicker`'s abort predicate against `PickerRead`. Delete the old
read paths. Every existing `fake-chatgpt.test.js` selection test must still
pass unmodified (they are the behavioral contract). Done: suite green,
`chatgpt-dom.js` ≤ 3,800 lines, live check unchanged.

**Wave 3 — shrink the fake (½ day).** Delete fake-DOM tests whose only
subject is DOM shape (now covered by fixtures); keep sequencing tests.
Remove the `querySelectorAll("*")` scans the fake forced. Done: fake suite
≤ ~60 tests, no test double shaping production code.

**Wave 4 — codify (½ day).** CLAUDE.md "Browser Architecture" paragraph
rewritten to describe the reader/driver split and the fixture-capture
procedure for the next drift (capture → assert → fix reader → done).
Memory + release.

## Review protocol (how I review one amit-pi wave)

1. Read the whole diff (not the receipt).
2. Run `node --test tests/*.test.js` myself; expect green.
3. For Wave 1/2: run the reader against every fixture and diff its output
   against the pre-wave driver's `findPickerState` result on the same
   fixture (a one-off script; parity is the acceptance test for "moved, not
   changed").
4. One live run, paced, expect `effort_options_disabled` + family verified.
5. One batched review message. Builder does not recut until the round closes.

## Non-goals

- Claude recipe (`claude-dom.js`) — separate design after this proves out;
  yoetz#472 stays open.
- CDP transports (`chatgpt_web.rs`) — yoetz#462, generate-from-reader later.
- Any change to what the recipe *selects* (Sol + Pro, Chat surface).

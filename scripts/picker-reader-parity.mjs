#!/usr/bin/env node
// scripts/picker-reader-parity.mjs — dev-only parity check for the Wave 1 reader.
//
// For each fixture in tests/fixtures/chatgpt-picker/*.html, loads it in jsdom
// and runs BOTH the new readPicker (from the reader module) and the pre-Wave
// findPickerState (imported from a temp file holding the main-branch
// chatgpt-dom.js), then prints a side-by-side table of {shape, family label,
// effort label, disabled} and exits non-zero on any mismatch. This is the
// "moved, not changed" acceptance test described in the design review protocol.
//
// Usage:
//   node scripts/picker-reader-parity.mjs            # all fixtures
//   node scripts/picker-reader-parity.mjs --fixture <name>.html
//
// Requires jsdom installed in extensions/chatgpt-native (npm ci --ignore-scripts).

import { readFileSync, readdirSync, existsSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { execSync } from "node:child_process";
import { JSDOM } from "../extensions/chatgpt-native/node_modules/jsdom/lib/api.js";
import { readPicker } from "../extensions/chatgpt-native/src/chatgpt-picker-reader.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..");
const FIXTURES_DIR = join(ROOT, "extensions/chatgpt-native/tests/fixtures/chatgpt-picker");

// --- Write the pre-Wave chatgpt-dom.js to a temp file so we can
// import its findPickerState without polluting the worktree. ---
const TMP_DIR = join(ROOT, ".tmp-parity");
const OLD_DOM_PATH = join(TMP_DIR, "chatgpt-dom-main.js");

// Pinned to the pre-Wave-1 revision of chatgpt-dom.js: 6aaa07f is the parent
// of the Wave 1 move commit 03c442c (#480) — the last commit where
// chatgpt-dom.js still owns findPickerState. Main moved findPickerState into
// the reader module in Wave 1, so a moving ref ("main") would silently
// compare the reader against itself. 15be4bc (#469) and eb46866 (#473) land
// before 03c442c and are part of the baseline; earlier pins (e.g. ad76610)
// predate the aria-hidden toggle + hybridFamilyView handling the unified
// quota-locked fixture needs and wrongly turn a real parity match into a
// "drift" INFO row.
const PARITY_BASELINE_COMMIT = "6aaa07f";

function prepareOldDom() {
  mkdirSync(TMP_DIR, { recursive: true });
  // `git show <pinned>:path` extracts the baseline file from history.
  const oldSrc = execSync(
    `git show ${PARITY_BASELINE_COMMIT}:extensions/chatgpt-native/src/chatgpt-dom.js`,
    {
      cwd: ROOT, encoding: "utf8", maxBuffer: 50 * 1024 * 1024
    }
  );
  // Rewrite imports to bare specifiers the temp file can resolve. The old
  // module has no relative imports (it is self-contained at this revision),
  // so we write it as-is.
  // Export findPickerState so this script can import it. The pre-Wave file
  // keeps it private; append a re-export without touching the original logic.
  //
  // jsdom shim: the baseline isVisible() ends with a layout gate
  // (getClientRects().length === 0). jsdom has no layout engine — every
  // element reports zero rects — and no checkVisibility, so as written the
  // baseline would see no menus at all in this script. Gating the layout
  // check on checkVisibility's existence skips only that layout gate, which
  // is fair here: jsdom gives both readers attribute + inline-style
  // evidence only (the new reader makes no layout calls at all), and the
  // fixtures carry no stylesheets. This is NOT a claim of browser
  // equivalence — in a real browser checkVisibility being true falls
  // through to the layout gate, which stays reachable there.
  const LAYOUT_GATE = 'if (!options.allowNoLayout && typeof element.getClientRects === "function" && element.getClientRects().length === 0) {';
  const SHIMMED_LAYOUT_GATE = 'if (!options.allowNoLayout && typeof element.checkVisibility === "function" && typeof element.getClientRects === "function" && element.getClientRects().length === 0) {';
  const shimmedOldSrc = oldSrc.replace(LAYOUT_GATE, SHIMMED_LAYOUT_GATE);
  if (shimmedOldSrc === oldSrc) {
    throw new Error(
      `parity baseline isVisible layout-gate not found at ${PARITY_BASELINE_COMMIT}; re-pin the shim`
    );
  }
  writeFileSync(
    OLD_DOM_PATH,
    shimmedOldSrc + "\n\nexport { findPickerState, pickerVerifiedEffortLabel };\n",
    "utf8"
  );
}

let findPickerStateOld;
let effortLabelOld;
async function loadOldReader() {
  const mod = await import(`file://${OLD_DOM_PATH}`);
  findPickerStateOld = mod.findPickerState;
  effortLabelOld = mod.pickerVerifiedEffortLabel;
  if (typeof findPickerStateOld !== "function") {
    throw new Error("pre-Wave chatgpt-dom.js does not export findPickerState");
  }
  if (typeof effortLabelOld !== "function") {
    throw new Error("pre-Wave chatgpt-dom.js does not export pickerVerifiedEffortLabel");
  }
}

function summarizeOld(state) {
  if (!state) return { shape: null, family: null, effort: null, disabled: null };
  return {
    shape: state.shape ?? null,
    family: state.family_label ?? null,
    // The old state carries the effort evidence, not a settled label; use the
    // baseline's own verification helper so both sides are compared as labels.
    effort: effortLabelOld(state),
    disabled: null
  };
}

function summarizeNew(read) {
  return {
    shape: read.shape,
    family: read.family?.label ?? null,
    effort: read.effort?.label ?? null,
    disabled: read.effort?.disabled ?? null
  };
}

const args = process.argv.slice(2);
let filter = null;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--fixture") filter = args[++i];
}

const fixtures = existsSync(FIXTURES_DIR)
  ? readdirSync(FIXTURES_DIR).filter((n) => n.endsWith(".html") && (!filter || n === filter))
  : [];

if (fixtures.length === 0) {
  console.error("no fixtures found in tests/fixtures/chatgpt-picker/");
  process.exit(0);
}

// The pinned 6aaa07f baseline predates #485: isFamilyOptionLabel there is
// /^gpt\b|^o3$/i, so the old reader drops the Latest radio on the gpt6 fixture
// and reports family "" where the new reader reads 'Latest' (shape and effort
// still match). That vocabulary drift is expected and INFO-only; a shape or
// effort disagreement on any row is a parity failure.
const KNOWN_BASELINE_DRIFT = new Map([
  ["2026-09-05-gpt6-chat-family-expanded.html",
    "baseline predates the 'Latest' family (#485): old reader drops the Latest radio"],
]);

let mismatches = 0;
let infos = 0;

const EXPECTATIONS = JSON.parse(readFileSync(join(FIXTURES_DIR, "expectations.json"), "utf8"));

function newMatchesExpectations(name, read) {
  const expected = EXPECTATIONS[name];
  if (!expected) return true; // no expectation recorded for this fixture
  return String(read.shape) === String(expected.shape)
    && String(read.family.label) === String(expected.family.label)
    && String(read.effort.label) === String(expected.effort.label)
    && Boolean(read.effort.disabled) === Boolean(expected.effort.disabled);
}
prepareOldDom();
await loadOldReader().catch((err) => {
  console.error(`failed to load pre-Wave reader: ${err?.message ?? err}`);
  process.exit(1);
});

console.log(`\nParity check: ${fixtures.length} fixture(s)\n`);
console.log("fixture".padEnd(55), "shape(old/new)", "family(old/new)", "effort(old/new)");
console.log("-".repeat(120));

for (const name of fixtures) {
  const html = readFileSync(join(FIXTURES_DIR, name), "utf8");
  const dom = new JSDOM(html);
  const doc = dom.window.document;

  let oldResult, newRead;
  try {
    oldResult = summarizeOld(findPickerStateOld(doc));
  } catch (e) {
    oldResult = { shape: `ERR:${e.message}`, family: null, effort: null, disabled: null };
  }
  try {
    newRead = readPicker(doc);
  } catch (e) {
    newRead = { shape: `ERR:${e.message}`, family: { label: null }, effort: { label: null, disabled: null } };
  }
  const newResult = summarizeNew(newRead);

  // Hard contract: the NEW reader must match expectations.json exactly.
  // Baseline parity stays informational on known-drift rows.
  if (!newMatchesExpectations(name, newRead)) {
    mismatches++;
    const expected = EXPECTATIONS[name];
    console.log(`✖ ${name.slice(0, 52).padEnd(53)} READER-vs-EXPECTATIONS mismatch`);
    console.log(`    expected: ${JSON.stringify({ shape: expected.shape, family: expected.family.label, effort: expected.effort.label, disabled: expected.effort.disabled })}`);
    console.log(`    actual:   ${JSON.stringify({ shape: newResult.shape, family: newResult.family, effort: newResult.effort, disabled: newResult.disabled })}`);
    continue;
  }

  const shapeMatch = String(oldResult.shape) === String(newResult.shape);
  const familyMatch = String(oldResult.family) === String(newResult.family);
  const effortMatch = String(oldResult.effort) === String(newResult.effort);
  const knownDriftReason = KNOWN_BASELINE_DRIFT.get(name);
  const identical = shapeMatch && familyMatch && effortMatch;
  // INFO only for the vocabulary gap the drift map names (family label on the
  // gpt6 row): shape and effort must still match for INFO treatment.
  const info = shapeMatch && effortMatch && !familyMatch && Boolean(knownDriftReason);
  const ok = identical || info;

  if (info) infos++;
  else if (!ok) mismatches++;
  const mark = ok ? (info ? "i" : "✓") : "✖";  console.log(
    `${mark} ${name.slice(0, 52).padEnd(53)}`,
    `${String(oldResult.shape).slice(0,12)}/${String(newResult.shape).slice(0,12)}`.padEnd(16),
    `${String(oldResult.family)?.slice(0,14)}/${String(newResult.family)?.slice(0,14)}`.padEnd(20),
    `${String(oldResult.effort)?.slice(0,10)}/${String(newResult.effort)?.slice(0,10)}`
  );
  if (info) {
    console.log(`    INFO: ${knownDriftReason}`);
    console.log(
      `    baseline (${PARITY_BASELINE_COMMIT}): shape=${JSON.stringify(oldResult.shape)} family=${JSON.stringify(oldResult.family)} effort=${JSON.stringify(oldResult.effort)} — new reader matches expectations.json`
    );
  }
}

rmSync(TMP_DIR, { recursive: true, force: true });

console.log("-".repeat(120));
if (mismatches > 0) {
  console.error(`\n${mismatches} mismatch(es) — reader is NOT at parity with pre-Wave findPickerState.`);
  process.exit(1);
}
console.log(`\nAll fixtures at parity${infos > 0 ? ` (${infos} known baseline-vocabulary INFO row(s))` : ""}.`);
process.exit(0);

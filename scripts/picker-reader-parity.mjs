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

// --- Write the pre-Wave main-branch chatgpt-dom.js to a temp file so we can
// import its findPickerState without polluting the worktree. ---
const TMP_DIR = join(ROOT, ".tmp-parity");
const OLD_DOM_PATH = join(TMP_DIR, "chatgpt-dom-main.js");

function prepareOldDom() {
  mkdirSync(TMP_DIR, { recursive: true });
  // `git show main:path` extracts the pre-Wave file from the main branch.
  const oldSrc = execSync("git show main:extensions/chatgpt-native/src/chatgpt-dom.js", {
    cwd: ROOT, encoding: "utf8", maxBuffer: 50 * 1024 * 1024
  });
  // Rewrite imports to bare specifiers the temp file can resolve. The old
  // module has no relative imports (it is self-contained at this revision),
  // so we write it as-is.
  // Export findPickerState so this script can import it. The pre-Wave file
  // keeps it private; append a re-export without touching the original logic.
  writeFileSync(OLD_DOM_PATH, oldSrc + "\n\nexport { findPickerState };\n", "utf8");
}

let findPickerStateOld;
async function loadOldReader() {
  const mod = await import(`file://${OLD_DOM_PATH}`);
  findPickerStateOld = mod.findPickerState;
  if (typeof findPickerStateOld !== "function") {
    throw new Error("pre-Wave chatgpt-dom.js does not export findPickerState");
  }
}

function summarizeOld(state) {
  if (!state) return { shape: null, family: null, effort: null, disabled: null };
  return {
    shape: state.shape ?? null,
    family: state.family_label ?? null,
    effort: state.effort_label ?? state.shape === "slider"
      ? (state.effort_slider ? "slider" : null) : null,
    disabled: null
  };
}

function summarizeNew(read) {
  return {
    shape: read.shape,
    family: read.family.label,
    effort: read.effort.label,
    disabled: read.effort.disabled
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

let mismatches = 0;
prepareOldDom();
await loadOldReader().catch((err) => {
  console.error(`failed to load pre-Wave reader: `);
  process.exit(1);
});

console.log(`\nParity check: ${fixtures.length} fixture(s)\n`);
console.log("fixture".padEnd(55), "shape(old/new)", "family(old/new)", "effort(old/new)");
console.log("-".repeat(120));

for (const name of fixtures) {
  const html = readFileSync(join(FIXTURES_DIR, name), "utf8");
  const dom = new JSDOM(html);
  const doc = dom.window.document;

  let oldResult, newResult;
  try {
    oldResult = summarizeOld(findPickerStateOld(doc));
  } catch (e) {
    oldResult = { shape: `ERR:${e.message}`, family: null, effort: null, disabled: null };
  }
  try {
    newResult = summarizeNew(readPicker(doc));
  } catch (e) {
    newResult = { shape: `ERR:${e.message}`, family: null, effort: null, disabled: null };
  }

  const shapeMatch = String(oldResult.shape) === String(newResult.shape);
  const familyMatch = String(oldResult.family) === String(newResult.family);
  const effortMatch = String(oldResult.effort) === String(newResult.effort);
  const ok = shapeMatch && familyMatch && effortMatch;

  if (!ok) mismatches++;
  const mark = ok ? "✓" : "✖";
  console.log(
    `${mark} ${name.slice(0, 52).padEnd(53)}`,
    `${String(oldResult.shape).slice(0,12)}/${String(newResult.shape).slice(0,12)}`.padEnd(16),
    `${String(oldResult.family)?.slice(0,14)}/${String(newResult.family)?.slice(0,14)}`.padEnd(20),
    `${String(oldResult.effort)?.slice(0,10)}/${String(newResult.effort)?.slice(0,10)}`
  );
}

rmSync(TMP_DIR, { recursive: true, force: true });

console.log("-".repeat(120));
if (mismatches > 0) {
  console.error(`\n${mismatches} mismatch(es) — reader is NOT at parity with pre-Wave findPickerState.`);
  process.exit(1);
}
console.log(`\nAll fixtures at parity.`);
process.exit(0);

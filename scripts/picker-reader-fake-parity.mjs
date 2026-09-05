#!/usr/bin/env node
// scripts/picker-reader-fake-parity.mjs — Wave 2 T5: fake-shape family-label parity.
//
// Feeds the 7 FakeDocument fixture builders through BOTH the old driver
// (main branch configureModelState) and the new Wave 2 driver, then prints a
// side-by-side table of the result's family_label (old == new). Exits non-zero
// on any mismatch.
//
// Uses YOETZ_PARITY_IMPORT=1 to import the test file's builders without
// running the test suite (the `test()` calls are no-op'd under that env var).
// Each shape gets a fresh fixture for old and new (the driver mutates the DOM).
//
// Usage:
//   node scripts/picker-reader-fake-parity.mjs

import { execSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "..");
const EXT = join(ROOT, "extensions/chatgpt-native");

// --- Import the new driver + the 7 fake-builders (env guard skips tests) ---
process.env.YOETZ_PARITY_IMPORT = "1";
const newDom = await import(join(EXT, "src/chatgpt-dom.js"));
const builders = await import(join(EXT, "tests/fake-chatgpt.test.js"));

// --- Write the pre-Wave main-branch reader + dom to a temp dir so we can
// import the old configureModelState. ---
const TMP_DIR = join(ROOT, ".tmp-fake-parity");
mkdirSync(TMP_DIR, { recursive: true });

const oldReader = execSync("git show main:extensions/chatgpt-native/src/chatgpt-picker-reader.js", {
  cwd: ROOT, encoding: "utf8", maxBuffer: 50 * 1024 * 1024
});
writeFileSync(join(TMP_DIR, "chatgpt-picker-reader.js"), oldReader, "utf8");

const oldDom = execSync("git show main:extensions/chatgpt-native/src/chatgpt-dom.js", {
  cwd: ROOT, encoding: "utf8", maxBuffer: 50 * 1024 * 1024
});
writeFileSync(join(TMP_DIR, "chatgpt-dom-main.js"), oldDom, "utf8");

const oldDomMod = await import(`file://${join(TMP_DIR, "chatgpt-dom-main.js")}`);

// --- Build the 7 fake shapes (each gets a fresh fixture for old + new) ---
const shapes = [
  {
    name: "SolPicker (menu, family submenu)",
    build: () => builders.makeSolPickerFixture({ family: "GPT-5.6 Sol", effort: "Pro" })
  },
  {
    name: "TwoPillComposer",
    build: () => builders.makeTwoPillComposerFixture({})
  },
  {
    name: "HybridSimpleView (slider, inline family)",
    build: () => builders.makeHybridSimpleViewFixture({})
  },
  {
    name: "ThinkingEffortList (menu)",
    build: () => builders.makeThinkingEffortListFixture({ effort: "Pro" })
  },
  {
    name: "OpacityZeroLeftover",
    build: () => builders.makeOpacityZeroLeftoverFixture()
  },
  {
    name: "SolSlider (backgroundFrozen)",
    build: () => builders.makeSolSliderFixture({ backgroundFrozen: true, initialValue: 5 })
  },
  {
    name: "PersonalPicker",
    build: () => builders.makePersonalPickerFixture({ effort: "Pro" })
  }
];

let mismatches = 0;
console.log("\nFake-shape family-label parity: 7 shapes\n");
console.log("shape".padEnd(45), "family(old/new)");
console.log("-".repeat(80));

for (const { name, build } of shapes) {
  let oldLabel, newLabel;
  try {
    const oldFixture = build();
    const oldResult = await oldDomMod.configureModelState(oldFixture.doc, {});
    oldLabel = oldResult?.family_label ?? null;
  } catch (e) {
    oldLabel = `ERR:${e.message}`;
  }
  try {
    const newFixture = build();
    const newResult = await newDom.configureModelState(newFixture.doc, {});
    newLabel = newResult?.family_label ?? null;
  } catch (e) {
    newLabel = `ERR:${e.message}`;
  }

  const oldNorm = String(oldLabel ?? "").trim();
  const newNorm = String(newLabel ?? "").trim();
  const match = oldNorm === newNorm;
  if (!match) mismatches++;
  const mark = match ? "✓" : "✖";
  console.log(
    `${mark} ${name.slice(0, 42).padEnd(43)}`,
    `${oldNorm.slice(0, 20)} / ${newNorm.slice(0, 20)}`
  );
}

console.log("-".repeat(80));
if (mismatches > 0) {
  console.error(`\n${mismatches} mismatch(es) — family label is NOT at parity.`);
  process.exit(1);
}
console.log("\nAll 7 fake shapes at family-label parity.");
process.exit(0);

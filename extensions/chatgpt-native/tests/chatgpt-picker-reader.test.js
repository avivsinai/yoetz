// tests/chatgpt-picker-reader.test.js
//
// Fixture-driven reader tests (Wave 1+). Each fixture in
// tests/fixtures/chatgpt-picker/*.html is loaded into jsdom and asserted
// against tests/fixtures/chatgpt-picker/expectations.json. See
// docs/design/chatgpt-picker-reader.md.

import { readFileSync, readdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import { readPicker } from "../src/chatgpt-picker-reader.js";

const __dirname = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(__dirname, "fixtures", "chatgpt-picker");
const expectationsPath = join(fixturesDir, "expectations.json");

const fixtures = existsSync(fixturesDir)
  ? readdirSync(fixturesDir).filter((name) => name.endsWith(".html"))
  : [];

const expectations = existsSync(expectationsPath)
  ? JSON.parse(readFileSync(expectationsPath, "utf8"))
  : {};

for (const name of fixtures) {
  test(`fixture ${name} has exactly one [role="menu"]`, () => {
    const html = readFileSync(join(fixturesDir, name), "utf8");
    const dom = new JSDOM(html);
    const menus = dom.window.document.querySelectorAll('[role="menu"]');
    assert.equal(menus.length, 1, `expected one [role="menu"], found ${menus.length}`);
  });

  const expected = expectations[name];
  if (!expected) continue;

  test(`readPicker on ${name} matches expectations`, () => {
    const html = readFileSync(join(fixturesDir, name), "utf8");
    const dom = new JSDOM(html);
    const read = readPicker(dom.window.document);

    assert.equal(read.shape, expected.shape, "shape");
    assert.equal(read.ready, expected.ready, "ready");

    if (expected.family) {
      assert.equal(read.family.label, expected.family.label, "family.label");
      assert.equal(read.family.checked, expected.family.checked, "family.checked");
      assert.equal(read.family.checkedCount, expected.family.checkedCount, "family.checkedCount");
      assert.deepEqual(read.family.options, expected.family.options, "family.options");
    }
    if (expected.effort) {
      assert.equal(read.effort.label, expected.effort.label, "effort.label");
      assert.equal(read.effort.disabled, expected.effort.disabled, "effort.disabled");
      assert.equal(read.effort.kind, expected.effort.kind, "effort.kind");
      assert.deepEqual(read.effort.options, expected.effort.options, "effort.options");
    }
    if (expected.nav) {
      assert.equal(read.nav.expanded, expected.nav.expanded, "nav.expanded");
    }
  });
}

// Boundary lock: the reader module must contain ZERO layout-dependent
// identifiers. The reader is jsdom-pure; if any of these leak in, jsdom
// fixtures would silently diverge from live Chrome. See the "jsdom boundary"
// section of docs/design/chatgpt-picker-reader.md.
test("reader module contains no layout-dependent identifiers (jsdom boundary lock)", () => {
  const readerSource = readFileSync(join(__dirname, "..", "src", "chatgpt-picker-reader.js"), "utf8");
  const forbidden = [
    "isVisible",
    "checkVisibility",
    "getBoundingClientRect",
    "getComputedStyle",
    "findModelButton",
    "modelControlScopes",
    "composerMenuTriggers",
    "openComposerPickerLeftovers",
    "findComposer",
    "isTranscriptModelControl",
    "modelControlLabel",
    "modelPillSummaryMatches",
    "pillHasModelFamilyToken"
  ];
  const failures = [];
  for (const name of forbidden) {
    const re = new RegExp(`\\b${name}\\b`);
    if (re.test(readerSource)) {
      failures.push(name);
    }
  }
  assert.deepEqual(failures, [], `forbidden identifiers found in reader: ${failures.join(", ")}`);
});

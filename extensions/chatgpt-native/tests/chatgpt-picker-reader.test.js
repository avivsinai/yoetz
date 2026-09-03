// tests/chatgpt-picker-reader.test.js
//
// Placeholder per docs/design/chatgpt-picker-reader.md (Wave 0). Each fixture
// file in tests/fixtures/chatgpt-picker/*.html is loaded into jsdom and
// asserted to contain exactly one [role="menu"]. With zero fixtures this loop
// is empty and the suite passes trivially. Wave 1 will extend these assertions
// to the full PickerRead value {shape, ready, family.label, effort.label,
// effort.disabled} once src/chatgpt-picker-reader.js lands.

import { readFileSync, readdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { JSDOM } from "jsdom";

const __dirname = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(__dirname, "fixtures", "chatgpt-picker");

const fixtures = existsSync(fixturesDir)
  ? readdirSync(fixturesDir).filter((name) => name.endsWith(".html"))
  : [];

for (const name of fixtures) {
  test(`chatgpt-picker fixture ${name} has exactly one [role="menu"]`, () => {
    const html = readFileSync(join(fixturesDir, name), "utf8");
    const dom = new JSDOM(html);
    const menus = dom.window.document.querySelectorAll('[role="menu"]');
    if (menus.length !== 1) {
      throw new Error(
        `expected exactly one [role="menu"] in ${name}, found ${menus.length}`
      );
    }
  });
}

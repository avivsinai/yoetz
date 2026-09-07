import assert from "node:assert/strict";
import test from "node:test";
import { openModelMenu } from "../src/claude-dom.js";

// Regression for gh-472 / yz-zdm: openModelMenu retried with a second
// button.click() when aria-expanded was not seen within the attempt budget;
// in a throttled hidden tab the menu opens late and the second click toggled
// it closed. Mirror the ChatGPT #469 fix: before any retry click, settle and
// re-read aria-expanded; abort the retry when the menu is open.

function makeRoot() {
  // classifyBlockingState returns null when body text is not a usage-credits
  // message, so throwIfModelBlocked is a no-op for this root.
  return { body: { textContent: "Claude" } };
}

test("openModelMenu does not toggle a late-opening menu closed (gh-472)", async () => {
  // The first click schedules aria-expanded=true on a timer that lands just
  // after the attempt-0 wait's budget (100ms) but inside the retry's settle
  // window (MODEL_MENU_SETTLE_MS=300ms). Without the fix the retry click
  // cancels the pending open and the menu never opens; with the fix the
  // settle re-check observes the open and returns without a second click.
  let open = false;
  let pendingTimer = null;
  let clicks = 0;
  const button = {
    getAttribute(name) {
      return name === "aria-expanded" ? (open ? "true" : "false") : null;
    },
    setAttribute() {},
    click() {
      clicks += 1;
      if (open) {
        // A click on an already-open Radix trigger toggles it closed — the bug.
        open = false;
        return;
      }
      if (pendingTimer === null) {
        // The menu opens late, just after the bounded wait misses it.
        pendingTimer = setTimeout(() => {
          pendingTimer = null;
          open = true;
        }, 150);
      } else {
        // A second click while the open is pending cancels it (toggle-close).
        clearTimeout(pendingTimer);
        pendingTimer = null;
      }
    }
  };

  await openModelMenu(makeRoot(), button, 100);

  assert.equal(button.getAttribute("aria-expanded"), "true", "the late-opened menu stays open");
  assert.equal(clicks, 1, "the retry must not click an already-open trigger");
});

test("openModelMenu still retries when the menu genuinely does not open", async () => {
  let clicks = 0;
  const button = {
    getAttribute: () => "false",
    setAttribute() {},
    click() { clicks += 1; }
  };

  await assert.rejects(
    openModelMenu(makeRoot(), button, 100),
    /did not open within 100ms/
  );
  assert.equal(clicks, 2, "both attempts click when the menu never opens");
});

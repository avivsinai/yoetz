import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { JSDOM } from "jsdom";

const script = fileURLToPath(new URL("../../../scripts/capture-gemini-surface.mjs", import.meta.url));

function withCapture(surface, markup, run, url = "https://gemini.google.com/app/sample-path?account=sample") {
  const snippet = execFileSync(process.execPath, [script, surface], { encoding: "utf8" });
  const { window } = new JSDOM(markup, { url, runScripts: "outside-only" });
  let output;
  window.copy = (value) => { output = value; };
  try {
    run(window.document, (root) => {
      window.$0 = root;
      window.eval(snippet);
      return output === undefined ? undefined : JSON.parse(output);
    }, () => output);
  } finally {
    window.close();
  }
}

test("chooser capture preserves selected states and strips all label and ID values", () => {
  withCapture("chooser", '<div id="chooser" role="menu" aria-label="sample profile" data-state="open">'
    + '<button role="menuitemradio" aria-checked="true" data-testid="sample-identity">Sample model</button>'
    + '<input type="radio" aria-labelledby="sample-identifier" value="sample value">'
    + '<script>sample script text</script></div><p>Sample conversation text</p>',
  (document, capture) => {
    document.querySelector("input").checked = true;
    const chooser = document.querySelector("#chooser");
    assert.deepEqual(capture(chooser), {
      surface: "chooser",
      root: {
        tag: "div", role: "menu", "data-state": "open", attributes_present: ["aria-label"],
        children: [
          { tag: "button", role: "menuitemradio", "aria-checked": "true", attributes_present: ["data-testid"] },
          { tag: "input", type: "radio", checked: true, attributes_present: ["aria-labelledby"] }
        ]
      }
    });
    assert.equal(chooser.querySelector("button").textContent, "Sample model");
    assert.equal(chooser.querySelector("input").value, "sample value");
  });
});

test("composer capture records editable and disabled controls without reading their contents", () => {
  withCapture("composer", '<div id="composer"><sample-component data-session="sample session">'
    + '<div role="textbox" contenteditable="true" aria-multiline="true">Sample draft</div>'
    + '</sample-component><textarea>Another sample draft</textarea>'
    + '<button type="submit" disabled aria-label="Sample send label">Sample button</button></div>',
  (document, capture) => {
    assert.deepEqual(capture(document.querySelector("#composer")), {
      surface: "composer",
      root: {
        tag: "div", children: [
          { tag: "custom", children: [{ tag: "div", role: "textbox", contenteditable: "true", "aria-multiline": "true" }] },
          { tag: "textarea" },
          { tag: "button", type: "submit", disabled: true, attributes_present: ["aria-label"] }
        ]
      }
    });
  });
});

test("capture refuses lookalike origins and broad roots before copying anything", () => {
  withCapture("chooser", "<div id='chooser'></div>", (document, capture, output) => {
    assert.throws(() => capture(document.querySelector("#chooser")), /top-level Gemini web tab/);
    assert.equal(output(), undefined);
  }, "https://gemini.google.com.example.invalid/app");
  withCapture("composer", "<main><div role='textbox'></div></main>", (document, capture, output) => {
    assert.throws(() => capture(document.body), /select the narrow chooser/i);
    assert.throws(() => capture(document.querySelector("main")), /select the narrow chooser/i);
    assert.equal(output(), undefined);
  });
});

test("capture refuses an oversized selection instead of copying a partial tree", () => {
  const markup = "<div id='large'>" + "<button></button>".repeat(128) + "</div>";
  withCapture("chooser", markup, (document, capture, output) => {
    assert.throws(() => capture(document.querySelector("#large")), /capture scope is too broad/i);
    assert.equal(output(), undefined);
  });
});

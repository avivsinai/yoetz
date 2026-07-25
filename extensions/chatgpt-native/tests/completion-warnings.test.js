import assert from "node:assert/strict";
import test from "node:test";
import { completionWarnings } from "../src/completion-warnings.js";

test("artifact warning coexists with the generic empty-response warning", () => {
  const artifactWarning = {
    code: "artifact_unextracted",
    count: 1,
    titles: ["Release plan"]
  };

  assert.deepEqual(
    completionWarnings({
      extraction: { text: "" },
      emptyResponseWarning: "empty Claude response extracted",
      extractionWarnings: [artifactWarning]
    }),
    [
      "empty Claude response extracted",
      artifactWarning
    ]
  );
});

import assert from "node:assert/strict";
import test from "node:test";
import {
  advertisedRecipes,
  siteAdapterForRecipe
} from "../src/sites/index.js";

test("Wave 2A advertises only the ChatGPT recipe", () => {
  assert.deepEqual(advertisedRecipes(), ["chatgpt"]);
});

test("site adapter defaults a missing legacy recipe to ChatGPT", () => {
  const adapter = siteAdapterForRecipe(undefined);
  assert.equal(adapter.recipe, "chatgpt");
  assert.equal(adapter.displayName, "ChatGPT");
  assert.equal(adapter.jobUrl("run 1"), "https://chatgpt.com/?_yoetz=run+1");
  assert.equal(adapter.isAllowedTabUrl("https://chatgpt.com/c/conv-1"), true);
  assert.equal(adapter.isAllowedTabUrl("https://claude.ai/chat/conv-1"), false);
  assert.equal(typeof adapter.dom.extractResponse, "function");
  assert.equal(typeof adapter.fetchConversationAnswer, "function");
});

test("site adapter rejects unavailable and unknown recipes", () => {
  for (const recipe of ["claude", "unknown", "", 42]) {
    assert.throws(
      () => siteAdapterForRecipe(recipe),
      (error) => error?.code === "unsupported_recipe" && /before side effects/.test(error.message)
    );
  }
});

test("ChatGPT adapter owns conversation and model policy", () => {
  const adapter = siteAdapterForRecipe("chatgpt");
  assert.deepEqual(adapter.normalizeConversationId(" conv-123 "), { ok: true, id: "conv-123" });
  assert.equal(adapter.conversationUrl("conv-123"), "https://chatgpt.com/c/conv-123");
  assert.equal(adapter.isAcceptableModelSelection({
    status: "selected",
    requested_model: "gpt-5-6-sol-pro",
    family_status: "verified",
    effort_status: "verified",
    model_used: "GPT-5.6 Sol Pro"
  }), true);
  assert.equal(adapter.isAcceptableModelSelection({
    status: "selected",
    requested_model: "gpt-5-6-sol-pro",
    family_status: "verified",
    effort_status: "verified",
    model_used: "GPT-5.6 Instant"
  }), false);
  assert.equal(adapter.completion.supportsBackendApiFallback, true);
  assert.equal(adapter.completion.renderRefreshMode, "reload_conversation");
  assert.equal(adapter.completion.hasFinalAssistantAffordance({
    is_generating: false,
    has_copy_button: true
  }), true);
  assert.equal(adapter.completion.hasFinalAssistantAffordance({
    is_generating: true,
    has_copy_button: true
  }), false);
  assert.equal(typeof adapter.completion.selectFinalAffordanceCandidate, "function");
  assert.equal(typeof adapter.completion.finalAffordanceExtractionFailureMessage, "function");
});

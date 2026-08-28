import assert from "node:assert/strict";
import test from "node:test";
import {
  advertisedRecipes,
  siteAdapterForRecipe
} from "../src/sites/index.js";

test("Wave 2B advertises ChatGPT and Claude atomically", () => {
  assert.deepEqual(advertisedRecipes(), ["chatgpt", "claude"]);
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
  for (const recipe of ["unknown", "", 42]) {
    assert.throws(
      () => siteAdapterForRecipe(recipe),
      (error) => error?.code === "unsupported_recipe" && /before side effects/.test(error.message)
    );
  }
});

test("Claude adapter owns UUID conversations, exact model policy, and DOM-only finality", () => {
  const adapter = siteAdapterForRecipe("claude");
  const conversationId = "123e4567-e89b-12d3-a456-426614174000";

  assert.equal(adapter.recipe, "claude");
  assert.equal(adapter.displayName, "Claude");
  assert.equal(adapter.jobUrl("run 1"), "https://claude.ai/new?_yoetz=run+1");
  assert.equal(adapter.conversationJobUrl(conversationId, "run 1"), `https://claude.ai/chat/${conversationId}?_yoetz=run+1`);
  assert.deepEqual(adapter.normalizeConversationId(conversationId), { ok: true, id: conversationId });
  assert.equal(adapter.normalizeConversationId("conv-123").ok, false);
  assert.equal(adapter.isAllowedTabUrl(`https://claude.ai/chat/${conversationId}`), true);
  assert.equal(adapter.isAllowedTabUrl("https://chatgpt.com/"), false);
  assert.deepEqual(adapter.tabActivation, {
    activateOnCreate: false,
    restorePreviousAfter: null
  });
  assert.equal(adapter.isAcceptableModelSelection({
    status: "selected",
    requested_model: "fable-5-max",
    modelVerified: true,
    maxVerified: true,
    model_used: "Fable 5 Max"
  }), true);
  assert.equal(adapter.isAcceptableModelSelection({
    status: "selected",
    requested_model: "fable-5-max",
    modelVerified: true,
    maxVerified: false,
    model_used: "Fable 5 High"
  }), false);
  assert.equal(adapter.completion.supportsBackendApiFallback, false);
  assert.equal(adapter.completion.renderRefreshMode, "none");
  assert.equal(adapter.completion.finalAffordanceRequiresStableIdle, true);
  assert.equal(adapter.completion.hasFinalAssistantAffordance({
    is_generating: false,
    method: "assistant_dom",
    text: "complete answer",
    has_copy_button: false
  }), true);
  assert.equal(adapter.completion.hasFinalAssistantAffordance({
    is_generating: true,
    method: "assistant_dom",
    text: "still streaming",
    has_copy_button: true
  }), false);
  assert.equal(typeof adapter.dom.extractResponse, "function");
});

test("ChatGPT adapter owns conversation and model policy", () => {
  const adapter = siteAdapterForRecipe("chatgpt");
  const provisionalConversationId = "WEB:ca5209ac-2836-440d-b674-ffc54ee5dd2d";
  const assignedConversationId = "6a5f60dc-8174-8329-949a-1f282d1dccbd";
  assert.deepEqual(adapter.tabActivation, {
    activateOnCreate: false,
    restorePreviousAfter: null
  });
  assert.deepEqual(adapter.normalizeConversationId(" conv-123 "), { ok: true, id: "conv-123" });
  assert.equal(adapter.conversationUrl("conv-123"), "https://chatgpt.com/c/conv-123");
  assert.equal(adapter.isExpectedConversationIdAssignment({
    conversation_id: null,
    submitted_conversation_id: provisionalConversationId
  }, provisionalConversationId, assignedConversationId), true);
  assert.equal(adapter.isExpectedConversationIdAssignment({
    conversation_id: null,
    submitted_conversation_id: provisionalConversationId
  }, provisionalConversationId, null), false);
  assert.equal(adapter.isExpectedConversationIdAssignment({
    conversation_id: null,
    submitted_conversation_id: provisionalConversationId
  }, provisionalConversationId, "WEB:different"), false);
  assert.equal(adapter.isExpectedConversationIdAssignment({
    conversation_id: "conv-requested",
    submitted_conversation_id: provisionalConversationId
  }, provisionalConversationId, assignedConversationId), false);
  assert.equal(adapter.isExpectedConversationIdAssignment({
    conversation_id: null,
    submitted_conversation_id: "WEB:different"
  }, provisionalConversationId, assignedConversationId), false);
  assert.equal(adapter.isAcceptableModelSelection({
    status: "selected",
    requested_model: "gpt-5-6-sol-chat-pro",
    family_status: "verified",
    effort_status: "verified",
    model_used: "GPT-5.6 Sol Pro"
  }), true);
  assert.equal(adapter.isAcceptableModelSelection({
    status: "selected",
    requested_model: "gpt-5-6-sol-chat-pro",
    family_status: "verified",
    effort_status: "verified",
    model_used: "GPT-5.6 Sol Expert"
  }), false);
  assert.equal(adapter.isAcceptableModelSelection({
    status: "selected",
    requested_model: "gpt-5-6-sol-chat-pro",
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

import assert from "node:assert/strict";
import test from "node:test";
import {
  chatgptConversationJobUrl,
  chatgptJobUrl,
  classifyManualHandoff,
  classifyWaitManualHandoff,
  normalizeText,
  ownedWindowName,
  parseOwnedWindowName
} from "../src/chatgpt-dom.js";

test("ownedWindowName round trips run and job ids", () => {
  const job = { run_id: "run_abc", job_id: "job_xyz" };
  assert.deepEqual(parseOwnedWindowName(ownedWindowName(job)), job);
  assert.equal(parseOwnedWindowName("not-yoetz"), null);
});

test("chatgptJobUrl scopes jobs to chatgpt.com with a Yoetz marker", () => {
  assert.equal(chatgptJobUrl("run 1"), "https://chatgpt.com/?_yoetz=run+1");
});

test("chatgptConversationJobUrl scopes resume jobs to a canonical conversation with a Yoetz marker", () => {
  assert.equal(
    chatgptConversationJobUrl("conv-123", "run 1"),
    "https://chatgpt.com/c/conv-123?_yoetz=run+1"
  );
});

test("classifyManualHandoff detects login, challenge, and rate limits", () => {
  assert.equal(classifyManualHandoff({ url: "https://chatgpt.com/auth/login" }).state, "login_required");
  assert.equal(classifyManualHandoff({ text: "Verify you are human" }).state, "challenge_required");
  assert.equal(classifyManualHandoff({ text: "Too many requests, try again later" }).state, "rate_limited");
  assert.equal(classifyManualHandoff({ text: "Message ChatGPT" }), null);
});

test("classifyWaitManualHandoff avoids prompt and response text false positives", () => {
  assert.equal(classifyWaitManualHandoff({ url: "https://chatgpt.com/auth/login" }).state, "login_required");
  assert.equal(classifyWaitManualHandoff({ title: "Too many requests | ChatGPT" }).state, "rate_limited");
  assert.equal(
    classifyWaitManualHandoff({
      extraction: {
        method: "page_text_fallback",
        text: "Too many requests. Please wait a few minutes.",
        user_count: 0,
        assistant_count: 0
      }
    }),
    null
  );
  assert.equal(
    classifyWaitManualHandoff({
      extraction: {
        method: "assistant_dom_fallback",
        text: "A rate limit is HTTP 429.",
        user_count: 1,
        assistant_count: 1
      }
    }),
    null
  );
  assert.equal(
    classifyWaitManualHandoff({
      extraction: {
        method: "page_text_fallback",
        text: "Explain rate limit handling",
        user_count: 1,
        assistant_count: 0
      }
    }),
    null
  );
});

test("normalizeText trims repeated whitespace conservatively", () => {
  assert.equal(normalizeText(" hello \n\n\n world \r\n"), "hello\n\n world");
});

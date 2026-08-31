import assert from "node:assert/strict";
import test from "node:test";
import {
  chatgptConversationJobUrl,
  chatgptJobUrl,
  classifyManualHandoff,
  classifyWaitManualHandoff,
  findAuthenticatedComposer,
  findComposer,
  manualHandoffContext,
  normalizeText,
  ownedWindowName,
  parseOwnedWindowName
} from "../src/chatgpt-dom.js";
import { chatgptSiteAdapter } from "../src/sites/chatgpt.js";
import { claudeSiteAdapter } from "../src/sites/claude.js";

test("ownedWindowName round trips run and job ids", () => {
  const job = { run_id: "run_abc", job_id: "job_xyz" };
  assert.deepEqual(parseOwnedWindowName(ownedWindowName(job)), job);
  assert.equal(parseOwnedWindowName("not-yoetz"), null);
});

test("ownedWindowName binds workspace and ownership nonce when present", () => {
  const job = {
    run_id: "run_abc",
    job_id: "job_xyz",
    workspace_id: "workspace_/release",
    ownership_nonce: "nonce-123"
  };
  assert.deepEqual(parseOwnedWindowName(ownedWindowName(job)), job);
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

test("classifyManualHandoff does not let composer authentication suppress a real handoff", () => {
  assert.equal(
    classifyManualHandoff({
      title: "ChatGPT",
      text: "Security check",
      authenticated: true
    }).state,
    "challenge_required"
  );
});

test("classifyManualHandoff does not match login words inside ordinary words", () => {
  for (const text of [
    "Design integration plan",
    "Blog index redesign",
    "Catalog inventory sync",
    "Assign inbox owners"
  ]) {
    assert.equal(classifyManualHandoff({ text }), null, text);
  }
});

test("classifyManualHandoff recognizes common Cloudflare challenge metadata", () => {
  assert.equal(classifyManualHandoff({ title: "Just a moment..." }).state, "challenge_required");
  assert.equal(
    classifyManualHandoff({
      url: "https://chatgpt.com/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1",
      authenticated: true
    }).state,
    "challenge_required"
  );
});

test("classifyManualHandoff parses routes without scanning conversation ids or query text", () => {
  for (const url of [
    "https://chatgpt.com/c/login",
    "https://chatgpt.com/c/cloudflare",
    "https://chatgpt.com/c/captcha-notes",
    "https://chatgpt.com/c/opaque-id?_yoetz=security-check"
  ]) {
    assert.equal(classifyManualHandoff({ url }), null, url);
  }
  assert.equal(classifyManualHandoff({ url: "https://chatgpt.com/auth/login" }).state, "login_required");
  assert.equal(classifyManualHandoff({ url: "https://chatgpt.com/auth/oauth" }).state, "login_required");
  assert.equal(
    classifyManualHandoff({
      url: "https://chatgpt.com/cdn-cgi/challenge-platform/h/g/orchestrate/chl_page/v1"
    }).state,
    "challenge_required"
  );
});

test("classifyManualHandoff ignores user-derived conversation titles", () => {
  for (const title of ["Rate limit design", "Too many requests", "Just a moment..."]) {
    assert.equal(
      classifyManualHandoff({
        url: "https://chatgpt.com/c/opaque-id",
        title
      }),
      null,
      title
    );
  }
});

test("findAuthenticatedComposer rejects permissive editor fallbacks", () => {
  const genericEditor = visibleElement({ contenteditable: "true" });
  const genericTextbox = visibleElement({ contenteditable: "true", role: "textbox" });
  const genericRoot = selectorRoot(new Map([
    ['div[contenteditable="true"][role="textbox"]', [genericTextbox]],
    ['div[contenteditable="true"]', [genericEditor]]
  ]));

  assert.equal(findComposer(genericRoot), genericTextbox);
  assert.equal(findAuthenticatedComposer(genericRoot), null);

  const chatgptComposer = visibleElement({ id: "prompt-textarea" });
  const chatgptRoot = selectorRoot(new Map([
    ["#prompt-textarea", [chatgptComposer]]
  ]));

  assert.equal(findAuthenticatedComposer(chatgptRoot), chatgptComposer);
});

test("manualHandoffContext suppresses all page text when the strict composer is visible", () => {
  const composer = visibleElement({ id: "prompt-textarea" });
  const main = visibleElement();
  main.innerText = "Rate limit design\nSecurity check\nAsk ChatGPT";
  const root = selectorRoot(new Map([
    ["#prompt-textarea", [composer]],
    ["main", [main]]
  ]));
  root.body = {
    innerText: "New chat\nPre-execution security check\nDesign integration plan\nAsk ChatGPT"
  };
  root.title = "Pre-execution security check";

  assert.deepEqual(manualHandoffContext(root), {
    authenticated: true,
    title: "",
    text: ""
  });
});

test("manualHandoffContext excludes transcript turns and conversation titles", () => {
  const transcript = visibleElement();
  transcript.innerText = [
    "Security check",
    "Log in rollout",
    "Rate limit design"
  ].join("\n");
  transcript.closest = (selector) => selector.includes("[data-message-author-role]") ? transcript : null;
  const safeLeaf = visibleElement();
  safeLeaf.innerText = "Ask ChatGPT";
  const main = visibleElement();
  main.children = [transcript, safeLeaf];
  const root = selectorRoot(new Map([
    ["main", [main]]
  ]));
  root.title = "Rate limit design";
  root.body = { innerText: "Sidebar\nSecurity check\nRate limit design" };

  const context = manualHandoffContext(root);
  assert.equal(context.authenticated, false);
  assert.equal(context.title, "");
  assert.equal(context.text, "Ask ChatGPT");
  assert.equal(
    classifyManualHandoff({
      url: "https://chatgpt.com/c/opaque-id",
      title: context.title,
      text: context.text
    }),
    null
  );
});

test("manualHandoffContext excludes standalone response text beside an assistant marker", () => {
  const assistantMarker = visibleElement({ "data-message-author-role": "assistant" });
  const standaloneAnswer = visibleElement();
  standaloneAnswer.innerText = "Rate limit design";
  const main = visibleElement();
  main.children = [assistantMarker, standaloneAnswer];
  const root = selectorRoot(new Map([
    ["main", [main]],
    ['[data-message-author-role="assistant"]', [assistantMarker]]
  ]));
  root.title = "Rate limit design";
  root.body = {
    innerText: "Rate limit design",
    children: [main]
  };

  assert.deepEqual(manualHandoffContext(root), {
    authenticated: false,
    title: "",
    text: ""
  });
});

test("manualHandoffContext excludes user text from an unrecognized editor variant", () => {
  const genericEditor = visibleElement({ contenteditable: "true" });
  genericEditor.innerText = "Security check";
  const safeLeaf = visibleElement();
  safeLeaf.innerText = "Welcome";
  const main = visibleElement();
  main.children = [genericEditor, safeLeaf];
  const root = selectorRoot(new Map([
    ["main", [main]],
    ['div[contenteditable="true"]', [genericEditor]]
  ]));
  root.title = "ChatGPT";
  root.body = { innerText: "Security check\nWelcome", children: [main] };

  assert.deepEqual(manualHandoffContext(root), {
    authenticated: false,
    title: "",
    text: "Welcome"
  });
});

test("manualHandoffContext does not fall back to sidebar text while shell chrome exists", () => {
  const nav = visibleElement();
  nav.innerText = "Security check\nDesign integration plan";
  const root = selectorRoot(new Map([
    ["nav", [nav]]
  ]));
  root.title = "Security check";
  root.body = { innerText: "Security check\nDesign integration plan" };

  assert.deepEqual(manualHandoffContext(root), {
    authenticated: false,
    title: "",
    text: ""
  });
});

test("manualHandoffContext does not fall back during header-only shell hydration", () => {
  const header = visibleElement();
  header.innerText = "Security check";
  const root = selectorRoot(new Map([
    ["header", [header]]
  ]));
  root.title = "Security check";
  root.body = { innerText: "Security check", children: [header] };

  assert.deepEqual(manualHandoffContext(root), {
    authenticated: false,
    title: "",
    text: ""
  });
});

test("manualHandoffContext recognizes class-token sidebar shell hydration", () => {
  const sidebar = visibleElement({ class: "sidebar" });
  sidebar.innerText = "Security check";
  const root = selectorRoot(new Map([
    ['[class~="sidebar"]', [sidebar]]
  ]));
  root.title = "Security check";
  root.body = { innerText: "Security check", children: [sidebar] };

  assert.deepEqual(manualHandoffContext(root), {
    authenticated: false,
    title: "",
    text: ""
  });
});

test("manualHandoffContext falls back to a document-only interstitial", () => {
  const root = selectorRoot(new Map());
  root.title = "Just a moment...";
  root.body = { innerText: "Just a moment...\nChecking your browser" };

  assert.deepEqual(manualHandoffContext(root), {
    authenticated: false,
    title: "Just a moment...",
    text: "Just a moment...\nChecking your browser"
  });
});

test("every advertised site adapter implements the manual handoff context contract", () => {
  for (const adapter of [chatgptSiteAdapter, claudeSiteAdapter]) {
    assert.equal(typeof adapter.dom.manualHandoffContext, "function", adapter.recipe);
  }

  const claudeComposer = visibleElement({ "data-testid": "chat-input" });
  const claudeRoot = selectorRoot(new Map([
    ["[data-testid='chat-input']", [claudeComposer]]
  ]));
  claudeRoot.title = "Claude";
  claudeRoot.body = { innerText: "Welcome to Claude" };
  assert.deepEqual(claudeSiteAdapter.dom.manualHandoffContext(claudeRoot), {
    authenticated: true,
    title: "Claude",
    text: "Welcome to Claude"
  });
});

test("classifyWaitManualHandoff avoids prompt and response text false positives", () => {
  assert.equal(classifyWaitManualHandoff({ url: "https://chatgpt.com/auth/login" }).state, "login_required");
  assert.equal(classifyWaitManualHandoff({ title: "Just a moment..." }).state, "challenge_required");
  assert.equal(classifyWaitManualHandoff({ title: "Too many requests | ChatGPT" }).state, "rate_limited");
  assert.equal(
    classifyWaitManualHandoff({
      url: "https://chatgpt.com/c/opaque-id",
      title: "Security check | ChatGPT"
    }),
    null
  );
  assert.equal(
    classifyWaitManualHandoff({
      url: "https://chatgpt.com/c/opaque-id",
      title: "Just a moment...",
      text: "Checking your browser"
    }).state,
    "challenge_required"
  );
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

function selectorRoot(selectors) {
  return {
    querySelector(selector) {
      return selectors.get(selector)?.[0] ?? null;
    },
    querySelectorAll(selector) {
      return selectors.get(selector) ?? [];
    }
  };
}

function visibleElement(attributes = {}) {
  return {
    disabled: false,
    hidden: false,
    ownerDocument: {
      defaultView: {
        getComputedStyle() {
          return {};
        }
      }
    },
    parentElement: null,
    checkVisibility() {
      return true;
    },
    getAttribute(name) {
      return attributes[name] ?? null;
    },
    getClientRects() {
      return [{}];
    }
  };
}

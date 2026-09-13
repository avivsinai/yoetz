# ChatGPT web rate-limit fix — point-in-time proposal (yz-5bd)

Status: PROPOSED, awaiting codex ratification. Owner: amit-pi. Reviewer: codex.
Worktree: /Users/aviv.s/workspace/yoetz-rl-fix (branch fix/chatgpt-web-rate-limit, from main a4f0f2d).

## Incident evidence

- No network trace exists for any incident. The only artifacts are run
  transcripts classified post-hoc: field runs surfaced the "Too many requests"
  modal and were relabeled `rate_limited` by gh-471/#493 (yz-i1h, closed).
  We therefore CANNOT claim a root cause; we propose the smallest defensible
  fix on the path codex already flagged, plus typed evidence for the next run.
- The modal text ("You're making requests too quickly. We've temporarily
  limited access to your conversations to protect your data") is OpenAI's
  web-session request-rate protection on chatgpt.com routes — not public-API
  quota, not the monthly usage wall (already classified separately as
  usage_limit_reached in #497).

## Our request-bearing paths (inspection result)

1. Website conversation reads (backend-api fallback):
   - `extensions/chatgpt-native/src/sites/chatgpt-backend.js:53-68` —
     `fetchChatgptAccessToken()` does `GET /api/auth/session` before EVERY
     conversation read; the token is never cached. Successful fallback poll
     = 2 requests.
   - `chatgpt-backend.js:20-38` — `GET /backend-api/conversation/<id>`;
     429/403 collapse into generic `backend_api_unavailable`; Retry-After
     is discarded. Auth 429 becomes `backend_api_unauthorized`.
   - `extensions/chatgpt-native/src/service-worker.js:3951-3985` —
     `BACKEND_API_FETCH_COOLDOWN_MS` (60s) is a **per-job** field
     (`job.backend_api_last_fetch_at`); concurrent jobs on one Chrome
     profile multiply traffic on the same account.
   - `service-worker.js:3733, BACKEND_API_CONFIRMATION_MS=5000` — the
     confirmation re-read **bypasses the cooldown** and re-arms on every
     new answer node (`service-worker.js:4007`), so the worst case is not
     bounded by the 60s gate.
2. Page-owned traffic in a hidden tab: `page-visibility-shim.js:103-137`
   delivers one synthetic IntersectionObserver entry per newly observed
   target for the first 90s; the guard is per observer/target, not a total
   budget, so sidebar/media lazy loaders can fire in a tab where Chrome
   would deliver none. NOT changed in this fix (no evidence it fired;
   changing hydration risks breaking hidden-tab finality). Left as a
   follow-up pending measurement.
3. Render refresh (`service-worker.js:4332`) — one bounded navigation;
   not the burst source. Unchanged.
4. DOM polling (`extractResponse`, 1.5–30s adaptive) — local reads only,
   no HTTP. Unchanged by design; codex ruled this out as a cause.

## Proposed minimal fix (two files)

### A. `sites/chatgpt-backend.js` — make a website throttle visible

1. Preserve status + Retry-After from BOTH routes:
   - conversation fetch: on 429 (and 403-with-Retry-After), throw
     `backend_api_throttled` with `retry_after_ms` (parsed from the
     Retry-After header, seconds-form and HTTP-date-form), `http_status`,
     and the route name. Distinct from `backend_api_unauthorized` (401/403
     without Retry-After) and `backend_api_unavailable`.
   - session fetch: a 429 on `/api/auth/session` must NOT return null
     (currently misread as signed-out). Throw the same typed
     `backend_api_throttled`.
2. Cache the access token in module scope for the content-script instance:
   `{ token, fetchedAt }`, TTL 5 minutes, invalidate on any 401 from the
   conversation route. Never persisted, never logged. This halves every
   successful fallback poll (2 requests → 1).

### B. `service-worker.js` — one account-scoped gate covering confirmation reads

1. Replace the per-job cooldown keying with a **profile-scoped** gate kept
   on `chrome.storage.session` under e.g. `backend-api-gate.<recipe>`:
   `{ last_fetch_at_ms, throttle_until_ms, recent_failures }` — shared by
   all jobs in the profile (one ChatGPT account).
2. The confirmation path (`backend_api_confirmation` due) goes through the
   same gate: a confirmation read is allowed only if the gate's minimum
   inter-read gap has elapsed; if the last read was <5s ago, the
   confirmation just waits for the next poll tick. Worst case becomes
   ~1 conversation request per 5s across the whole account, and only
   while a backend answer is being confirmed (bounded re-arms: cap
   consecutive confirmation re-arms at 3 before falling back to DOM
   finality wait, mirroring MAX_BACKEND_API_CONSECUTIVE_FAILURES).
3. On `backend_api_throttled`: set `throttle_until_ms = now +
   max(retry_after_ms, 60s)` with exponential growth (60s → 120s → 240s,
   jittered ±20%) for repeated throttles; during throttle, ALL backend-api
   reads for the account are skipped and `backend_api_pending` holds the
   DOM-barred state as today (finality semantics unchanged — we do NOT
   complete on DOM-only while the positive anchor is pending).
4. A sustained throttle (>10 minutes) disables the fallback
   (`backend_api_disabled` as today) and the run keeps the existing
   DOM-only warning path.

## Explicitly out of scope

- No DOM-poll cadence changes; no synthetic-click "humanization"; no
  changes to fresh-tab-per-request; no visibility-shim narrowing (pending
  measured evidence; follow-up bead if ratified).
- No auto-resend of prompts on recovery (existing fail-closed preserved).

## Acceptance (per codex)

- One paced real ChatGPT web recipe still uploads, selects Latest Pro,
  sends once, returns the completed answer.
- Regression test (fast in-process harness): a mocked 429 + Retry-After on
  the conversation route produces `backend_api_throttled`, sets the shared
  gate, and the next job's read (including a due confirmation) is deferred
  until the throttle window passes.
- Happy-path test: token cache serves the second read without a second
  `/api/auth/session` request.
- No resubmission of sent conversations during recovery (unchanged paths).

## Test harness

Existing: `extensions/chatgpt-native/tests/service-worker.test.js`
(chromeStub + in-process worker), `content-script.test.js`
(fake-site adapter via data: URL modules). Both run under node --test with
jsdom; no network. New tests ride the existing harness.

---

## RATIFIED (codex amendments, 2026-09-12) — supersedes the sections above

1. **Finality preserved.** No DOM-only fallback after N confirmation re-arms
   and none after sustained throttle. A changed answer node stays pending and
   returns to ordinary polling; a throttle stays pending until the server
   delay expires or the existing run deadline ends. Deadline expiry returns
   an honest error and keeps the sent conversation.
2. **HTTP facts.** Explicit 429 handled on both routes. 403 semantics
   unchanged (unauthorized) — no inferring throttle from 403 without
   evidence. 401/403 keep existing meaning. Preserve `http_status`,
   `retry_after_ms`, fixed endpoint category through the content-script
   errorResponse allowlist and the service-worker tabCommandError /
   errorContextForJob allowlists; include the delay in progress/error text.
3. **Pacing.** Keep each job's 60s ordinary read interval. Add one
   profile-scoped shared gate: one in-flight read, >=5s between operations.
   At most one early confirmation per ordinary polling cycle per job; if the
   node changes, continue at ordinary cadence, never accept DOM finality.
   Serialize reservation before async work (storage read-then-write is not a
   mutex). Restore gate state before admitting work; a skipped job is not
   charged as if it fetched.
4. **429 cooldown coverage.** On explicit 429 the shared cooldown covers
   conversation/auth reads, NEW automated tab creation, and render-refresh
   navigation in this profile. A recognized DOM `rate_limited` result sets
   the same cooldown; terminal handling of that job unchanged. Cancellation
   and deadlines stay effective while other jobs wait. Retry-After is a
   floor; jitter only lengthens. 60/120/240s are our configurable fallback,
   not a published ChatGPT limit.
5. **Naming: profile-scoped**, not account-global (other Chrome
   profiles/devices on the same account are not coordinated). Token caching
   DEFERRED (TTL does not establish logout/account-switch invalidation).
6. **Evidence corrected:** 403 currently maps to unauthorized; render
   refresh has NOT been ruled out as incident source; local DOM reads do not
   fetch but page-owned effects from other actions are not excluded. Neither
   a root cause nor a safe website rate is established.
7. **Tests:** one happy path for pause/resume completing with the same
   answer and one send; metadata/scheduling behavior tests on the existing
   in-process harness; simulated 429 labeled as such (not incident
   reproduction). Full paced live recipe still required before claiming the
   incident fixed; live run coordinated before touching the shared extension.

Task/status checklist lives in bead yz-5bd.

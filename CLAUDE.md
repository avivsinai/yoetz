# Yoetz

CLI-first LLM council, bundler and multimodal gateway for coding agents. Rust
workspace (`crates/yoetz-core`, `crates/yoetz-cli`, binary `yoetz`) plus a
Chrome extension (`extensions/chatgpt-native/`). The design and its invariants
are in `ARCHITECTURE.md`; user docs are in `README.md`; the agent skill is
`skills/yoetz/`. `AGENTS.md` is a symlink to this file.

## Develop

- Rust: `cargo build`, `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`
  (exactly what CI runs). Rust 2021, MSRV 1.88, `anyhow` in the CLI,
  `thiserror` in libraries, `tokio`.
- Extension: `npm ci --prefix extensions/chatgpt-native --ignore-scripts`
  once (jsdom is required), then
  `node --test extensions/chatgpt-native/tests/<file>.test.js` and
  `npm run lint --prefix extensions/chatgpt-native`. CI uses Node 24.
- Run only the test files you changed locally; full suites run in CI. Tests
  need no API keys.
- Attributes stack onto the next item: never insert a test between a
  `#[cfg(...)]`/`#[serial]` attribute and its function.
- Branch work goes in worktrees under `~/workspace/yoetz-wt/`; scratch files
  under `~/workspace/yoetz-wt-scratch/`, never `/tmp`. The shared checkout stays
  on `main`.
- Tasks live in beads: `bd update <id> --status in_progress` to claim. `bd`
  does not expand `@file`; pass `--description "$(cat <file>)"`.

## Changelog and release

- Every user-visible change adds a line under `## [Unreleased]` in
  `CHANGELOG.md` in the same PR (a CI check enforces the placement). That
  section becomes the GitHub release notes.
- Release only from `main` with `./scripts/release.sh --skip-verify X.Y.Z`,
  then merge the PR it opens (auto-merge is on). Never create tags or GitHub
  releases by hand; never push to `main`.
- Run `cargo fetch` first: the script updates `Cargo.lock` offline and fails on
  a dependency it has not downloaded.
- The script bumps every version stamp (workspace, plugin manifests, skill
  frontmatter, extension manifest, content-script build ids) and leaves the
  checkout on the release branch: `git switch main` afterwards.
- Merging `chore(release): vX.Y.Z` to `main` makes `release.yml` tag, publish
  artifacts (including the extension zip), and update Homebrew and Scoop.
  `workflow_dispatch` retries an existing tag.
- Branch protection needs an up-to-date branch: on "Head branch is out of
  date", run `gh pr update-branch <n>`, wait for CI, then merge. Never `--admin`.
- Gitleaks scans the whole PR range. A secret-shaped literal (including a UUID
  next to a `key`-like attribute in a fixture) must be replaced and the
  introducing commit rewritten; a later deleting commit does not clear it.

## Tests

- One small happy-path test per user-visible behavior, plus a regression test
  for each defect seen in the field or CI. No speculative negative matrices,
  guard tests or test-only PRs.
- A regression test must fail on `main`; check that yourself before you claim
  it. The failure must be the defect: a named assertion, or a timeout only when
  "nothing arrives" is the defect itself.
- A flaky or slow test is a defect: rewrite it to the bar or delete it.

## Browser recipes: rules agents break

- Native extension first: when `extension status` reports `connected`, built-in
  recipes run only on `chrome-extension-native` and fail closed. Fall back to
  CDP only with `--transport chrome-extension-native --allow-cdp-fallback`.
- Never rerun a recipe that failed after a side effect (upload or send started)
  unless you intend a duplicate. Inspect first:
  `yoetz browser extension inspect --chatgpt --run-id <run>`.
- Never hand-patch `$YOETZ_DIR/chatgpt-native-extension`, and never load a repo
  checkout in Chrome. Use `yoetz browser extension setup|update --chatgpt|--claude`.
  After `update`, wait for `status` to show the new version before a run.
- Several connected Chrome profiles: route with
  `--var extension_instance_id=<ext_…>` (stable across reloads).
- Parallel recipes need distinct bundle session directories (`session_busy`
  otherwise).
- The live-attach daemon is trusted: never recycle it in normal flows; recovery
  is `yoetz browser reset`.
- Treat yoetz as a thin wrapper over the transport. Own behavior in yoetz only
  when correctness or UX needs it.
- Live tests use only the personal account (`avivsinai@gmail.com`, instance
  `ext_937d13fd52980082ed530899`), never the enterprise account.

### Live-run safety

- Pace live runs. ChatGPT's "Too many requests" modal limits conversation
  history reads; every page load is one, and probing whether it cleared keeps
  it alive.
- A typed `rate_limited`, or a `rate_limit_cooldown_active` refusal, is a hard
  stop: end the loop and ask a human. Never back off and retry, and never key a
  watchdog on "no text produced".
- `effort_options_disabled` means the account's Pro quota lock is on. It flips
  from day to day; read it from one run, never from a probe loop.
- `tab_pacing_active` with `min_gap` is waited out by the CLI; `max_concurrent`
  fails, so retry after a running job finishes.
- Send recipe stderr to a file (`2> run.log`) and inspect by run id; never pipe
  a long run through `tail`, and never kill a run before checking whether
  ChatGPT is still generating.
- Read-only probes of a live tab: `osascript` → `tell application "Google Chrome"`
  → `execute <tab> javascript`. It needs no CDP approval and is not a request
  to OpenAI. Use it to check generation state (a Stop control or a
  `[data-streaming-response-status]` turn), to recover a finished answer, and
  to capture fixtures. Never click, type or navigate through it.
- Captured DOM goes through `src/capture-sanitizer.js` (strips scripts, styles,
  input values and secret-shaped attributes, redacts JWTs). Replace UUIDs with
  placeholders before committing a fixture.
- Phrase security-review prompts as verification of fixes and defensive
  hardening, not as bypass discovery; the other phrasing has triggered
  ChatGPT's usage-policy filter (`content_policy_flagged`).

### ChatGPT DOM drift

ChatGPT changes its markup without notice; a drift shows up as a typed
fail-closed error (`model_family_not_found`, upload `attached=false`, a finished
answer "waiting for final assistant controls").

1. Capture the live DOM of the failing tab and of a fresh chat: they can
   differ (the family is `Latest` on a conversation page and `GPT-6` on a fresh
   chat). Ways to capture: `yoetz browser extension dump-picker --run-id` or
   `dump-conversation --run-id|--tab-id` (the job record or its `_yoetz` tab
   must still exist), `scripts/capture-chatgpt-picker.mjs` (raw CDP), or the
   read-only osascript probe.
2. Add the sanitized fixture under `tests/fixtures/chatgpt-picker/` (with an
   `expectations.json` row and `_provenance`) or
   `tests/fixtures/chatgpt-conversation/` (with a provenance note).
3. The fixture test must fail on `main`. Fix the reader or extractor, not the
   test.
4. Picker changes: `node scripts/picker-reader-parity.mjs` must exit 0, and add
   a fake-page `configureModelState` test that asserts `status: "selected"` and
   `model_used: "Latest Pro"`. A reader fixture alone does not prove the
   selection contract.
5. One paced live run on the installed build.

## dev-browser transport (`crates/yoetz-cli/src/dev_browser.rs`)

dev-browser is a QuickJS/WASM runner, not Node:

- Keep generated scripts small and linear; no nested async helpers or
  closure-heavy control flow. Orchestrate micro-scripts from Rust.
- Carry state across scripts with named pages (`browser.getPage(name)`,
  `browser.listPages()`); use `console.log(JSON.stringify(...))` as the IPC.
- Use only the locator verbs the QuickJS bridge supports (the script-source
  lint enforces them). No `require`, `fs` or `fetch`.
- Type into contenteditable inputs with `pressSequentially`, not `fill()`.
- File upload: QuickJS cannot drive `setInputFiles`, so dev-browser falls back
  to a macOS clipboard paste via `osascript` (inline paste elsewhere). Always
  report the real `delivery_mode` and `auto_paste_fallback`. The native and
  CDP transports upload directly and reject `--var paste=true`.
- Correctness must not depend on the QuickJS GC-crash stdout salvage.

## Providers

API keys come from environment variables; the README lists them and the config
file locations. The `cursor` backend runs `cursor-agent` in read-only Ask mode
in a temporary yoetz-owned workspace: keep it behind the shared
ask/review/council dispatcher, never point it at the real repository, and never
pass Cursor force, YOLO or MCP-approval flags.

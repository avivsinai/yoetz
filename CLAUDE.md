# Yoetz

Fast CLI-first LLM council, bundler, and multimodal gateway for coding agents.

This is the master agent instruction file for this repository. Keep repository policy here. `AGENTS.md` exists only as a Codex compatibility shim and should contain only Codex-specific notes.

## Project Structure

Rust workspace with two crates:
- `crates/yoetz-cli` - CLI binary (`yoetz`)
- `crates/yoetz-core` - Core types, bundling, session management

External dependency: [litellm-rust](https://github.com/avivsinai/litellm-rust) - Multi-provider LLM SDK

## Development

```bash
cargo build                    # Build all crates
cargo test                     # Run all tests
cargo fmt                      # Format code
cargo clippy                   # Lint
```

Tests use `assert_cmd`, `predicates`, and `serial_test` — no API keys needed for `cargo test`.

## Release

- Release from `main` only through `./scripts/release.sh [options] X.Y.Z`
  (the parser consumes options before the positional version) and the
  resulting release PR. Never create
  manual tags or GitHub releases; never push directly to `main`.
- Populate `## [Unreleased]` in `CHANGELOG.md` BEFORE running the script: the
  release commit's changelog section becomes the GitHub release notes.
- The script moves the Unreleased section, bumps and aligns ALL version
  metadata (workspace, plugin.json files, SKILL.md frontmatter, extension
  manifest — CI validates consistency), runs `cargo check --workspace`, pushes
  `release/vX.Y.Z`, and opens the PR with `gh`.
- After the release PR merges, `release.yml` detects `chore(release): vX.Y.Z`
  on `main`, creates the tag, publishes artifacts (including the ChatGPT
  native extension zip when the extension source is present), and updates
  Homebrew/Scoop. `workflow_dispatch` is the retry path for an existing tag.
- A push to `main` also updates the AvivSinai marketplace immediately for the
  `yoetz` skill.
- We deliberately keep this custom release flow over release-plz/release-please:
  the repo ships GitHub artifacts plus Homebrew/Scoop, not crates.io, and the
  merged release commit driving the whole pipeline is the fastest fit.

## Code Style

- Rust 2021 edition, MSRV 1.88
- Use `anyhow::Result` for CLI, `thiserror` for library errors
- Async with `tokio`
- Follow existing patterns in the crate you're modifying

## dev-browser Recipe Constraints

When editing `crates/yoetz-cli/src/dev_browser.rs` or adding new ChatGPT/browser
recipe flows, treat `dev-browser` as a QuickJS/WASM runner, not Node.js:

- The sandbox is QuickJS. Keep recipe scripts small and linear.
- Avoid large generated scripts, nested async helpers, or closure-heavy control
  flow. Prefer micro-scripts orchestrated from Rust.
- Use named pages via `browser.getPage(name)` / `browser.listPages()` to carry
  state across scripts.
- Use `console.log(JSON.stringify(...))` as the script-to-Rust IPC boundary.
- Keep generated scripts within the locator verbs supported by the QuickJS
  bridge; the script-source lint is the compatibility lock for that surface.
- Prefer Playwright actions on the page plus Rust orchestration. Do not assume
  Node features such as `require`, arbitrary `fs`, or `fetch`.
- For contenteditable ChatGPT inputs, use typing APIs such as
  `pressSequentially` instead of `fill()`.
- For file upload, primary transports use first-class APIs: the
  `chrome-extension-native` transport streams the bundle over native messaging
  chunks, and `chrome-devtools-mcp` uses CDP `upload_file` (the transport
  explicitly rejects `--var paste=true`, per `crates/yoetz-cli/src/main.rs`).
  Only the `dev-browser` (QuickJS) transport still falls back to macOS
  clipboard paste via `osascript` because QuickJS cannot drive
  `setInputFiles`; this is a dev-browser-specific workaround and is not the
  default upload path. Non-macOS dev-browser runs degrade to inline paste.
  Always report the actual `delivery_mode` and `auto_paste_fallback`, including
  inline fallback when a clipboard gesture produces no upload.
- The QuickJS GC crash recovery in `dev_browser.rs` can salvage stdout from a
  completed script, but recipe correctness must not depend on that recovery.

## Browser Architecture

- The built-in ChatGPT recipe selects the `Latest` family with the literal
  `Pro` effort tier on the Chat surface (requested model id `gpt-6-pro-chat`,
  `model_used` `Latest Pro`; never Sol, never the Faster/Smarter power slider,
  which stays at the user's default). It first verifies the Chat surface
  radiogroup and forces the `Chat` radio, then drives the composer model pill
  (`findModelButton` prefers a grammar-matched pill — including the `6 Pro`
  generation-plus-effort form — then a family-token pill, then any visible
  `__composer-pill`; the mid-flow `Thinking effort` label is a live pill
  string, not a decoy). A click is never proof: after every mutation the
  picker is reopened via `findModelButton` and re-read; after close, the
  composer pill must corroborate Pro effort (`Pro`, a family-plus-`Pro`
  suffix, or the `6 Pro` form) — family is never inferred from the pill, so a
  Sol-worded pill fails closed. Missing controls, Work mode, a Sol-only menu,
  another family, another effort tier, and disabled tier rows
  (`effort_options_disabled` — the account quota lock keeps the rows mounted
  with `aria-disabled` + `data-disabled`) all fail closed, in both the
  selection and post-close reverification legs. Already-Latest already-Pro is
  verify-only.
- The picker read is separated from the picker driver.
  `src/chatgpt-picker-reader.js` exports `readPicker(root, { pill,
  leftoverTriggers, familySurface })` → `PickerRead` (shape, trust, family
  label/options, effort label/options, surface, nav, diagnostics); it never
  locates anything itself — pill and leftover triggers are inputs — and never
  mutates the DOM. `src/chatgpt-dom.js` is the driver: it locates the pill
  and triggers, opens/expands/closes surfaces, and builds
  `selectionFailure` / `closedPillDiagnostics` / `pickerCloseVerification`
  from the PickerRead value. Observed picker shapes: the simple menu, the
  legacy advanced slider, the hybrid simple-view (effort slider plus inline
  family radios), the unified list (tier rows beside the family radios), and
  the personal picker. Per-shape DOM rules live in the fixtures and tests, not
  here: `extensions/chatgpt-native/tests/fixtures/chatgpt-picker/*.html` +
  `expectations.json`, and `extensions/chatgpt-native/tests/fake-chatgpt.test.js`
  — consult those before extending classification. Classification is structural
  (`aria-expanded`, `data-state`, `inert`, `aria-controls`), never opacity,
  because background tabs never animate; a retained closed menu keeps its
  toggle mounted with a stale `aria-expanded="true"`, counted as open only
  inside an open surface.
- Drift procedure — when ChatGPT changes the picker: (a) capture the live DOM
  into `extensions/chatgpt-native/tests/fixtures/chatgpt-picker/<date>-<shape>.html`
  via `scripts/capture-chatgpt-picker.mjs` (raw CDP), or — when raw CDP is
  wedged — through the native channel: `yoetz browser extension inspect
  --dump-picker-html <PATH> --chatgpt --run-id <run>` (rides the
  native-messaging host; refuses a live job unless `--allow-live-job` is set,
  re-reads the surface after Escape and reports `closed_after_dump`); (b) add
  an `expectations.json` row with `_provenance`; (c) run
  `node --test tests/chatgpt-picker-reader.test.js` — it fails on the new
  fixture; (d) fix the reader only; (e) `node ../../scripts/picker-reader-parity.mjs`
  from `extensions/chatgpt-native` must exit 0 (baseline pinned to 6aaa07f;
  rows the baseline predates print INFO); (f) one paced live run; family must
  verify, and effort is read from the run — Pro selected, or
  `effort_options_disabled` while the account quota lock holds. Reconstructed
  fixtures are weaker than live captures; prefer a capture whenever the DOM is
  reachable.
- Hidden-tab facts, kept: ChatGPT defers hydration in hidden tabs; a MAIN-world
  visibility shim (injected only into `?_yoetz=` tabs, stamped `data-yoetz-shim`
  at document start, `data-yoetz-hydrated` on `<html>` once a composer menu
  trigger carries a React fiber) makes them hydrate. Model selection waits for
  the composer pill to settle before touching the page; with the shim marker
  present the gate waits for `data-yoetz-hydrated`, reserving a short
  node-stability window at the end of the same total budget (stability alone
  is never a hydration proof), never exceeds `hydration_timeout_ms`, and
  reports `hydration_signal`. The shim also fires IntersectionObserver
  entries (once per target, only while genuinely hidden, only during the
  first 90s) and backs `requestIdleCallback` with a shrinking timer slice in
  yoetz tabs — Chrome never delivers them to background tabs and the Chat/Work
  header mounts lazily behind them; native delivery is never suppressed.
  Radix opens on pointerdown and a trailing click on the open trigger toggles
  it closed, so the activation gesture aborts on a mounted-open menu —
  pointerdown always goes out first, so a leftover menu can never suppress
  activation. Open leftover picker surfaces are closed structurally
  (`aria-expanded` / `data-state`), including opacity-0 menus in background
  tabs. ChatGPT rate-limits accounts that open many automation tabs quickly
  ("Too many requests" modal) and it surfaces as composer/surface not found,
  so pace live verification runs.
- Treat yoetz as a thin wrapper over the underlying browser transport unless
  yoetz must own behavior for correctness or UX.
- Extension-free by default. Preferred live-Chrome transport order:
  `chrome-devtools-mcp`, then `dev-browser`, then `agent-browser`.
- The `claude` recipe mirrors `chatgpt` end to end through the typed contract in
  `crates/yoetz-cli/src/claude_recipe.rs` and DOM builders in `claude_web.rs`.
  Its only model target is Fable 5 + Effort Max. Selection is fail-closed:
  re-read the model radio and `effort-option-max`; a successful click is never
  proof. Claude's July 2026 picker has no independent Thinking control; the
  effort scale now expresses reasoning depth and Max is the strongest option.
- Built-in web-recipe exception: when extension status for the selected site
  reports `connected`, `chrome-extension-native` is auto-selected as the only
  default transport and fails closed instead of falling through to CDP
  transports. Opt out with `--transport <other>` or a pinned `transports:` in
  the recipe yaml; CDP fallback after a native failure requires the explicit
  `--transport chrome-extension-native --allow-cdp-fallback`. Other recipes
  and unhealthy/missing extensions are unaffected. Plain `yoetz browser check`
  remains ChatGPT-scoped; use `--claude` for Claude. Pass an explicit
  `--transport`, `--cdp`, `--browser-id`, or `--profile` to verify the CDP
  stack instead.
- The native extension is one pinned multi-site package
  (`extensions/chatgpt-native/`, display name "Yoetz Native Transport") with
  adapters under `src/sites/`. `job_start.payload.recipe` selects `chatgpt` or
  `claude` (missing means ChatGPT; unknown fails before side effects).
  Extension `hello` advertises `recipes`; derive `claude_ready` from that
  capability, never from version comparison.
- Extension lifecycle: `setup --chatgpt` or `setup --claude` materializes the
  same packaged source into `$YOETZ_DIR/chatgpt-native-extension`. Load that
  exact directory unpacked in the Chrome profile that hosts the AI sessions;
  `chrome://extensions` load state is profile-specific. Never load a repo
  checkout. `update` re-syncs the managed copy with a stamped identity, reloads
  it, and verifies the loaded version. `status` and `doctor` fail on wrong-path
  or unstamped loads. Never hand-patch the managed directory.
- The native host and managed extension directory are machine-global,
  single-writer state shared by every agent lane. Recipe runs hold the shared
  side of the lifecycle lock; setup/update/reload/auto-heal require the
  exclusive side and fail closed if a recipe is active. Recipe lanes may run
  only against one frozen loaded artifact.
- Independent ChatGPT and Claude recipes may run concurrently through one
  connected extension profile: each job owns a separate background tab. Profile
  selectors route among loaded profiles; they are not required for recipe
  parallelism.
  Every parallel recipe must use a distinct Yoetz bundle session directory;
  reusing one managed `bundle.md` fails with `session_busy` before browser work.
- ChatGPT and Claude conversation resume use
  `--var conversation=<site-specific-id|url>` or `--followup`
  (native-extension only, no automatic context management); callers own the
  resume-vs-fresh decision.
- Claude attachments may be inline or retrieval-backed. Warn, do not fail,
  above the `inline_warn_tokens` estimate, and do not raise Yoetz's byte caps:
  the quality cliff is tokens in context, not file bytes. Claude's upload input
  is hidden/zero-size, so resolve the exact selector rather than accessibility
  snapshots; use the native `input.files` setter so page-world handlers observe
  files assigned from the extension's isolated world.
- Claude jobs open in background tabs. On released `v0.5.42`, a live run proved
  exactly two concurrent Claude recipes in one connected profile on an
  Enterprise workspace account. The jobs overlapped for 125s and used distinct
  conversations. Both verified Fable 5 and Effort Max. Tab non-activation has
  separate evidence: the released adapter sets `activateOnCreate:false`, a
  single-job live probe measured `tab_active=false` at every phase, and
  service-worker coverage asserts that no tab activation call occurs. The
  concurrency evidence covers two jobs, not higher fanout or other account
  types. Service-worker coverage separately proves two Claude jobs use distinct
  background tabs through overlapping phases and that cancelling one does not
  affect the other.
  Base UI picker choreography needs settle pacing, attributed pointer-event
  hover fallback, and diagnostics for every verification leg. Early-exit and
  full-selection results must have the same acceptable shape.
- Claude finality requires the last assistant turn to be non-streaming and no
  `Stop response` control; the CDP path adds a bounded no-progress failure,
  while the native path fails at the response deadline. The copy control
  is hover-dependent and is not a primary finality anchor. Exclude cloned
  `group/status` thinking rows from response text without mutating the live DOM.
- Multiple loaded extension profiles route by `profile_email` when Chrome
  exposes it, else by the stable `extension_instance_id` from
  `status --chatgpt` or `status --claude`.
- Default mode is connect-first: attach to the user's already running Chrome
  before considering cookie sync or managed-profile fallbacks.
- The daemon is trusted by default. Do not silently recycle live-attach
  daemons during normal attach/check/recipe flows; recovery is an explicit
  `yoetz browser reset`.

## Provider Configuration

API keys via environment variables:
- `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`
- `OPENROUTER_API_KEY`, `XAI_API_KEY`

Config file: `~/.config/yoetz/config.toml` (optional)

Local CLI backend:
- `cursor` resolves models from authenticated `cursor-agent models` output and
  runs text consults in read-only Ask mode inside a temporary Yoetz-owned
  workspace. Keep this path behind the shared ask/review/council dispatcher;
  never trust the real repository or pass Cursor force/YOLO/MCP approval flags.

## litellm-rust (external)

The [`litellm-rust`](https://github.com/avivsinai/litellm-rust) crate (separate repo) provides unified access to multiple LLM providers:
- `LiteLLM::completion()` - Chat completions
- `LiteLLM::embedding()` - Text embeddings
- `LiteLLM::image_generation()` - Image generation
- `LiteLLM::video_generation()` - Video generation (Gemini)

Model routing: use `provider/model` format (e.g., `openrouter/anthropic/claude-sonnet-4-5`) or configure a default provider.

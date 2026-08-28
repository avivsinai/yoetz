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

- The built-in ChatGPT recipe pins GPT-5.6 Sol at Chat effort Pro. The recipe
  first verifies `role=radiogroup[aria-label="Select chat surface"]`
  and forces the `Chat` radio (`data-tpp-toggle-value="chatgpt"`), then selects
  the composer model pill. `findModelButton` prefers a grammar-matched pill,
  then a family-token pill, then any visible `__composer-pill` — including a
  closed label of `Thinking effort`, which is a live model-pill string, not a
  decoy. Default mapping: select `GPT-5.6 Sol` with the literal `Pro` effort
  tier. The current ChatGPT simple-view picker is a hybrid surface: an effort
  slider whose snapshot parses (`Pro, 5 of 5.`) plus inline family radios
  (`GPT-5.6 Sol` / `GPT-5.5`) in the same menu, with no `Advanced`/`Effort`
  gating text. Classify that as the existing slider shape (legacy Advanced
  view remains an OR). Family proof is the checked inline `GPT-5.6 Sol`
  radio when present, else the Model-row submenu. A successful click is never
  proof: reopen through `findModelButton` and re-read both legs. Already-Sol
  already-Pro is verify-only. Missing controls, Work mode, another family, or
  another effort tier fail closed. The requested model id is
  `gpt-5-6-sol-chat-pro`; `model_used` is `GPT-5.6 Sol Pro`. Speed and the
  Faster/Smarter power slider stay at the user's default. Open leftover picker
  surfaces are closed structurally (`aria-expanded` / `data-state`), including
  opacity-0 menus in background tabs.
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

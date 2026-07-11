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
- The QuickJS GC crash recovery in `dev_browser.rs` can salvage stdout from a
  completed script, but recipe correctness must not depend on that recovery.

## Browser Architecture

- Treat yoetz as a thin wrapper over the underlying browser transport unless
  yoetz must own behavior for correctness or UX.
- Extension-free by default. Preferred live-Chrome transport order:
  `chrome-devtools-mcp`, then `dev-browser`, then `agent-browser`.
- ChatGPT recipe exception: when `yoetz browser extension status --chatgpt`
  reports `connected`, `chrome-extension-native` is auto-selected as the only
  default transport and fails closed instead of falling through to CDP
  transports. Opt out with `--transport <other>` or a pinned `transports:` in
  the recipe yaml; CDP fallback after a native failure requires the explicit
  `--transport chrome-extension-native --allow-cdp-fallback`. Non-ChatGPT
  recipes and unhealthy/missing extensions are unaffected. Plain
  `yoetz browser check` follows the same auto-selection; pass an explicit
  `--transport`, `--cdp`, `--browser-id`, or `--profile` to verify the CDP
  stack instead.
- Extension lifecycle: `setup --chatgpt` materializes packaged source into the
  stable `$YOETZ_DIR/chatgpt-native-extension` directory, loaded unpacked in
  Chrome exactly once; `update --chatgpt` re-syncs the managed copy, reloads
  the extension, and verifies the loaded version. Never hand-patch the managed
  directory. Other subcommands (`doctor`, `status`, `inspect`, ...) are listed
  in `yoetz browser extension --help`.
- ChatGPT conversation resume uses `--var conversation=<id|url>`
  (native-extension only, no automatic context management); callers own the
  resume-vs-fresh decision.
- Multiple loaded extension profiles route by `profile_email` when Chrome
  exposes it, else by the stable `extension_instance_id` from
  `status --chatgpt`.
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

## litellm-rust (external)

The [`litellm-rust`](https://github.com/avivsinai/litellm-rust) crate (separate repo) provides unified access to multiple LLM providers:
- `LiteLLM::completion()` - Chat completions
- `LiteLLM::embedding()` - Text embeddings
- `LiteLLM::image_generation()` - Image generation
- `LiteLLM::video_generation()` - Video generation (Gemini)

Model routing: use `provider/model` format (e.g., `openrouter/anthropic/claude-sonnet-4-5`) or configure a default provider.

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

### Release Contract

- Release from `main` only through `./scripts/release.sh X.Y.Z` and the resulting release PR; do not create manual tags or GitHub releases.
- A push to `main` updates the AvivSinai marketplace immediately for the `yoetz` skill.
- Keep one version across `CHANGELOG.md`, workspace metadata, skill/plugin metadata, and the release commit; after merge, CI validates the merged commit, creates the tag, publishes the GitHub release from the committed changelog entry, and updates Homebrew/Scoop.

Use the fast release path:

```bash
./scripts/release.sh 0.2.24
```

This script updates `CHANGELOG.md`, bumps `[workspace.package].version`,
aligns skill/plugin metadata, runs `cargo check --workspace`, creates
`release/vX.Y.Z`, commits
`chore(release): vX.Y.Z`, pushes the branch, and opens a PR with `gh`.

After the release PR merges:
- `.github/workflows/release.yml` detects the merged `chore(release): vX.Y.Z`
  commit on `main`, creates/pushes the matching tag, publishes artifacts,
  uses `CHANGELOG.md` for the GitHub release notes, and updates Homebrew/Scoop
- `.github/workflows/release.yml` also supports `workflow_dispatch` as a retry
  path for an existing tag if a release job needs to be rerun manually

Repository setup for the fast path:
- `gh auth login`: needed locally if you want `./scripts/release.sh` to open the
  PR automatically after pushing the release branch

`CHANGELOG.md` is part of release prep again. The release commit is the source
of truth, and CI republishes that same changelog entry in the GitHub release.

We intentionally keep the custom GitHub Actions release flow instead of adopting
`release-plz`/`release-please` wholesale: this repo ships GitHub release
artifacts plus Homebrew/Scoop updates, but does not use crates.io publishing as
its primary release path. The fastest fit here is letting the merged release
commit drive the entire pipeline, not replacing the release pipeline.

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
- For file upload, the ChatGPT recipe uses macOS clipboard paste via `osascript`
  (set clipboard to POSIX file, then `Meta+v` in the composer). This avoids
  QuickJS buffer limitations and native file dialog issues with `setInputFiles`.
  Non-macOS platforms currently fall back to paste mode (inline text).
- The QuickJS GC crash recovery in `dev_browser.rs` can salvage stdout from a
  completed script, but recipe correctness must not depend on that recovery.

## Browser Architecture

Yoetz browser integrations are extension-free by design.

- Treat yoetz as a thin wrapper over the underlying browser transport unless
  yoetz must own behavior for correctness or UX.
- Preferred transport order for live Chrome work is `chrome-devtools-mcp`
  first, `dev-browser` second, `agent-browser` third. Keep the stack
  extension-free.
- Default mode is connect-first: attach to the user's already running Chrome
  session (`--connect`, auto-connect, or explicit `--cdp`) before considering
  cookie sync or managed-profile fallbacks.
- The daemon is trusted by default. Do not silently recycle live-attach daemons
  during normal attach/check/recipe flows. If recovery is needed, require an
  explicit `yoetz browser reset`.

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

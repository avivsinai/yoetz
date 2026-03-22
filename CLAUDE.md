# Yoetz

Fast CLI-first LLM council, bundler, and multimodal gateway for coding agents.

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

Tests use WireMock for HTTP mocking - no API keys needed for `cargo test`.

## Release

Use the fast release path:

```bash
./scripts/release.sh 0.2.24
```

This script bumps `[workspace.package].version`, runs `cargo check --workspace`,
creates `release/vX.Y.Z`, commits `chore(release): vX.Y.Z`, pushes the branch,
and opens a PR with `gh`.

After the release PR merges:
- `.github/workflows/release.yml` detects the merged `chore(release): vX.Y.Z`
  commit on `main`, creates/pushes the matching tag, publishes artifacts,
  generates release notes with `git-cliff`, and updates Homebrew/Scoop
- `.github/workflows/release.yml` also supports `workflow_dispatch` as a retry
  path for an existing tag if a release job needs to be rerun manually

Repository setup for the fast path:
- `gh auth login`: needed locally if you want `./scripts/release.sh` to open the
  PR automatically after pushing the release branch

`CHANGELOG.md` is no longer part of manual release prep. GitHub release notes
generated in CI are the source of truth.

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

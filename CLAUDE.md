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

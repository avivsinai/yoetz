# yoetz

[![CI](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml/badge.svg)](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust: 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)

Fast, CLI-first LLM council + bundler + multimodal gateway for coding agents.

> **Note**: This project is under active development. APIs may change.

## Why yoetz?

Most LLM CLI tools focus on a single provider or a single workflow. yoetz is different:

- **Multi-model council** — get consensus from multiple LLMs in one command, not sequential copy-paste between tabs
- **Multimodal native** — text, images, and video as first-class inputs across providers
- **Agent-first design** — structured JSON output, budget tracking, and agent skill integration out of the box
- **Zero lock-in** — one config, any provider (OpenRouter, OpenAI, Gemini, LiteLLM), switch with a flag
- **Bundle-aware** — package your codebase with gitignore-awareness for maximum LLM context

## Table of Contents

- [Why yoetz?](#why-yoetz)
- [Features](#features)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [Supported Providers](#supported-providers)
- [Environment Variables](#environment-variables)
- [MSRV Policy](#msrv-policy)
- [Contributing](#contributing)
- [License](#license)

## Features

- **Bundle**: Package code files with gitignore-awareness for LLM context
- **Ask**: Query LLMs with text, images, or video
- **Council**: Multi-model consensus with configurable voting
- **Review**: AI-powered code review for diffs and files
- **Generate**: Create images (OpenAI) and videos (Sora, Veo)
- **Browser**: Fallback to web UIs via recipes

## Installation

### Homebrew (macOS / Linux)

```bash
brew install avivsinai/tap/yoetz
```

### Scoop (Windows)

```powershell
scoop bucket add avivsinai https://github.com/avivsinai/scoop-bucket
scoop install yoetz
```

### Pre-built Binaries

Download the latest release from [GitHub Releases](https://github.com/avivsinai/yoetz/releases).

```bash
# macOS (Apple Silicon)
curl -LO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-aarch64-apple-darwin.tar.gz
tar xzf yoetz-aarch64-apple-darwin.tar.gz
sudo mv yoetz /usr/local/bin/

# macOS (Intel)
curl -LO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-x86_64-apple-darwin.tar.gz
tar xzf yoetz-x86_64-apple-darwin.tar.gz
sudo mv yoetz /usr/local/bin/

# Linux (x86_64)
curl -LO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-x86_64-unknown-linux-gnu.tar.gz
tar xzf yoetz-x86_64-unknown-linux-gnu.tar.gz
sudo mv yoetz /usr/local/bin/

# Linux (ARM64)
curl -LO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-aarch64-unknown-linux-gnu.tar.gz
tar xzf yoetz-aarch64-unknown-linux-gnu.tar.gz
sudo mv yoetz /usr/local/bin/
```

### From Source

```bash
cargo install --git https://github.com/avivsinai/yoetz
```

### Build Locally

```bash
git clone https://github.com/avivsinai/yoetz.git
cd yoetz
cargo build --release
```

### Agent Skill (Claude Code / Codex CLI)

```bash
# Via skills marketplace
/plugin marketplace add avivsinai/skills-marketplace
/plugin install yoetz@avivsinai-marketplace

# Via skills.sh
npx skills add avivsinai/yoetz

# Via skild.sh
npx skild install @avivsinai/yoetz
```

## Quick Start

### Configuration

Create `~/.yoetz/config.toml`:

```toml
[defaults]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-5-20250929"

[providers.openrouter]
api_key_env = "OPENROUTER_API_KEY"

[providers.openai]
api_key_env = "OPENAI_API_KEY"

[providers.gemini]
api_key_env = "GEMINI_API_KEY"
```

### Basic Usage

```bash
# Bundle files for LLM context
yoetz bundle --prompt "Review this code" --files "src/**/*.rs"

# Ask a question
yoetz ask --prompt "Explain this function" --files "src/main.rs"

# Ask with structured JSON output (OpenAI-compatible)
yoetz ask --prompt "Return JSON only" --provider openai --model gpt-5.2 --response-format json

# Ask with an image (vision)
yoetz ask --prompt "Describe this diagram" --image diagram.png --provider gemini --model gemini-3-flash-preview

# Override MIME type for signed/extensionless URLs
yoetz ask --prompt "Describe this" --image https://example.com/signed --image-mime image/png

# Ask about a video
yoetz ask --prompt "Summarize this" --video meeting.mp4 --provider gemini --model gemini-3-flash-preview

# Override video MIME type for signed/extensionless URLs
yoetz ask --prompt "Summarize this" --video https://example.com/signed --video-mime video/mp4

> Note: Gemini can return empty content if `--max-output-tokens` is too low because tokens are consumed by thoughts. If you see warnings or empty output, increase the limit.

# Debug raw provider responses
yoetz --debug ask --provider gemini --model gemini-3-flash-preview --prompt "ping"

# Multi-model council
yoetz council --prompt "Review this PR" --models "openai/gpt-5.2-codex,anthropic/claude-sonnet-4-5-20250929"

# Code review
yoetz review diff --model openai/gpt-5.2-codex
yoetz review file --path src/lib.rs --model anthropic/claude-sonnet-4-5-20250929
```

### Generation

```bash
# Generate images
yoetz generate image --prompt "A cozy cabin in snow" --provider openai --model gpt-image-1.5

# Generate video (Sora)
yoetz generate video --prompt "Drone flyover" --provider openai --model sora-2-pro

# Generate video (Veo)
yoetz generate video --prompt "Ocean waves" --provider gemini --model veo-3.1-generate-preview
```

### Browser Fallback

```bash
# Direct browser command
yoetz browser exec -- open https://chatgpt.com/

# Use a recipe
yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle bundle.md

# JSON output aggregates steps
yoetz --format json browser recipe --recipe recipes/chatgpt.yaml --bundle bundle.md
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for design details.

```
yoetz/
├── crates/
│   ├── yoetz-core/       # Core library
│   │   ├── bundle.rs     # File bundling with gitignore
│   │   ├── config.rs     # TOML config loading + profiles
│   │   ├── media.rs      # Media types for multimodal
│   │   └── types.rs      # Shared types
│   └── yoetz-cli/        # CLI binary
│       ├── main.rs       # Command handlers
│       ├── providers/    # OpenAI, Gemini implementations
│       ├── registry.rs   # Model registry (OpenRouter, LiteLLM)
│       └── budget.rs     # Daily spend tracking
├── recipes/              # Browser automation YAML
└── docs/                 # Configuration examples
```

## Supported Providers

| Provider | Text | Vision | Image Gen | Video Gen | Video Understanding |
|----------|------|--------|-----------|-----------|---------------------|
| OpenRouter | Yes | via model | - | - | - |
| OpenAI | Yes | Yes | Yes | Yes (Sora) | - |
| Gemini | Yes | Yes | - | Yes (Veo) | Yes |
| LiteLLM | Yes | via model | - | - | - |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `GEMINI_API_KEY` | Google Gemini API key |
| `LITELLM_API_KEY` | LiteLLM proxy key |
| `YOETZ_CONFIG_PATH` | Custom config path |

## MSRV Policy

The minimum supported Rust version is **1.88**. MSRV is tested in CI and bumped only with minor releases.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## Verifying Downloads

After downloading a release binary, verify its checksum:

```bash
# Download the checksums file
curl -LO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt

# Verify (macOS)
shasum -a 256 -c SHA256SUMS.txt --ignore-missing

# Verify (Linux)
sha256sum -c SHA256SUMS.txt --ignore-missing
```

## License

[MIT](LICENSE)

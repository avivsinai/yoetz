# yoetz

[![CI](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml/badge.svg)](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust: 1.75+](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)

Fast, CLI-first LLM council + bundler + multimodal gateway for coding agents.

> **Note**: This project is under active development. APIs may change.

## Features

- **Bundle**: Package code files with gitignore-awareness for LLM context
- **Ask**: Query LLMs with text, images, or video
- **Council**: Multi-model consensus with configurable voting
- **Review**: AI-powered code review for diffs and files
- **Generate**: Create images (OpenAI) and videos (Sora, Veo)
- **Browser**: Fallback to web UIs via recipes

## Installation

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

## Quick Start

### Configuration

Create `~/.yoetz/config.toml`:

```toml
[defaults]
provider = "openrouter"
model = "anthropic/claude-3.5-sonnet"

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
yoetz ask --prompt "Return JSON only" --provider openai --model gpt-4.1 --response-format json

# Ask with an image (vision)
yoetz ask --prompt "Describe this diagram" --image diagram.png --provider gemini --model gemini-2.0-flash

# Ask about a video
yoetz ask --prompt "Summarize this" --video meeting.mp4 --provider gemini --model gemini-2.0-flash

# Multi-model council
yoetz council --prompt "Review this PR" --models "openai/gpt-4o,anthropic/claude-3.5-sonnet"

# Code review
yoetz review diff --model openai/gpt-4o
yoetz review file --path src/lib.rs --model anthropic/claude-3.5-sonnet
```

### Generation

```bash
# Generate images
yoetz generate image --prompt "A cozy cabin in snow" --provider openai --model gpt-4.1

# Generate video (Sora)
yoetz generate video --prompt "Drone flyover" --provider openai --model sora-2-pro

# Generate video (Veo)
yoetz generate video --prompt "Ocean waves" --provider gemini --model veo-3.1
```

### Browser Fallback

```bash
# Direct browser command
yoetz browser exec -- open https://chatgpt.com/

# Use a recipe
yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle bundle.md
```

## Architecture

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
| OpenRouter | ✅ | via model | - | - | - |
| OpenAI | ✅ | ✅ | ✅ | ✅ (Sora) | - |
| Gemini | ✅ | ✅ | - | ✅ (Veo) | ✅ |
| LiteLLM | ✅ | via model | - | - | - |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `GEMINI_API_KEY` | Google Gemini API key |
| `LITELLM_API_KEY` | LiteLLM proxy key |
| `YOETZ_CONFIG_PATH` | Custom config path |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

[MIT](LICENSE)

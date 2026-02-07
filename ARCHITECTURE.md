# Architecture

This document describes the high-level architecture of yoetz.

## Crate Structure

```
yoetz/
├── crates/
│   ├── yoetz-core/          # Library crate (no network, no async)
│   │   ├── bundle.rs        # File bundling with gitignore awareness
│   │   ├── config.rs        # TOML config loading and profiles
│   │   ├── media.rs         # Media type detection (image/video MIME)
│   │   ├── types.rs         # Shared types (Usage, PricingEstimate, etc.)
│   │   ├── output.rs        # JSON/JSONL output formatting
│   │   ├── paths.rs         # XDG-aware path resolution
│   │   ├── registry.rs      # Model registry and provider routing
│   │   └── session.rs       # Session storage and retrieval
│   │
│   └── yoetz-cli/           # Binary crate (async, networked)
│       ├── main.rs           # CLI entry point, clap definitions, dispatch
│       ├── commands/
│       │   ├── ask.rs        # Single-model query
│       │   ├── council.rs    # Multi-model consensus
│       │   ├── review.rs     # Code review (diff/file)
│       │   ├── bundle.rs     # Bundle subcommand handler
│       │   ├── generate.rs   # Image/video generation
│       │   ├── pricing.rs    # Cost estimation
│       │   ├── models.rs     # Model listing
│       │   └── apply.rs      # Apply review suggestions
│       ├── providers/
│       │   ├── openai.rs     # OpenAI/OpenRouter API client
│       │   └── gemini.rs     # Google Gemini API client
│       ├── browser.rs        # Browser automation (Playwright)
│       ├── budget.rs         # Daily spend tracking (file-based)
│       ├── registry.rs       # Runtime model resolution
│       └── http.rs           # Shared HTTP utilities
│
├── recipes/                  # Browser automation YAML recipes
├── skills/                   # Agent skill definitions
├── scripts/                  # Helper scripts (cookie extraction)
└── docs/                     # Configuration examples
```

## Design Decisions

### Core vs CLI Split

`yoetz-core` contains pure, synchronous logic with no network dependencies. This makes it testable without mocking HTTP and reusable as a library. `yoetz-cli` owns all async runtime, network calls, and user interaction.

### Provider Abstraction

Rather than a trait-based provider abstraction, yoetz uses [litellm-rs](https://github.com/avivsinai/litellm-rs) as its unified LLM SDK. litellm-rs handles provider-specific API differences (auth, endpoints, request/response formats) behind a single `LiteLLM::completion()` interface.

Provider-specific code in `providers/` exists only for features not yet in litellm-rs (e.g., Gemini video generation, OpenAI image generation with specific parameters).

### Model Routing

Models use `provider/model` format (e.g., `openai/gpt-5.2`). OpenRouter models use nested format: `openrouter/anthropic/claude-sonnet-4`. The registry resolves these to the correct API endpoint and configuration.

### Session Management

Every `ask`, `council`, `review`, and `bundle` command creates a session under `~/.yoetz/sessions/<id>/`. Sessions store:
- `bundle.md` - the assembled context
- `response.json` - raw provider responses
- `metadata.json` - timing, cost, model info

This enables replay, debugging, and the `apply` command for code review suggestions.

### Budget Tracking

Daily spend is tracked in a local JSON file. The `--max-cost-usd` flag estimates cost before sending (using the pricing registry) and aborts if over budget. `--daily-budget-usd` accumulates across commands.

### Browser Fallback

For models without API access (e.g., ChatGPT Pro), yoetz bundles files into markdown, then drives a Playwright browser with extracted Chrome cookies to submit the bundle through the web UI. This is experimental and fragile by nature.

## Data Flow

```
User Input (prompt + files)
    │
    ├─ bundle.rs: collect files, apply gitignore, assemble markdown
    │
    ├─ media.rs: detect/validate image/video inputs
    │
    ├─ config.rs: resolve provider + model from config/flags
    │
    ├─ budget.rs: estimate cost, check daily budget
    │
    ├─ litellm-rs: send request to provider API
    │   ├─ OpenAI / OpenRouter
    │   ├─ Gemini
    │   └─ LiteLLM proxy
    │
    ├─ session.rs: persist request/response
    │
    └─ output.rs: format as JSON/text, write to stdout/file
```

## Testing Strategy

- **Unit tests**: Inline `#[cfg(test)]` modules testing core logic (bundling, media detection, budget math)
- **HTTP mocking**: WireMock for provider API tests (no API keys needed)
- **CLI integration**: `assert_cmd` tests for command-line behavior
- **Serial execution**: `serial_test` for tests that share filesystem state

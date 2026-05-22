# yoetz

<p>
  <img src="assets/branding/yoetz-quorum-mark.svg" alt="Yoetz quorum mark" width="96" height="96">
</p>

[![CI](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml/badge.svg)](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust: 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)

Fast, CLI-first LLM council + bundler + multimodal gateway for coding agents.

> **Note**: This project is under active development. APIs may change.

## Why yoetz?

Most LLM CLI tools focus on a single provider or a single workflow. yoetz is different:

- **Multi-model council** — get consensus from multiple LLMs in one command, not sequential copy-paste between tabs
- **Multimodal native** — text, images, and video as first-class inputs across providers
- **Agent-first design** — structured JSON output, local budget tracking for ask/council/review, and agent skill integration out of the box
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
- [Verifying Downloads](#verifying-downloads)
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
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-aarch64-apple-darwin.tar.gz
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt
shasum -a 256 -c SHA256SUMS.txt --ignore-missing
tar xzf yoetz-aarch64-apple-darwin.tar.gz
sudo mv yoetz /usr/local/bin/

# macOS (Intel)
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-x86_64-apple-darwin.tar.gz
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt
shasum -a 256 -c SHA256SUMS.txt --ignore-missing
tar xzf yoetz-x86_64-apple-darwin.tar.gz
sudo mv yoetz /usr/local/bin/

# Linux (x86_64)
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-x86_64-unknown-linux-gnu.tar.gz
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt
sha256sum -c SHA256SUMS.txt --ignore-missing
tar xzf yoetz-x86_64-unknown-linux-gnu.tar.gz
sudo mv yoetz /usr/local/bin/

# Linux (ARM64)
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-aarch64-unknown-linux-gnu.tar.gz
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt
sha256sum -c SHA256SUMS.txt --ignore-missing
tar xzf yoetz-aarch64-unknown-linux-gnu.tar.gz
sudo mv yoetz /usr/local/bin/
```

### From Source

```bash
cargo install --git https://github.com/avivsinai/yoetz --locked
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

Create `~/.config/yoetz/config.toml`:

```bash
mkdir -p ~/.config/yoetz
cp docs/config.example.toml ~/.config/yoetz/config.toml
```

Yoetz also loads the legacy `~/.yoetz/config.toml`, profiles under those config
directories, repo-local `./yoetz.toml` with untrusted-provider safeguards, and
`YOETZ_CONFIG_PATH` when set.

### Basic Usage

```bash
# Bundle files for LLM context
yoetz bundle --prompt "Review this code" --files "src/**/*.rs"
```

Bundles are trust boundaries: treat bundled repository content, issues, logs,
and pasted browser output as untrusted prompt input. Keep instructions in
`--prompt`, avoid bundling secrets, and review generated changes before applying
them.

```bash
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
```

> Note: Gemini can return empty content if `--max-output-tokens` is too low because tokens are consumed by thoughts. If you see warnings or empty output, increase the limit.

```bash
# Debug raw provider responses
yoetz --debug ask --provider gemini --model gemini-3-flash-preview --prompt "ping"

# Resolve live model IDs before putting them in scripts
yoetz models frontier --format json

# Multi-model council
yoetz council --prompt "Review this PR" --models "openai/<model-id>,openrouter/anthropic/claude-sonnet-4.5"

# Code review
yoetz review diff --model openai/gpt-5.2-codex
yoetz review file --path src/lib.rs --provider openrouter --model anthropic/claude-sonnet-4.5
```

Council calls require explicit model IDs and cost roughly scales with every
selected model. Budget flags are local preflight/accounting aids for
ask/council/review, not provider-side hard limits; verify current model IDs and
provider capabilities before relying on examples.

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

The default browser stack remains extension-free: ChatGPT recipes use
`chrome-devtools-mcp`, then `dev-browser`, then `agent-browser` / cookie
fallbacks as needed. The experimental `chrome-extension-native` transport is an
explicit opt-in exception for ChatGPT Pro runs that need install-once native
messaging instead of CDP approval prompts. This native-host path is currently
macOS/Linux-only; Windows CLI artifacts still ship, but Windows native-host
registration is not implemented yet.

```bash
yoetz browser extension setup --chatgpt --open-chrome
yoetz browser extension install-host --chatgpt
yoetz browser extension doctor --chatgpt
yoetz browser extension status --chatgpt
yoetz browser extension reconnect --chatgpt
yoetz browser extension reload --chatgpt
yoetz browser extension canary --chatgpt
yoetz browser extension inspect --chatgpt --run-id <run-id>
yoetz browser extension grant-identity --chatgpt
yoetz browser check --transport chrome-extension-native

yoetz browser recipe --recipe chatgpt --transport chrome-extension-native --bundle bundle.md
yoetz browser recipe --recipe chatgpt --transport chrome-extension-native --bundle bundle.md --var profile_email=user@example.com
yoetz browser recipe --recipe chatgpt --transport chrome-extension-native --bundle bundle.md --var extension_instance_id=ext_...
```

For the extension transport, every loaded Chrome profile publishes a separate
native bridge instance. With exactly one connected instance, Yoetz uses it. With
multiple connected instances, pass `--var profile_email=<email>` or
`--var extension_instance_id=<id>` from `status --chatgpt` so Yoetz can route to
the matching Chrome profile; otherwise it fails closed rather than guessing. If
Chrome does not expose the profile email to the extension, an explicit
`profile_email` request also fails closed, but the stable
`extension_instance_id` selector still works. These selectors identify the
Chrome extension/profile instance, not the ChatGPT account or Enterprise
workspace. To use `profile_email`, first opt in with
`yoetz browser extension grant-identity --chatgpt`; exact
`browser_context_id` targeting remains CDP-only.

Release builds publish the ChatGPT native extension as a separate versioned zip
artifact alongside the CLI archives. To install or update it manually, unzip the
artifact, open `chrome://extensions`, enable Developer mode, choose **Load
unpacked**, and select the extracted zip directory itself. From a source
checkout, select `extensions/chatgpt-native` instead. When updating an unpacked
install, replace the extracted files and click the extension row's reload
button, or run `yoetz browser extension reload --chatgpt` when the currently
loaded extension already supports the reload command; then run
`yoetz browser extension reconnect --chatgpt` and
`yoetz browser extension doctor --chatgpt`.

For agent-driven setup, `yoetz browser extension setup --chatgpt --open-chrome`
does everything Chrome allows from the CLI: it installs or updates the native
host, finds the unpacked extension directory when available, opens
`chrome://extensions`, and prints the exact folder to select. Chrome still
requires the explicit **Load unpacked** UI step for local unpacked extensions.
If the extension directory is not discoverable from the current checkout or
installation, set `YOETZ_CHATGPT_NATIVE_EXTENSION_DIR` to the extracted
extension directory and rerun `setup`.

For normal Google Chrome profiles, `install-host` writes the Native Messaging
host manifest to Chrome's default user path. If Chrome is launched with a custom
`--user-data-dir`, or if you are using Chrome for Testing or Chromium, set
`YOETZ_CHROME_NATIVE_MESSAGING_DIR` to that browser user-data directory's
`NativeMessagingHosts` folder before running `install-host`. On Windows this
transport fails closed until registry-based native-host setup is added.
Use `yoetz browser check --transport chrome-extension-native` to verify the
installed extension bridge without exercising CDP or triggering Chrome's remote
debugging approval dialog. Use `yoetz browser extension canary --chatgpt --live`
only when you intentionally want to submit a tiny live ChatGPT probe.

### DX: ChatGPT Pro autonomous review via the extension transport

The transport drives upload → send → wait → extract reliably, but ChatGPT
Pro's file analyzer can still stall or return truncated answers on large
real-review attachments. This is not a stable token ceiling: in live testing,
large review bundles around 60k and 220k effective tokens failed, while tiny
sentinel canaries succeeded. For autonomous code review, prefer focused
per-directory slices over a single large bundle, and raise `wait_timeout_ms`
for jobs that are expected to spend a long time in ChatGPT file analysis.
Yoetz fails terminally with privacy-scoped diagnostics (`response_timeout`,
extraction method/status, assistant-turn counts, and bounded scoped snippets)
rather than returning partial or thought-only chrome as success.

Real ChatGPT Pro review jobs can run for 15-20 minutes while file analysis
runs. That is expected. The native-extension transport emits low-noise lifecycle
and `waiting_response` progress to stderr, including in `--format json` mode so
stdout stays parseable. The recipe response poll default is 30 minutes; agents
should keep the original process attached, write the response with
`--output-final`, and avoid launching a duplicate run just because progress is
sparse. Use `--var wait_timeout_ms=2400000` only for slices that are expected to
exceed the default. If a terminal upload/send/wait error is reported, inspect
the marked tab with
`yoetz browser extension inspect --chatgpt --run-id <run-id>` before deciding
whether an intentional rerun is safe.

The recipe never auto-falls-back to another transport once a side effect has
landed in the user's tab. If the run fails after upload/send, the error
includes a manual recovery hint (`window.name` marker, `_yoetz` URL marker,
extension marker prefix) so an agent can decide whether to reuse the tab or
abort. Pass `--allow-cdp-fallback` only if you understand that explicitly
permits a second submission via CDP.

The built-in ChatGPT recipe defaults to `model=gpt-5-4-pro` with Extended left
enabled. `model=auto` still prefers the Pro/Extended personal UI control when
it exists, and the enterprise model switcher remains supported. The
`--var extended=false` toggle is best-effort and only runs when explicitly
requested. Yoetz scopes the chip match to the ChatGPT composer with
negative-control guards, but ChatGPT re-skins the Extended thinking control
occasionally; on a miss the run continues with whatever Extended state the tab
was in and emits a warning.

If the next ChatGPT run after an unexplained failure should inspect the
Yoetz-owned tab without resubmitting, use
`yoetz browser extension inspect --chatgpt --run-id <id>` to read the live
extraction, conversation id, and privacy-scoped diagnostics through the
extension bridge. Inspection is read-only, omits broad page text by default,
and never restarts the run.

If inspection shows `response_extraction_failed` and the owned tab also contains
only a tiny/truncated assistant fragment, treat it as a bad ChatGPT answer and
rerun intentionally with a smaller bundle or a more explicit prompt. If the tab
visibly contains the full answer, preserve the tab and report the extraction
miss instead of rerunning blindly.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for design details.

```
yoetz/
├── crates/
│   ├── yoetz-core/       # Core library
│   │   ├── bundle.rs     # File bundling with gitignore
│   │   ├── config.rs     # TOML config loading + profiles
│   │   ├── media.rs      # Media types for multimodal
│   │   ├── output.rs     # JSON/JSONL formatting
│   │   ├── paths.rs      # Home/XDG path helpers
│   │   ├── registry.rs   # Model registry types
│   │   ├── session.rs    # Session storage
│   │   └── types.rs      # Shared types
│   └── yoetz-cli/        # CLI binary
│       ├── main.rs       # Command handlers
│       ├── commands/     # ask, bundle, council, generate, models, pricing, review
│       ├── providers/    # Provider-specific helpers not covered by litellm-rust
│       ├── browser.rs    # Browser recipe orchestration
│       ├── browser_extension_native.rs
│       ├── chatgpt_recipe.rs
│       ├── chatgpt_web.rs
│       ├── chrome_devtools_mcp/
│       ├── dev_browser.rs
│       ├── live_attach.rs
│       ├── live_cdp_daemon.rs
│       ├── registry.rs   # Runtime model registry
│       └── budget.rs     # Daily spend tracking
├── recipes/              # Browser automation YAML
└── docs/                 # Config examples and design decisions
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
| `ANTHROPIC_API_KEY` | Anthropic API key for direct Anthropic provider configs |
| `XAI_API_KEY` | xAI API key for direct xAI/OpenAI-compatible provider configs |
| `LITELLM_API_KEY` | LiteLLM proxy key |
| `YOETZ_CONFIG_PATH` | Custom config path |
| `YOETZ_AGENT=1` | Default command output to JSON for agent parsing |
| `YOETZ_BROWSER_CDP` | CDP endpoint for browser attach/check/recipe commands |
| `YOETZ_BROWSER_PROFILE` | Browser profile name used by browser flows |
| `YOETZ_BROWSER_TARGET_PATH` | Browser target metadata path for live attach |
| `YOETZ_AGENT_BROWSER_BIN` | Override `agent-browser` executable |
| `YOETZ_DEV_BROWSER_BIN` | Override `dev-browser` executable |
| `YOETZ_CHROME_NATIVE_MESSAGING_DIR` | Native Messaging host directory for custom Chrome/Chromium profiles |
| `YOETZ_REGISTRY_PATH` | Override local model registry cache path |
| `YOETZ_BUDGET_PATH` | Override local budget ledger path |

## MSRV Policy

The minimum supported Rust version is **1.88**. MSRV is tested in CI and bumped only with minor releases.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## Verifying Downloads

Verify archive checksums before extracting or moving binaries into `PATH`:

```bash
# Download the archive and checksums file
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-aarch64-apple-darwin.tar.gz
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt

# Verify the downloaded archive (macOS)
shasum -a 256 -c SHA256SUMS.txt --ignore-missing

# Verify the downloaded archive (Linux)
sha256sum -c SHA256SUMS.txt --ignore-missing

# Then extract and install
tar xzf yoetz-aarch64-apple-darwin.tar.gz
sudo mv yoetz /usr/local/bin/
```

## License

[MIT](LICENSE)

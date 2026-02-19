---
name: yoetz
version: 1.0.0
description: >
  Fast CLI-first LLM council, bundler, and multimodal gateway. Use ONLY when user
  explicitly mentions "yoetz", "yoetz ask", "yoetz council", "yoetz review",
  "yoetz generate", "yoetz bundle", "yoetz browser". NOT triggered by generic
  "second opinion" or "ask another model" requests.
metadata:
  short-description: LLM council and multimodal gateway CLI
  compatibility: claude-code, codex-cli
---

# Yoetz Skill

Fast, agent-friendly LLM council tool for multi-model consensus, code review, and bundling.

## When to Use

**Explicit triggers only:**
- "yoetz ask" / "yoetz council" / "yoetz review"
- "yoetz bundle" / "yoetz generate" / "yoetz browser"
- "use yoetz to..."

**NOT triggered by:**
- "second opinion" / "ask another model" (could be amq-cli)
- "council" alone / "review" alone (other skills may apply)

## Installation (auto-bootstrap)

Before running any `yoetz` command, ensure the CLI is installed.
If `command -v yoetz` fails, install via one of the following:

| Platform | Command |
|----------|---------|
| macOS (Homebrew) | `brew install avivsinai/homebrew-tap/yoetz` |
| Linux (Homebrew if available) | `brew install avivsinai/homebrew-tap/yoetz` |
| From source (Rust 1.88+) | `cargo install --git https://github.com/avivsinai/yoetz` |
| Windows (Scoop) | `scoop bucket add yoetz https://github.com/avivsinai/scoop-bucket && scoop install yoetz` |
| Pre-built binary | Download from [GitHub Releases](https://github.com/avivsinai/yoetz/releases) and place in PATH |

Prefer Homebrew when available — pre-built binaries, fastest install.

## Agent Contract

- Always use `--format json` for parsing
- Set `YOETZ_AGENT=1` environment variable
- Parse JSON results and present summary to user
- For large bundles, run `yoetz bundle` first to inspect size
- Always resolve uncertain model IDs with `yoetz models resolve` before calling

## Model Discovery

Before using an unfamiliar model ID, resolve it against the synced registry:

```bash
yoetz models resolve "grok-4.1" --format json
```

Example output:
```json
[{"id":"x-ai/grok-4","score":800,"provider":"openrouter","context_length":131072,"max_output_tokens":16384}]
```

If the registry is stale or empty, sync first:
```bash
yoetz models sync
```

Search for models by keyword:
```bash
yoetz models list -s claude --format json
```

## Quick Reference

| Task | Command |
|------|---------|
| Ask single model | `yoetz ask -p "question" -f src/*.rs --provider openai --model gpt-5.2 --format json` |
| Council vote | `yoetz council -p "question" --models openai/gpt-5.2,gemini/gemini-3-pro-preview,openrouter/xai/grok-4.1 --format json` |
| Review staged diff | `yoetz review diff --staged --format json` |
| Review file | `yoetz review file --path src/main.rs --format json` |
| Bundle files | `yoetz bundle -p "context" -f src/**/*.rs --format json` |
| Generate image | `yoetz generate image -p "description" --provider openai --model gpt-image-1 --format json` |
| Resolve model ID | `yoetz models resolve "grok-4.1" --format json` |
| Search models | `yoetz models list -s claude --format json` |
| Estimate cost | `yoetz pricing estimate --model gpt-5.2 --input-tokens 1000 --output-tokens 500` |

## Council (Multi-Model Consensus)

Get opinions from multiple LLMs in parallel. **`--models` is required.**

```bash
yoetz council \
  -p "Should we use async traits or callbacks for this API?" \
  -f src/lib.rs -f src/api/*.rs \
  --models openai/gpt-5.2,gemini/gemini-3-pro-preview,openrouter/xai/grok-4.1 \
  --format json
```

## Ask (Single Model)

```bash
yoetz ask \
  -p "What's the bug in this error handling?" \
  -f src/error.rs \
  --provider openai --model gpt-5.2 \
  --format json
```

## Review

```bash
yoetz review diff --staged --format json
yoetz review file --path src/main.rs --format json
```

## Bundle

```bash
yoetz bundle -p "context" -f src/**/*.rs --format json
```

## Provider Configuration

**Built-in providers** (work with just env var):
- `openai` - `OPENAI_API_KEY`
- `gemini` - `GEMINI_API_KEY`
- `openrouter` - `OPENROUTER_API_KEY`

**Model format:** `provider/model` (e.g., `openai/gpt-5.2`, `openrouter/anthropic/claude-sonnet-4`)

## Cost Control

```bash
yoetz pricing estimate --model gpt-5.2 --input-tokens 12000 --output-tokens 800
yoetz ask -p "Review" --max-cost-usd 1.00 --daily-budget-usd 5.00 --format json
```

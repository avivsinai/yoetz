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

## Agent Contract

- Always use `--format json` for parsing
- Set `YOETZ_AGENT=1` environment variable
- Parse JSON results and present summary to user
- For large bundles, run `yoetz bundle` first to inspect size

## Quick Reference

| Task | Command |
|------|---------|
| Ask single model | `yoetz ask -p "question" -f src/*.rs --provider openai --model gpt-5.2 --format json` |
| Council vote | `yoetz council -p "question" --models openai/gpt-5.2,gemini/gemini-pro-3,openrouter/xai/grok-4.1 --format json` |
| Review staged diff | `yoetz review diff --staged --format json` |
| Review file | `yoetz review file --path src/main.rs --format json` |
| Bundle files | `yoetz bundle -p "context" -f src/**/*.rs --format json` |
| Generate image | `yoetz generate image -p "description" --provider openai --model gpt-image-1 --format json` |
| Estimate cost | `yoetz pricing estimate --model gpt-5.2 --input-tokens 1000 --output-tokens 500` |

## Council (Multi-Model Consensus)

Get opinions from multiple LLMs in parallel. **`--models` is required.**

```bash
yoetz council \
  -p "Should we use async traits or callbacks for this API?" \
  -f src/lib.rs -f src/api/*.rs \
  --models openai/gpt-5.2,gemini/gemini-pro-3,openrouter/xai/grok-4.1 \
  --format json
```

**Example council sets:**
- Cross-provider: `openai/gpt-5.2,gemini/gemini-pro-3,openrouter/xai/grok-4.1`
- Via OpenRouter only: `openrouter/openai/gpt-5.2,openrouter/anthropic/claude-sonnet-4,openrouter/google/gemini-pro-3`

## Ask (Single Model)

Quick question with file context:

```bash
yoetz ask \
  -p "What's the bug in this error handling?" \
  -f src/error.rs \
  --provider openai --model gpt-5.2 \
  --format json
```

**For Anthropic/XAI models**, use OpenRouter (no extra config needed):
```bash
yoetz ask -p "Review this" -f src/*.rs \
  --provider openrouter --model anthropic/claude-sonnet-4 \
  --format json
```

## Review

### Staged changes
```bash
yoetz review diff --staged --format json
```

### Specific file
```bash
yoetz review file --path src/main.rs --format json
```

### With custom model
```bash
yoetz review diff --staged --provider openai --model gpt-5.2 --format json
```

## Bundle (for manual paste or browser mode)

Bundle creates a session with files at `~/.yoetz/sessions/<id>/bundle.md`.

```bash
# Get bundle path from JSON output
yoetz bundle -p "Explain this" -f src/**/*.rs --format json
# Output includes: {"artifacts":{"bundle_md":"/Users/.../.yoetz/sessions/.../bundle.md",...},...}

# Extract bundle_md path directly
BUNDLE=$(yoetz bundle -p "Review" -f src/*.rs --format json | jq -r .artifacts.bundle_md)
cat "$BUNDLE"
```

**For browser workflows**, pass the bundle.md path:
```bash
BUNDLE=$(yoetz bundle -p "Review" -f src/*.rs --format json | jq -r .artifacts.bundle_md)
yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle "$BUNDLE"
```

## Browser Fallback (Experimental)

For web-only models like ChatGPT Pro that lack API access.

### Prerequisites

```bash
# Install agent-browser globally
npm install -g agent-browser

# Or set path to local install
export YOETZ_AGENT_BROWSER_BIN=/path/to/agent-browser
```

### First-time setup (one-time manual login)

```bash
./scripts/setup-chatgpt-profile.sh
```

Or manually:
```bash
agent-browser --profile ~/.chatgpt-profile --headed open "https://chatgpt.com/"
# Log in, then close browser
```

### Use ChatGPT Pro via recipe

```bash
export AGENT_BROWSER_PROFILE=~/.chatgpt-profile

# Create bundle and get bundle.md path
BUNDLE=$(yoetz bundle -p "Review this code" -f src/*.rs --format json | jq -r .artifacts.bundle_md)

# Send to ChatGPT
yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle "$BUNDLE"
```

### Combined workflow: API + Browser

```bash
# Get fast API results first
yoetz council -p "Review" -f src/*.rs \
  --models openai/gpt-5.2,gemini/gemini-pro-3 --format json > api.json

# Then get ChatGPT Pro opinion
BUNDLE=$(yoetz bundle -p "Review" -f src/*.rs --format json | jq -r .artifacts.bundle_md)
yoetz browser recipe --recipe recipes/chatgpt.yaml --bundle "$BUNDLE"
```

## Provider Configuration

**Built-in providers** (work with just env var):
- `openai` - `OPENAI_API_KEY`
- `gemini` - `GEMINI_API_KEY`
- `openrouter` - `OPENROUTER_API_KEY`

**Via OpenRouter** (recommended for Anthropic/XAI - no extra config):
- `openrouter/anthropic/claude-sonnet-4`
- `openrouter/xai/grok-4.1`

**Model format:** `provider/model`
- `openai/gpt-5.2`
- `gemini/gemini-pro-3`
- `openrouter/anthropic/claude-sonnet-4` (nested for OpenRouter)

## Cost Control

```bash
# Estimate before running
yoetz pricing estimate --model gpt-5.2 --input-tokens 12000 --output-tokens 800

# Set limits
yoetz ask -p "Review" --max-cost-usd 1.00 --daily-budget-usd 5.00 --format json
```

## Tips

- Use `--debug` to capture raw responses during troubleshooting
- Gemini may return empty content if `--max-output-tokens` is too low
- Session artifacts stored in `~/.yoetz/sessions/<id>/`
- For image inputs: `yoetz ask -p "Describe" --image photo.png --format json`
- ChatGPT recipe placeholder may vary by locale; check snapshot output if fill fails

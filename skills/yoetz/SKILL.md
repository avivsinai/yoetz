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
- **NEVER type a model ID from memory.** Your training data model names are WRONG. Always resolve first.

## Model Resolution Protocol (MANDATORY)

**NEVER type a model ID from memory.** Agent training data contains stale model names. Always query the live registry.

**To find the current frontier model per provider:**
```bash
yoetz models frontier --format json
```

**To find a specific model:**
```bash
yoetz models resolve "grok" --format json
```

**Use the returned ID verbatim in your commands.** Do not modify, shorten, or guess model IDs.

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
| Find frontier model per provider | `yoetz models frontier --format json` |
| Find frontier model for a provider | `yoetz models frontier --family openai --format json` |
| Resolve a model ID | `yoetz models resolve "grok" --format json` |
| Search models | `yoetz models list -s claude --format json` |
| Ask single model | `yoetz ask -p "question" -f src/*.rs --provider openai --model MODEL_ID --format json` |
| Council vote | `yoetz council -p "question" --models MODEL1,MODEL2,MODEL3 --format json` |
| Review staged diff | `yoetz review diff --staged --format json` |
| Review file | `yoetz review file --path src/main.rs --format json` |
| Bundle files | `yoetz bundle -p "context" -f src/**/*.rs --format json` |
| Generate image | `yoetz generate image -p "description" --provider openai --model MODEL_ID --format json` |
| Estimate cost | `yoetz pricing estimate --model MODEL_ID --input-tokens 1000 --output-tokens 500` |
| Browser check | `yoetz browser check` |
| Browser attach | `yoetz browser attach` |
| Browser login | `yoetz browser login` |

**Replace MODEL_ID with IDs from `yoetz models frontier` or `yoetz models resolve`.**

## Council (Multi-Model Consensus)

Get opinions from multiple LLMs in parallel. **`--models` is required.**

```bash
yoetz council \
  -p "Should we use async traits or callbacks for this API?" \
  -f src/lib.rs -f src/api/*.rs \
  --models openai/gpt-5.4,gemini/gemini-3.1-pro-preview,openrouter/xai/grok-4.20-multi-agent-beta \
  --format json
```

**Example council sets:**
- Cross-provider: `openai/gpt-5.4,gemini/gemini-3.1-pro-preview,openrouter/xai/grok-4.20-multi-agent-beta`
- Via OpenRouter only: `openrouter/openai/gpt-5.4,openrouter/anthropic/claude-sonnet-4.6,openrouter/google/gemini-3.1-pro-preview`

## Ask (Single Model)

Quick question with file context:

```bash
yoetz ask \
  -p "What's the bug in this error handling?" \
  -f src/error.rs \
  --provider openai --model gpt-5.4 \
  --format json
```

**For Anthropic/XAI models**, use OpenRouter (no extra config needed):
```bash
yoetz ask -p "Review this" -f src/*.rs \
  --provider openrouter --model anthropic/claude-sonnet-4.6 \
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
yoetz review diff --staged --provider openai --model gpt-5.4 --format json
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
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE"
```

## Browser Mode

For web-only models like ChatGPT Pro that lack API access. Connects to your running Chrome via CDP (Chrome DevTools Protocol) to submit bundles through the web UI.

### Prerequisites

```bash
# agent-browser is auto-resolved via npx if not in PATH.
# For faster startup, install globally:
npm install -g agent-browser
```

### How connection works

yoetz connects to your already logged-in Chrome session via auto-connect (CDP). No cookie extraction or separate browser needed.

**Connection priority:** explicit `--cdp` > auto-connect > cookie state > profile fallback.

### First-time setup

**Step 1: Enable remote debugging in Chrome**
1. Open Chrome and go to `chrome://inspect/#remote-debugging`
2. Ensure "Discover network targets" is enabled

If Chrome lands on `chrome://inspect/#devices` instead, that's fine. Keep "Discover network targets" enabled there.

**Step 2: Run a recipe**
```bash
BUNDLE=$(yoetz bundle -p "Review" -f src/*.rs --format json | jq -r .artifacts.bundle_md)
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE"
```

**Step 3: Approve remote debugging (Chrome 146+)**
Chrome 146+ may show an "Allow remote debugging?" dialog on the first live attach. Click **Allow** once for that browser instance.

**Step 4: Verify connection**
```bash
yoetz browser attach
```

### Chrome 146+ notes

Chrome 146 introduced a security dialog for external CDP connections. yoetz handles this automatically:
- Detects the dialog and tells you to click Allow (instead of hanging)
- Reuses healthy daemon connections (no repeated dialogs)
- Cleans up stale daemons when the connection breaks

If you see "Allow remote debugging?" in Chrome, click Allow and retry.

Explicit `--cdp` is already supported on `yoetz browser attach`, `check`, `recipe`, and `login`, but it only bypasses auto-discovery. It does **not** bypass Chrome's approval gate when targeting the same live browser instance started from `chrome://inspect`.

If the approval dialog is frozen or unclickable, use the manual CDP path instead:
```bash
chrome --remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug
yoetz browser attach --cdp http://127.0.0.1:9222
```

Chrome for Testing is also a good fallback for this manual path.

### Cookie sync (legacy fallback)

If auto-connect isn't available, cookie sync is still supported:
```bash
# Log into ChatGPT in real Chrome, close Chrome, then:
yoetz browser sync-cookies
yoetz browser check
```
Requires Node >= 24.4. If macOS shows a Keychain prompt for `Chrome Safe Storage`, click `Always Allow`.

### Use ChatGPT Pro via recipe

```bash
# Create bundle and get bundle.md path
BUNDLE=$(yoetz bundle -p "Review this code" -f src/*.rs --format json | jq -r .artifacts.bundle_md)

# Send to ChatGPT
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE"

# Override the built-in model selection if needed
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE" --var model=gpt-5-4-pro
```

### Combined workflow: API + Browser

```bash
# Get fast API results first
yoetz council -p "Review" -f src/*.rs \
  --models openai/gpt-5.4,gemini/gemini-3.1-pro-preview --format json > api.json

# Then get ChatGPT Pro opinion
BUNDLE=$(yoetz bundle -p "Review" -f src/*.rs --format json | jq -r .artifacts.bundle_md)
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE"
```

### Recipe name resolution

Recipes can be specified by name (resolved from installed locations) or by path:

```bash
# By name (searches Homebrew share, XDG, etc.)
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE"

# By explicit path
yoetz browser recipe --recipe ./my-recipes/custom.yaml --bundle "$BUNDLE"
```

Built-in recipes: `chatgpt`, `claude`, `gemini`.

### Troubleshooting

| Symptom | Fix |
|---------|-----|
| `Allow remote debugging?` dialog | Click **Allow** in Chrome, then retry. If the dialog is frozen, launch Chrome with `--remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug` and use `yoetz browser attach --cdp http://127.0.0.1:9222` instead. |
| `auto-connect probe timed out` | Chrome dialog is probably showing. Click Allow. If Chrome will not accept the dialog, switch to the manual `--cdp` flow above. If agent-browser is missing, install it with `npm install -g agent-browser`. |
| `chatgpt login required` | Chrome was reached but not logged into ChatGPT. Log into ChatGPT in that Chrome session, then retry. Or use `yoetz browser login` for manual auth. |
| `daemon already running` | Run `yoetz browser attach` to check connection, or kill stale daemon: `agent-browser close` |
| `agent-browser failed` | Ensure `npx agent-browser --version` works, or `npm install -g agent-browser` |
| Recipe not found | Use `--recipe chatgpt` (name) or full path. Check `brew --prefix`/share/yoetz/recipes/ |
| `cookie extraction failed` | Legacy path: ensure Node >= 24.4, log into ChatGPT in Chrome, close Chrome, `yoetz browser sync-cookies` |

### Claude-in-Chrome MCP Fallback

When `yoetz browser` pipeline fails (agent-browser issues, cookie extraction errors), use Claude-in-Chrome MCP tools directly:

1. **Create bundle**: `yoetz bundle -p "Review" -f src/**/*.ts --format json`
2. **Copy to clipboard as file**: Use macOS `osascript` to put the bundle.md on clipboard:
   ```bash
   osascript -e 'set the clipboard to POSIX file "/path/to/bundle.md"'
   ```
3. **Navigate to ChatGPT**: Use `mcp__claude-in-chrome__navigate` to open chatgpt.com
4. **Paste file**: Click the input area, then Cmd+V to paste the file from clipboard
5. **Type prompt**: Use `mcp__claude-in-chrome__form_input` or `computer` tool to type the review prompt
6. **Wait and extract**: Use `mcp__claude-in-chrome__get_page_text` to extract the response

This bypasses agent-browser entirely and works with any browser-based LLM the user is logged into.

### How it works

The browser module connects to your running Chrome via CDP (Chrome DevTools Protocol):
- **Auto-connect** (primary): attaches to Chrome's remote debugging port, reuses your logged-in session
- **Cookie sync** (fallback): extracts cookies from Chrome's encrypted store, injects into agent-browser
- Uses stealth User-Agent headers and disables automation detection flags
- Daemon model: one persistent connection per session, reused across recipe steps

## Provider Configuration

**Built-in providers** (work with just env var):
- `openai` - `OPENAI_API_KEY`
- `gemini` - `GEMINI_API_KEY`
- `openrouter` - `OPENROUTER_API_KEY`

**Via OpenRouter** (recommended for Anthropic/XAI - no extra config):
- `openrouter/anthropic/claude-sonnet-4.6`
- `openrouter/xai/grok-4.20-multi-agent-beta`

**Model format:** `provider/model`
- `openai/gpt-5.4`
- `gemini/gemini-3.1-pro-preview`
- `openrouter/anthropic/claude-sonnet-4.6` (nested for OpenRouter)

## Cost Control

```bash
# Estimate before running
yoetz pricing estimate --model gpt-5.4 --input-tokens 12000 --output-tokens 800

# Set limits
yoetz ask -p "Review" --max-cost-usd 1.00 --daily-budget-usd 5.00 --format json
```

## Tips

- Use `--debug` to capture raw responses during troubleshooting
- Gemini may return empty content if `--max-output-tokens` is too low
- Session artifacts stored in `~/.yoetz/sessions/<id>/`
- For image inputs: `yoetz ask -p "Describe" --image photo.png --format json`
- ChatGPT recipe placeholder may vary by locale; check snapshot output if fill fails

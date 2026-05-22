---
name: yoetz
version: 0.5.12
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
| macOS (Homebrew) | `brew install avivsinai/tap/yoetz` |
| Linux (Homebrew if available) | `brew install avivsinai/tap/yoetz` |
| From source (Rust 1.88+) | `cargo install --git https://github.com/avivsinai/yoetz --locked` |
| Windows (Scoop) | `scoop bucket add avivsinai https://github.com/avivsinai/scoop-bucket && scoop install yoetz` |
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
| Browser check (CDP/default) | `yoetz browser check` |
| Extension check | `yoetz browser check --transport chrome-extension-native` |
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

**Example council sets (illustrative — always resolve live IDs via `yoetz models frontier`):**
- Cross-provider: `openai/<FRONTIER>,gemini/<FRONTIER>,openrouter/xai/<FRONTIER>`
- Via OpenRouter only: `openrouter/openai/<FRONTIER>,openrouter/anthropic/<FRONTIER>,openrouter/google/<FRONTIER>`

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

### ChatGPT Pro DX recommendation

If the user is doing repeated ChatGPT Pro/Enterprise recipe work, or mentions
Chrome remote-debugging approval friction, recommend the opt-in
`chrome-extension-native` transport as the robust path. Keep it explicit:
install the native host, load/update the extension in Chrome, run `doctor`, then
target the stable `extension_instance_id` when multiple profiles are open.

Do not silently switch transports after upload/send/wait side effects. If the
extension reports a terminal ChatGPT phase, preserve the manual-recovery tab and
warn before any rerun that could duplicate a submission.

ChatGPT Pro code reviews can legitimately spend 15-20 minutes in file analysis.
This is normal. The native-extension transport prints low-noise lifecycle and
`waiting_response` progress to stderr, including in `--format json` mode so
stdout remains parseable. For unattended review loops, keep waiting on the
original process until it returns, write the result with `--output-final`, and do
not launch a second run just because progress is sparse. The recipe response
poll default is 30 minutes; use
`--var wait_timeout_ms=2400000` only when the submitted review slice is expected
to exceed that. If the extension returns a terminal upload/send/wait error, use
`yoetz browser extension inspect --chatgpt --run-id <run-id>` before deciding
whether an intentional rerun is safe.

If progress says `waiting for final assistant controls`, ChatGPT has exposed
post-send assistant text but not a final scoped action affordance yet. Do not
treat the visible text as complete and do not start a duplicate run; use the
reported inspect command to check the preserved tab if you need live state.

### Prerequisites

```bash
# Optional fallback browser transports:
npm install -g dev-browser

# Secondary fallback transport:
npm install -g agent-browser
```

### How connection works

yoetz connects to your already logged-in Chrome session via auto-connect (CDP). No cookie extraction or separate browser needed.

**Transport priority:** `chrome-devtools-mcp` > `dev-browser` > `agent-browser` > manual browser upload.

**Connection priority:** explicit `--cdp` > auto-connect > cookie state > profile fallback.

Use `--cdp http://127.0.0.1:9222` when you need to target a specific Chrome instance/profile.

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

Chrome 146 introduced a security dialog for external CDP connections. Yoetz is extension-free by design, so the only way to get "approve once, then run silently" behavior is to keep the daemon/CDP session alive and avoid tearing it down between invocations.

Current policy:
- Prefer live attach over cookie sync: `chrome-devtools-mcp` first, `dev-browser` second, `agent-browser` third.
- Trust an existing live-attach daemon by default; yoetz does not silently recycle it during normal attach/check/recipe flows.
- If recovery is actually needed, use `yoetz browser reset` explicitly.
- Default browser work is extension-free. The experimental `chrome-extension-native` transport is an explicit opt-in ChatGPT-only exception, not part of the default transport order.

If you see "Allow remote debugging?" in Chrome, click Allow and retry.

Explicit `--cdp` is already supported on `yoetz browser attach`, `check`, `recipe`, and `login`, but it only bypasses auto-discovery. It does **not** bypass Chrome's approval gate when targeting the same live browser instance started from `chrome://inspect`.

If the approval dialog is frozen or unclickable, use the manual CDP path instead:
```bash
chrome --remote-debugging-port=9222 --user-data-dir=/tmp/chrome-debug
yoetz browser attach --cdp http://127.0.0.1:9222
```

Chrome for Testing is also a good fallback for this manual path.

### Experimental Chrome extension native transport

Use this only when ChatGPT Pro recipe robustness needs install-once native
messaging instead of CDP approval prompts:

```bash
yoetz browser extension setup --chatgpt --open-chrome
yoetz browser extension install-host --chatgpt
yoetz browser extension doctor --chatgpt
yoetz browser extension status --chatgpt
yoetz browser extension reconnect --chatgpt
yoetz browser extension reload --chatgpt
yoetz browser extension canary --chatgpt
yoetz browser check --transport chrome-extension-native

yoetz browser recipe --recipe chatgpt --transport chrome-extension-native --bundle "$BUNDLE"
yoetz browser recipe --recipe chatgpt --transport chrome-extension-native --bundle "$BUNDLE" --var extension_instance_id=ext_...
```

The extension transport is ChatGPT-only, experimental, explicit, and currently
macOS/Linux-only. Do not use it as a general browser interpreter, and do not
silently fall back to CDP after browser-side side effects have started.
For extension-native workflows, do not run plain `yoetz browser check`; that
checks the default CDP/browser stack and may trigger Chrome's remote-debugging
approval. Use `yoetz browser check --transport chrome-extension-native` or
`yoetz browser extension doctor --chatgpt` instead.

For autonomous ChatGPT Pro work, prefer focused bundles and expect long waits:
15-20 minutes is normal for large file analysis, and the default
`wait_timeout_ms` is 30 minutes. Use JSON output and a durable result file;
progress is emitted to stderr so stdout remains valid JSON. Example:

```bash
YOETZ_AGENT=1 yoetz browser recipe \
  --recipe chatgpt \
  --transport chrome-extension-native \
  --bundle "$BUNDLE" \
  --format json \
  --output-final /tmp/yoetz-chatgpt-review.json \
  --var extension_instance_id=ext_...
```

When `--bundle` points at a Yoetz session `bundle.md`, the ChatGPT composer
prompt defaults to the user prompt stored in the adjacent `bundle.json`; use
`--var prompt=...` only when intentionally overriding that prompt for a test or
manual run.

Keep the process attached until it returns. Parse the JSON `response` as the
model's answer to the prompt; Yoetz does not attach pass/fail/review semantics
to the text. Hand the response back according to the user's current task. If
that task explicitly calls for iteration, apply the requested follow-up work,
build a fresh focused bundle, and run a new native-extension recipe with a new
`run_id` and `--output-final`. If the answer is obviously truncated or
nonsensical, report that the model response was unusable; rerun only when the
current user task calls for another attempt.

If a run fails after upload/send/wait, inspect the marked tab with the run id
from the error instead of rerunning blindly; reruns after browser side effects
can duplicate the submission.
For `response_extraction_failed`, compare the tab and diagnostics: if the owned
tab itself only shows a tiny/truncated assistant fragment, report that ChatGPT
returned an unusable answer; if the tab visibly contains the full answer,
preserve the tab and report the extraction miss.

Manual Chrome-side install/update is part of this path: unzip the release
artifact, load the extracted extension from `chrome://extensions` with Developer
mode enabled, and click the extension reload button after replacing files. Then
run `yoetz browser extension reconnect --chatgpt` and `doctor --chatgpt`.
From a source checkout, load `extensions/chatgpt-native`; from a release zip,
load the extracted zip directory itself.
For agent-driven setup, prefer `yoetz browser extension setup --chatgpt
--open-chrome`; it installs the host, opens Chrome's extension page, and prints
the exact folder to select. Chrome still requires the **Load unpacked** UI
confirmation for local extensions.
When multiple Chrome profiles have the extension loaded, pass
`--var profile_email=<email>` if Chrome exposes one, or the stable
`--var extension_instance_id=<id>` shown by `status --chatgpt`.

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

# By default yoetz selects GPT-5.4 Pro with Extended enabled, preferring a clear
# unavailable-model error over silent tier fallback. Override it only if needed.
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE" --var model=gpt-5-4-pro

# Every request opens a fresh, yoetz-owned ChatGPT tab marked with ?_yoetz=<run-id>
# so your own ChatGPT conversations are never touched. `--var thread=reuse` is
# rejected — yoetz is always a fresh-tab consumer.
```

The wait loop reports `completion_reason` in its JSON output:
- `copy_button` — the strong signal: a copy control rendered on the new
  assistant message (response is fully streamed).

There is no generic "stable text" completion fallback for the extension
transport. If ChatGPT text is visible but final controls have not appeared yet,
Yoetz keeps waiting and prints the inspect command so the owning agent can
debug the live tab instead of accepting a partial prefix.

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
| `auto-connect probe timed out` | Chrome dialog is probably showing. Click Allow. If Chrome will not accept the dialog, switch to the explicit `--cdp` flow above. Install `dev-browser` first; `agent-browser` remains a fallback. |
| `chatgpt login required` | Chrome was reached but the wrong profile/tab was used. Open ChatGPT in the target Chrome profile first, or connect that profile explicitly with `--cdp`, then retry. |
| `daemon already running` | Run `yoetz browser attach` to check connection. If the daemon is stale, use `yoetz browser reset`, not `agent-browser close` directly. |
| `agent-browser failed` | Ensure `npx agent-browser --version` works, or `npm install -g agent-browser` |
| `dev-browser failed` | Ensure `dev-browser --help` works, verify Chrome remote debugging is enabled, and retry with `--cdp` if you need a specific Chrome profile. |
| `chrome-extension-native` not connected | Run `yoetz browser extension install-host --chatgpt`, load/reload the extension in `chrome://extensions`, then run `yoetz browser extension reconnect --chatgpt` and `doctor --chatgpt`. |
| Multiple extension profiles | Run `yoetz browser extension status --chatgpt`; pass `--var extension_instance_id=ext_...` for deterministic routing. `profile_email` is only a Chrome-profile guard when Chrome exposes it. |
| Extension terminal phase after upload/send/wait | Do not rerun automatically. Continue in the Yoetz-owned ChatGPT tab or intentionally rerun knowing it may duplicate a submission. |
| Recipe not found | Use `--recipe chatgpt` (name) or full path. Check `brew --prefix`/share/yoetz/recipes/ |
| `cookie extraction failed` | Legacy path: ensure Node >= 24.4, log into ChatGPT in Chrome, close Chrome, `yoetz browser sync-cookies` |

### dev-browser Fallback

When `yoetz browser recipe` needs manual browser automation, use `dev-browser` directly against the authenticated Chrome session:

1. **Create bundle**: `BUNDLE=$(yoetz bundle -p "Review" -f src/**/*.ts --format json | jq -r .artifacts.bundle_md)`
2. **Connect to Chrome**:
   ```bash
   dev-browser --connect <<'EOF'
   const page = await browser.getPage("chatgpt");
   await page.goto("https://chatgpt.com/");
   console.log(await page.title());
   EOF
   ```
3. **Use the Playwright-style API**:
   `browser.getPage(name)`, `page.goto(url)`, `page.click(selector)`, `page.fill(selector, text)`, `page.evaluate(fn)`, `page.title()`
4. **Use file helpers when needed**:
   `saveScreenshot(buf, name)`, `writeFile(name, data)`, `readFile(name)`
5. **Target a specific Chrome profile**: prefer opening ChatGPT in that profile first, or connect to its explicit CDP endpoint with `yoetz browser recipe --cdp http://127.0.0.1:9222 ...`

This keeps the default path aligned with the same browser transport users already rely on outside Yoetz.

### How it works

The browser module connects to your running Chrome via CDP (Chrome DevTools Protocol):
- **chrome-devtools-mcp** (primary): built into yoetz, backed by `headless_chrome`, attaches directly to your logged-in Chrome session
- **dev-browser** (fallback): Playwright-based transport for the same live-attach flow
- **agent-browser** (fallback 2): browser automation fallback with cookie/profile fallback support
- **Cookie sync** (final fallback): extracts cookies from Chrome's encrypted store, injects into agent-browser only after live attach paths are exhausted
- Extension-free by default: no browser extension is required for normal browser
  recipes; `chrome-extension-native` is the explicit experimental exception for
  ChatGPT Pro
- Daemon model: one persistent connection per session, reused across invocations until you explicitly run `yoetz browser reset`

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

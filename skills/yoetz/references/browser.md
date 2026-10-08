# Yoetz Browser Recipes

Browser recipes send a Yoetz bundle to a web-only model (ChatGPT Pro, Claude)
through the user's logged-in Chrome. `yoetz browser <cmd> --help` is the source
of truth for flags.

## Contents

- [Built-in recipes](#built-in-recipes)
- [Transport selection](#transport-selection)
- [Native extension setup](#native-extension-setup)
- [CDP setup](#cdp-setup)
- [Run a recipe](#run-a-recipe)
- [Timeouts and long runs](#timeouts-and-long-runs)
- [Conversations: --thread, --followup, conversation=](#conversations---thread---followup-conversation)
- [Results and completion_reason](#results-and-completion_reason)
- [Side effects and recovery](#side-effects-and-recovery)
- [Troubleshooting](#troubleshooting)

## Built-in recipes

| Recipe | Target | Selection |
|--------|--------|-----------|
| `chatgpt` | GPT-6 family (labeled `Latest` or `GPT-6`), literal `Pro` effort tier, Chat surface; reports `model_used: "Latest Pro"` | Fail-closed: an unproven selection stops the run before upload/send |
| `claude` | Fable 5, Effort Max | Fail-closed: the model radio and Max are re-read after every click |

Do not pass `model`, `extended`, `effort`, or `thinking` overrides; the CLI
rejects them. `--model-strategy current` (ChatGPT only) leaves the picker
untouched for picker-drift diagnosis; do not use it for routine runs.

A recipe can also be a path (`--recipe ./my-recipes/custom.yaml`). The bundled
`gemini` recipe is legacy: a minimal `agent-browser` sequence without the typed
fail-closed contract.

## Transport selection

1. **Native extension.** When `yoetz browser extension status --chatgpt` (or
   `--claude`) reports `connected`, the matching built-in recipe uses
   `chrome-extension-native` as its only transport and fails closed. No Chrome
   remote-debugging approval is involved.
2. **Extension-free order** (no connected extension):
   `chrome-devtools-mcp` (built into yoetz, direct CDP) → `dev-browser` →
   `agent-browser` → manual upload. `dev-browser` and `agent-browser` are
   optional `npm install -g` fallbacks.

Opt out of the native auto-select with `--transport <other>`, a pinned
`transports:` list in the recipe yaml, or an explicit `--cdp`, `--browser-id`,
or managed `--profile`. CDP fallback after a native failure needs both
`--transport chrome-extension-native --allow-cdp-fallback`, and it applies only
before browser-side side effects.

`yoetz browser check --format json` follows the same rule: it auto-selects the
extension when connected and returns `auto_selected: true`. Plain `check` is
ChatGPT-scoped; add `--claude` for Claude.

Connection priority for CDP transports: explicit `--cdp` > auto-connect >
cookie state > managed profile. Yoetz keeps one live-attach daemon and never
recycles it silently; recover explicitly with `yoetz browser reset`.

## Native extension setup

One package ("Yoetz Native Transport") serves ChatGPT and Claude. Every
`extension` subcommand takes `--chatgpt` or `--claude`. The native host runs on
macOS and Linux only.

```bash
yoetz browser extension setup --chatgpt --open-chrome   # or --claude
yoetz browser extension doctor --chatgpt
yoetz browser extension status --chatgpt --format json
yoetz browser check --transport chrome-extension-native --format json
```

`setup` installs the native host, copies the extension to
`$YOETZ_DIR/chatgpt-native-extension`, opens `chrome://extensions`, and prints
the folder. In Chrome, enable Developer mode and **Load unpacked** that exact
folder, in the profile that is logged in to the site. Never load a repo
checkout. After a Yoetz upgrade, run `yoetz browser extension update --chatgpt`;
it refreshes the managed copy, reloads Chrome, and verifies the loaded version.

Maintenance: `install-host`, `reconnect`, `reload`, `update`,
`inspect --run-id <id>` (or `--list`), and `grant-identity` (opt-in
`profile_email`).

**Multiple profiles.** When more than one Chrome profile has the extension
loaded, pass `--var extension_instance_id=<id>` from `status`. Use
`--var profile_email=<email>` only as a guard when Chrome exposes the email.

**Concurrency.** ChatGPT and Claude jobs can run at the same time through one
profile; each job owns a background tab. Give every parallel run its own bundle
session; reusing one bundle fails with `session_busy` before browser work.
`setup`, `update`, and `reload` fail closed while a recipe runs; they never
swap the extension mid-run.

## CDP setup

Use this path only when the extension is not installed.

1. In Chrome, open `chrome://inspect/#remote-debugging` and enable "Discover
   network targets".
2. Run `yoetz browser attach --format json`. On the first live attach Chrome
   can show "Allow remote debugging?"; click **Allow** once.
3. Run a recipe (below).

Explicit `--cdp` on `attach`, `check`, `recipe`, and `login` skips
auto-discovery but not Chrome's approval dialog for that browser. If the dialog
freezes, start a separate Chrome (or Chrome for Testing) with its own profile:

```bash
chrome --remote-debugging-port=9222 --user-data-dir="$HOME/chrome-debug"
yoetz browser attach --cdp http://127.0.0.1:9222 --format json
```

Log in to the site in that profile first (`yoetz browser login --cdp ...`).

Legacy cookie sync (needs Node 24.4+): log in to ChatGPT in Chrome, close
Chrome, then `yoetz browser sync-cookies` and
`yoetz browser check --transport agent-browser --format json`. On macOS, click
**Always Allow** on the `Chrome Safe Storage` Keychain prompt.

## Run a recipe

```bash
PROMPT='Review the attached Rust code for correctness and regressions.
Give file and line evidence for every finding. Flag missing context instead of guessing.'
BUNDLE=$(yoetz bundle -p "$PROMPT" -f "src/*.rs" --format json | jq -er '.artifacts.bundle_md')
YOETZ_AGENT=1 yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE" \
  --format json --output-final ./yoetz-chatgpt.json 2> ./yoetz-chatgpt.log
```

- Use `--recipe claude` for Claude. Pass a caller-supplied bundle unchanged.
- The composer prompt defaults to the prompt stored in the bundle's
  `bundle.json`. Use `--var prompt=...` only to override it on purpose.
- Progress goes to stderr, so stdout stays valid JSON. Write stderr to a file;
  do not pipe the run through `tail` or `head`.
- `response` is the model's answer. Yoetz adds no pass/fail meaning to it.
- Every run opens a new Yoetz-owned background tab marked `?_yoetz=<run-id>`;
  the user's own tabs are not touched. `--keep-tab` keeps it after success.
  `--browser-id <id>` selects a local Chrome by its `/devtools/browser/<id>`
  suffix.

## Timeouts and long runs

ChatGPT Pro reviews of large bundles often take 15-20 minutes. Keep waiting on
the original process; sparse progress is not a hang. Never start a second run
because the first is slow.

| Var | Default | Bounds |
|-----|---------|--------|
| `wait_timeout_ms` | 5400000 (90 min) | the response wait |
| `send_timeout_ms` | 120000 | the send |
| `upload_timeout_ms` | 120000, plus 5000 per MiB of bundle, capped at 3600000 | the attach |

Set them with `--var <name>=<ms>`, on `chatgpt` and `claude`. An upload timeout
("page did not reach the requested state within <n>ms") is processing latency,
not transfer size: the attachment can still land after the deadline. Inspect
the tab, then raise `upload_timeout_ms` instead of rerunning blind.

## Conversations: --thread, --followup, conversation=

Resume is native-extension only; other transports reject it before side
effects. The three forms are mutually exclusive. The caller decides when to
resume and when to start fresh.

- `--var conversation=<id|url>` continues a known conversation. Successful runs
  return `conversation_id` and `conversation_url`; save them for this.
- `--followup <session-id|conversation-id|url>` resumes; a session id resumes
  from that session's stored conversation.
- `--thread <label>` addresses a conversation by a stable name. It also needs a
  managed bundle session (a named `.md` plus `bundle.json`).

```bash
# Start a new conversation and point the label at it.
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE" \
  --thread release-review --fresh --format json
# Reuse it; wait up to 5 minutes if another process holds the label.
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE" \
  --thread release-review --on-thread-conflict wait:5m --format json
```

`--fresh` and `--on-thread-conflict` need `--thread`. Conflict modes: `fail`
(default), `wait` (no limit), `wait:<n>ms|s|m|h`, `fork` (new unlabelled
conversation; the label does not move). `--var thread=reuse` is rejected.

`--allow-duplicate-prompt` allows the same prompt+bundle hash to go to the same
conversation again. A session-id follow-up compares with that session's last
prompt hash, not the conversation head.

## Results and completion_reason

Success values of `completion_reason` (native extension):

- `copy_button`, `stable_idle`, `backend_api`: the turn is final.
- `stable_idle_unscoped_copy_button`: recovery for long responses; the text was
  stable and idle and a new copy control appeared that could not be scoped to
  the turn. Read the response before you use it.

The CDP path reports `copy_button` or `stable_idle_fallback`.

Failures come back as an error code; the code is what you act on:

| Error code | `completion_reason` | Action |
|------------|---------------------|--------|
| `response_timeout` | `timeout` | If `still_generating: true`, the answer will finish in the tab: resume with `--var conversation=<id>` or inspect. Else inspect. |
| `response_finality_stalled` | `non_streaming_turn_with_persistent_stop` | Inspect the tab; do not rerun. |
| `response_extraction_failed` | `final_affordance_without_scoped_text` | Inspect. A tiny or cut fragment in the tab = unusable answer; a full answer in the tab = extraction miss. Report which. |
| `content_policy_flagged` | `content_policy_flagged` | Rephrase; do not resend the same prompt. |
| `rate_limited`, `rate_limit_cooldown_active` | — | Hard stop. Ask a human. |

If progress says `waiting for final assistant controls`, the text is visible
but not final. Do not use it and do not start another run.

## Side effects and recovery

**Never rerun after upload, send, or wait has started.** A rerun can duplicate
the submission. Errors after those phases leave the owned tab open and print an
inspect command:

```bash
yoetz browser extension inspect --chatgpt --run-id <run-id>   # or --claude
```

Inspect first, then decide. Do not switch transports after a side effect.

**`rate_limited` is a hard stop.** ChatGPT's "Too many requests" modal limits
conversation-history reads. Every page load is a read, so retries and probes
keep the limit in place. A typed `rate_limited` at any phase arms a
profile-wide cooldown; while it holds, new jobs fail at once with
`rate_limit_cooldown_active` and `cooldown_remaining_ms` (also shown by
`extension status --chatgpt`). Do not loop; report and wait for a human.

Do not run a live canary as a readiness step. Use
`yoetz browser extension canary --chatgpt --live` only when `doctor` or
`inspect` points to a site-side problem and the user asks for a probe.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| Extension not connected | `yoetz browser extension setup --chatgpt --open-chrome` (or `--claude`), load the printed folder, then `extension doctor`. |
| Extension loaded from the wrong folder or stale | `yoetz browser extension update --chatgpt`; `status` and `doctor` flag wrong-path loads. |
| Multiple extension profiles | `extension status --chatgpt --format json`, then `--var extension_instance_id=ext_...`. |
| "Allow remote debugging?" dialog | Click **Allow** and retry. If frozen, use the separate-profile `--cdp` path. |
| `auto-connect probe timed out` | The approval dialog is probably open. Click Allow, or use explicit `--cdp`. |
| Login required | Wrong profile or tab. Open the site in the target profile, or connect that profile with `--cdp`. |
| `daemon already running` / stale daemon | `yoetz browser attach --format json`; if stale, `yoetz browser reset`. |
| `dev-browser failed` / `agent-browser failed` | Check `dev-browser --help` / `npx agent-browser --version`; these are optional fallbacks. |
| `session_busy` | Another run uses the same bundle session. Make a new bundle. |
| Recipe not found | Use `--recipe chatgpt` / `claude` or a full path. |
| `cookie extraction failed` | Node 24.4+, log in to ChatGPT, close Chrome, `yoetz browser sync-cookies`. |

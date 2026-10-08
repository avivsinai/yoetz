# yoetz

<p>
  <img src="assets/branding/yoetz-quorum-mark.svg" alt="Yoetz quorum mark" width="96" height="96">
</p>

[![CI](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml/badge.svg)](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml)
[![Latest Release](https://img.shields.io/github/v/release/avivsinai/yoetz)](https://github.com/avivsinai/yoetz/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust: 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)

Yoetz is a CLI for coding agents and terminal workflows. It bundles repository
context (gitignore-aware), sends it to one model or to a council of models in
parallel, reviews diffs, generates images and video, and drives web-only models
such as ChatGPT Pro and Claude's Fable 5 through browser recipes. Output is
structured JSON, and each run writes replayable session artifacts.

## Quickstart

```bash
brew install avivsinai/tap/yoetz

# No API key needed:
yoetz bundle -p "Summarize this project" -f README.md --format json

# With a provider key (see Configuration):
export OPENROUTER_API_KEY=...
yoetz models frontier --format json
yoetz ask -p "Explain this file" -f src/main.rs --provider openrouter \
  --model <id from models frontier> --format json
```

## Install

Homebrew (`brew install avivsinai/tap/yoetz`) installs everything: the binary,
the browser recipes and scripts, and the native Chrome extension source.

### Scoop (Windows)

```powershell
scoop bucket add avivsinai https://github.com/avivsinai/scoop-bucket
scoop install yoetz
```

On Windows the API-backed commands work; the native-extension transport needs a
native messaging host that Yoetz registers on macOS and Linux only.

### Prebuilt archives

Download an archive and `SHA256SUMS.txt` from the
[latest release](https://github.com/avivsinai/yoetz/releases/latest). Each
archive contains the `yoetz` binary plus `recipes/` and `scripts/`. Browser
recipes need those two directories, so install them too:

```bash
mkdir yoetz-dist && cd yoetz-dist
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-aarch64-apple-darwin.tar.gz
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt
shasum -a 256 -c SHA256SUMS.txt --ignore-missing   # sha256sum on Linux
tar xzf yoetz-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin ~/.local/share/yoetz
mv yoetz ~/.local/bin/                     # must be on PATH
cp -R recipes scripts ~/.local/share/yoetz/
```

Yoetz also finds `recipes/` and `scripts/` next to the binary, in
`<binary dir>/../share/yoetz/`, and in `$XDG_DATA_HOME/yoetz/`, so keeping the
extracted directory together works too.

The native Chrome extension ships as a separate release asset,
`yoetz-chatgpt-native-extension-<version>.zip`. Use the zip from the same
release as your binary: `setup` refuses a source whose content does not match
the CLI build. Unzip it into `<binary dir>/../share/yoetz/extensions/chatgpt-native/`
(with the layout above, `~/.local/share/yoetz/extensions/chatgpt-native/`), or
unzip it anywhere and set `YOETZ_CHATGPT_NATIVE_EXTENSION_DIR` to it.

### From source

```bash
cargo install --git https://github.com/avivsinai/yoetz --locked
```

This installs only the binary. For browser recipes, copy `recipes/` and
`scripts/` from the release archive of the same version into
`~/.local/share/yoetz/`, and set `YOETZ_CHATGPT_NATIVE_EXTENSION_DIR` to the
unzipped extension, as shown above.

### Agent skill

The [yoetz skill](skills/yoetz/SKILL.md) teaches Claude Code, Codex CLI, and
compatible agents to call the CLI safely. Install it with:

```text
/plugin marketplace add avivsinai/skills-marketplace
/plugin install yoetz@avivsinai-marketplace
```

Or run `npx skills add avivsinai/yoetz` (skills.sh) or
`npx skild install @avivsinai/yoetz` (skild.sh).

## Configuration

Environment variables are enough for most users:

| Variable | Used for |
| --- | --- |
| `OPENROUTER_API_KEY` | OpenRouter |
| `OPENAI_API_KEY` | OpenAI |
| `GEMINI_API_KEY` | Gemini |
| `ANTHROPIC_API_KEY` | Direct Anthropic-compatible provider configs |
| `XAI_API_KEY` | Direct xAI/OpenAI-compatible provider configs |
| `ZAI_API_KEY` | Direct Z.AI/OpenAI-compatible routing |
| `LITELLM_API_KEY` | LiteLLM proxy |
| `YOETZ_DIR` | State directory (sessions, extension state); default `~/.yoetz` |
| `YOETZ_CONFIG_PATH` | An extra config file, loaded after the defaults |

Config files load in this order, and a later file overrides an earlier one:
`~/.yoetz/config.toml`, `~/.config/yoetz/config.toml`,
`$XDG_CONFIG_HOME/yoetz/config.toml`, repo-local `./yoetz.toml`, then
`$YOETZ_CONFIG_PATH`. `--config-profile <name>` then overlays
`profiles/<name>.toml` from the same home directories and `./yoetz.<name>.toml`.
Repo-local files are untrusted: Yoetz applies only their `[notifications]`
table and warns about the rest.

```toml
[defaults]
provider = "openrouter"
# model = "<id from yoetz models resolve>"

[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
kind = "openai-compatible"
```

[docs/config.example.toml](docs/config.example.toml) shows every section.
`[frontier].families` sets the default lab list for `models frontier`;
`--all` and `--family` bypass it.

Global flags: `--timeout-secs` (default `180`) bounds provider HTTP calls and
local Cursor CLI calls. `--allow-unknown` permits model IDs absent from the
registry; use it only for self-hosted models.

Sessions: `ask` writes artifacts to `$YOETZ_DIR/sessions/<id>/`. Skip them
with `ask --no-session` or `[sessions] no_session = true`; JSON output then has
`session_dir: ""` and `response_json: null`. `[sessions] max_age_days` and
`max_count` prune old sessions at startup (off by default; `0` for `max_count`
removes all completed sessions but keeps active writers).

## Workflows

Resolve live model IDs instead of hard-coding them. The examples use this
helper; `jq -e` fails when nothing matches, so a `set -e` script stops instead
of passing an empty `--model`:

```bash
set -euo pipefail
frontier_id() { yoetz models frontier --family "$1" --format json | jq -er '.[0].model.id // empty'; }
```

`yoetz models sync` refreshes the registry; `yoetz models list -s <text>` and
`yoetz models resolve <query>` search it.

### Ask with files, images, or video

```bash
MODEL_ID=$(frontier_id openai)
yoetz ask -p "Explain the error handling tradeoffs in this file" \
  -f crates/yoetz-cli/src/main.rs --provider openrouter --model "$MODEL_ID" --format json

GEMINI_MODEL=$(frontier_id gemini)
yoetz ask -p "Describe this diagram" --image diagram.png --provider gemini --model "$GEMINI_MODEL" --format json
yoetz ask -p "Summarize this clip" --video demo.mp4 --provider gemini --model "$GEMINI_MODEL" --format json
```

Use `--image-mime` or `--video-mime` for signed URLs or extensionless files.

### Run a council

```bash
OPENAI_MODEL=$(frontier_id openai)
GEMINI_MODEL=$(frontier_id gemini)
XAI_MODEL=$(frontier_id xai)
yoetz council -p "Which API shape is safer for agents?" -f crates/yoetz-core/src/types.rs \
  --models "$OPENAI_MODEL,$GEMINI_MODEL,$XAI_MODEL" --format json
```

`--models` is explicit on purpose: pass the IDs from `models frontier` or
`models resolve` verbatim.

### Review a diff and apply a patch

```bash
yoetz review diff --staged --format json
yoetz review file --path crates/yoetz-core/src/bundle.rs --format json

# A response schema makes patches extractable:
cat > patches-schema.json <<'JSON'
{"type": "object", "required": ["patches"], "additionalProperties": false,
 "properties": {"patches": {"type": "array", "items": {"type": "string"}}}}
JSON

yoetz review diff --staged --response-schema patches-schema.json --format json > review.json
jq -r '.content | fromjson | .patches | join("\n")' review.json > review.patch
yoetz apply --patch-file review.patch --check   # git apply --check; changes nothing
yoetz apply --patch-file review.patch
```

Without `--response-schema`, `content` is free-form text. `yoetz apply` runs
`git apply`; it does not judge the findings. Read the patch first.

### Bundle for another tool

```bash
yoetz bundle -p "Review the browser transport design" --name browser-transport-review \
  -f "crates/yoetz-cli/src/browser*.rs" -f recipes/chatgpt.yaml --format json
```

The JSON points to `$YOETZ_DIR/sessions/<id>/<name>_<timestamp>.md`. Without
`--name`, Yoetz derives the name from the prompt.

### Generate media

```bash
gemini_id() { yoetz models list -s "$1" --format json \
  | jq -er '[.models[].id | select(startswith("gemini/"))] | .[0] // empty | sub("^gemini/"; "")'; }
IMAGE_MODEL=$(gemini_id imagen)
VIDEO_MODEL=$(gemini_id veo)
yoetz generate image -p "A clean product diagram of a terminal-first LLM router" \
  --provider gemini --model "$IMAGE_MODEL" --format json
yoetz generate video -p "A short UI walkthrough" --provider gemini --model "$VIDEO_MODEL" --format json
```

Generation needs an explicit `--provider` (`openai` or `gemini`) and a model
that provider accepts. `--image` on `generate image` edits an input image.

### Consult through Cursor CLI

Yoetz can use an authenticated local Cursor CLI as a text backend. Install it
and run `cursor-agent login` (or `agent login`), then:

```bash
CURSOR_MODEL=$(yoetz models list --provider cursor -s grok --format json \
  | jq -er '[.models[].id | select(startswith("cursor-grok") and endswith("-xhigh"))] | .[0] // empty')
yoetz ask -p "Find the root cause in this error path" -f "crates/**/*.rs" \
  --provider cursor --model "$CURSOR_MODEL" --format json

# In a mixed council, prefix the Cursor model so routing stays explicit:
OTHER_MODEL=$(frontier_id openai)
yoetz council -p "Challenge this design" --models "cursor/$CURSOR_MODEL,$OTHER_MODEL" --format json
```

Through Yoetz, Cursor is text only: media, response schemas, explicit
output-token limits, and dollar budgets fail closed. `--temperature` accepts
only the default `0.1`.

## Agent Usage

```bash
export YOETZ_AGENT=1
yoetz ask -p "Return JSON only" -f src/lib.rs --format json --output-final result.json
```

- `--format json` keeps stdout parseable; progress and diagnostics go to stderr.
- `--response-schema <path>` asks the model for structured output (`ask`,
  `council`, `review`). `--output-schema <path>` validates Yoetz's own final
  JSON envelope before `--output-final <path>` writes it.
- `--max-cost-usd` and `--daily-budget-usd` are local preflight checks, not
  provider-side limits.

## Browser Recipes: ChatGPT Pro And Claude

Browser recipes use web-only model surfaces from the terminal. The `chatgpt`
recipe selects the `Latest` family at the `Pro` tier; the `claude` recipe
selects Fable 5 with Effort Max. Both fail closed: if Yoetz cannot prove the
requested model is selected, it stops instead of downgrading. The bundled
`gemini` recipe is a legacy `agent-browser` sequence without that contract.

```bash
yoetz browser check --format json
yoetz browser recipe --recipe chatgpt --bundle ~/.yoetz/sessions/<id>/<name>.md --format json
yoetz browser check --claude --format json
yoetz browser recipe --recipe claude --bundle ~/.yoetz/sessions/<id>/<name>.md --format json
```

### Transports

Recipes run over `chrome-devtools-mcp`, `dev-browser`, `agent-browser`, or
`chrome-extension-native`. The default is extension-free, unless the native
extension reports `connected` for the site: then the recipe uses only
`chrome-extension-native` and fails closed on error.

- `--transport <name>` selects one transport explicitly.
- `--transport chrome-extension-native --allow-cdp-fallback` is the only way
  to fall back to CDP after a native-extension failure, and only if the failure
  came before any browser-side side effect.
- `--browser-id <id>` selects a local Chrome by its `/devtools/browser/<id>`
  suffix; `--cdp <url>` connects to an explicit endpoint.

### Native extension

One extension package (Yoetz Native Transport) serves both sites. Pick the site
with `--chatgpt` or `--claude`:

```bash
yoetz browser extension setup --chatgpt --open-chrome
yoetz browser extension doctor --chatgpt
yoetz browser extension status --chatgpt --format json
yoetz browser extension update --chatgpt
```

`setup` copies the packaged source to `$YOETZ_DIR/chatgpt-native-extension`.
Load that directory unpacked in the Chrome profile that hosts your AI sessions,
never a repo checkout. The native host and managed copy are machine-wide:
setup, update, and reload fail with `extension_lifecycle_busy` while a recipe
runs, and an older CLI can overwrite a newer managed copy, so keep one Yoetz
version per machine.

- With several Chrome profiles loaded, route with
  `--var extension_instance_id=<id>` from `extension status`, or opt into
  `profile_email` routing with `yoetz browser extension grant-identity`.
- ChatGPT and Claude runs may share one profile; each job owns a background
  tab. Give each parallel run its own bundle session directory, or it fails
  with `session_busy`.
- `yoetz browser extension inspect --chatgpt --run-id <run>` inspects a failed
  run's page; without `--run-id` it lists active jobs and exits 2.
- `yoetz browser extension dump-conversation --chatgpt --run-id <run> --path answer.html`
  writes a kept tab's conversation HTML to a file. It refuses an in-flight job
  unless you pass `--allow-live-job`.

### Conversations

Resume with `--var conversation=<id|url>` or `--followup <session-id|conversation-id|url>`
(native transport). For a stable address, use `--thread <label>`:

```bash
# New conversation; point the label at it.
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE" --thread release-review --fresh --format json

# Reuse the label. Conflict policy: fail (default), wait, wait:<duration>, or fork.
yoetz browser recipe --recipe chatgpt --bundle "$BUNDLE" --thread release-review \
  --on-thread-conflict wait:5m --format json
```

`--fresh` and `--on-thread-conflict` require `--thread`. A `wait:<duration>`
accepts `ms`, `s`, `m`, or `h` and fails with `thread_busy_timeout` when it
expires; `fork` starts an unlabelled conversation and leaves the label as is.
`--thread`, `--followup`, and `--var conversation=` are mutually exclusive.
`--keep-tab` keeps the Yoetz-owned tab after a successful run.

The `claude` recipe warns when a bundle is likely to become retrieval-backed
instead of inline. The threshold is `inline_warn_tokens` (default 150,000);
`--var inline_warn_tokens=0` disables the warning. It does not change byte
limits.

## Provider Support

Capabilities vary by model. Check `yoetz models frontier`, `models list`, and
`models resolve` before pinning a model.

| Provider | Text | Vision | Image Gen | Video Gen | Video Understanding |
| --- | --- | --- | --- | --- | --- |
| OpenRouter | Yes | Model-dependent | No | No | No |
| OpenAI | Yes | Yes | Yes | Yes (Sora) | No |
| Gemini | Yes | Yes | Yes (Imagen) | Yes (Veo) | Yes |
| Cursor CLI | Yes | No | No | No | No |
| Z.AI | Yes | Model-dependent | No | No | No |
| LiteLLM-compatible | Yes | Model-dependent | No | No | No |

Anthropic, xAI, and Z.AI models are usually reached through OpenRouter; you can
also configure them as direct providers.

## Safety And Trust

Bundles are prompt input, not a control channel. Treat bundled repository
content, issues, logs, and pasted browser output as untrusted. Keep intent in
CLI flags and the prompt, do not bundle secrets, and review generated changes
before you apply them. Cursor calls get only the rendered consult, in an
isolated temporary workspace, in Ask + sandbox mode without force, YOLO, or MCP
auto-approval.

Security policy: [SECURITY.md](SECURITY.md).

## Development

```bash
git clone https://github.com/avivsinai/yoetz.git
cd yoetz
cargo build
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Browser scripts and the extension (CI uses Node 24):

```bash
./scripts/build-live-cdp-daemon.sh --check
./scripts/build-chatgpt-native-extension.sh --check
npm ci --prefix extensions/chatgpt-native --ignore-scripts
npm run lint --prefix extensions/chatgpt-native
node --test extensions/chatgpt-native/tests/*.test.js
```

Design: [ARCHITECTURE.md](ARCHITECTURE.md). Agent and contributor rules:
[CLAUDE.md](CLAUDE.md).

## License

[MIT](LICENSE)

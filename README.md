# yoetz

<p>
  <img src="assets/branding/yoetz-quorum-mark.svg" alt="Yoetz quorum mark" width="96" height="96">
</p>

[![CI](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml/badge.svg)](https://github.com/avivsinai/yoetz/actions/workflows/ci.yml)
[![Latest Release](https://img.shields.io/github/v/release/avivsinai/yoetz)](https://github.com/avivsinai/yoetz/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust: 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)

Fast CLI for routing code and media work through the right LLM path: direct API
calls when available, multi-model councils when you need agreement, and browser
recipes for web-only models such as ChatGPT Pro.

Yoetz is built for coding agents and terminal workflows: gitignore-aware
bundles, structured JSON output, reproducible session artifacts, live model
resolution, local budget checks, and multimodal inputs across OpenAI,
OpenRouter, Gemini, and LiteLLM-compatible backends.

> Yoetz is under active development. Command behavior may change before 1.0.

## What It Does

- **Bundle** repository context into prompt-ready artifacts without fighting
  `.gitignore`.
- **Ask** one model with text, code, images, or video.
- **Council** multiple models in parallel when you want independent opinions.
- **Review** staged diffs or files with agent-readable output.
- **Generate** images and video through providers that expose generation APIs.
- **Use browser recipes** for web-only model surfaces while keeping terminal
  output parseable.

## Install

### Homebrew

```bash
brew install avivsinai/tap/yoetz
yoetz --version
```

### Scoop

```powershell
scoop bucket add avivsinai https://github.com/avivsinai/scoop-bucket
scoop install yoetz
yoetz --version
```

### Prebuilt Binaries

Download archives and checksums from the
[latest GitHub release](https://github.com/avivsinai/yoetz/releases/latest).

```bash
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/yoetz-aarch64-apple-darwin.tar.gz
curl -fLO https://github.com/avivsinai/yoetz/releases/latest/download/SHA256SUMS.txt
shasum -a 256 -c SHA256SUMS.txt --ignore-missing
tar xzf yoetz-aarch64-apple-darwin.tar.gz
sudo mv yoetz /usr/local/bin/
```

Release archives are published for macOS, Linux, and Windows targets. Use
`sha256sum` instead of `shasum -a 256` on Linux.

### From Source

```bash
cargo install --git https://github.com/avivsinai/yoetz --locked
```

## Agent Skills

Yoetz also ships an agent skill so Claude Code, Codex CLI, and compatible agent
runtimes know how to call the CLI safely: resolve live model IDs, keep stdout
parseable, bundle large context first, and use browser recipes intentionally.

### Codex / Claude Plugin Marketplace

```text
/plugin marketplace add avivsinai/skills-marketplace
/plugin install yoetz@avivsinai-marketplace
```

### skills.sh

```bash
npx skills add avivsinai/yoetz
```

### skild.sh

```bash
npx skild install @avivsinai/yoetz
```

The skill source lives at [skills/yoetz/SKILL.md](skills/yoetz/SKILL.md) and is
versioned with the CLI release metadata.

## First Run

Start with a command that needs no API key:

```bash
yoetz bundle -p "Summarize this project" -f README.md --format json
```

Then configure at least one provider. Environment variables are enough for most
users:

```bash
export OPENROUTER_API_KEY=...
export OPENAI_API_KEY=...
export GEMINI_API_KEY=...
```

For persistent configuration:

```bash
mkdir -p ~/.config/yoetz
cat > ~/.config/yoetz/config.toml <<'EOF'
[defaults]
provider = "openrouter"
# Optional after resolving a current model ID:
# model = "<id from yoetz models resolve>"

[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"
kind = "openai-compatible"
EOF
```

From a source checkout, you can start from the full example instead:

```bash
cp docs/config.example.toml ~/.config/yoetz/config.toml
```

Yoetz also supports `YOETZ_CONFIG_PATH`, repo-local `./yoetz.toml`, and config
profiles. See [docs/config.example.toml](docs/config.example.toml) for the
shape.

Resolve live model IDs before putting them in scripts:

```bash
yoetz models sync
yoetz models frontier --format json
yoetz models frontier --family anthropic --format json
```

## Common Workflows

### Ask With File Context

```bash
MODEL_ID=$(yoetz models frontier --family openai --format json | jq -r '.[0].model.id')
yoetz ask \
  -p "Explain the error handling tradeoffs in this file" \
  -f crates/yoetz-cli/src/main.rs \
  --provider openrouter \
  --model "$MODEL_ID" \
  --format json
```

### Review A Diff

```bash
yoetz review diff --staged --format json
yoetz review file --path crates/yoetz-core/src/bundle.rs --format json
```

### Run A Council

```bash
OPENAI_MODEL=$(yoetz models frontier --family openai --format json | jq -r '.[0].model.id')
GEMINI_MODEL=$(yoetz models frontier --family gemini --format json | jq -r '.[0].model.id')
XAI_MODEL=$(yoetz models frontier --family xai --format json | jq -r '.[0].model.id')

yoetz council \
  -p "Which API shape is safer for agents?" \
  -f crates/yoetz-core/src/types.rs \
  --models "$OPENAI_MODEL,$GEMINI_MODEL,$XAI_MODEL" \
  --format json
```

`--models` is explicit on purpose. Pick current IDs from `yoetz models frontier`
or `yoetz models resolve`, and pass the returned IDs verbatim. Avoid using
stale provider names or hand-written wrapper paths.

### Bundle For Another Tool

```bash
yoetz bundle \
  -p "Review the browser transport design" \
  -f "crates/yoetz-cli/src/browser*.rs" \
  -f recipes/chatgpt.yaml \
  --format json
```

The JSON response points to session artifacts under `~/.yoetz/sessions/<id>/`,
including `bundle.md`.

### Multimodal Input

```bash
MODEL_ID=$(yoetz models resolve "gemini" --format json | jq -r '.[0].id')
yoetz ask -p "Describe this diagram" --image diagram.png --provider gemini --model "$MODEL_ID" --format json
yoetz ask -p "Summarize this clip" --video demo.mp4 --provider gemini --model "$MODEL_ID" --format json
```

Use `--image-mime` or `--video-mime` for signed URLs or extensionless files.

### Generate Media

```bash
IMAGE_MODEL_ID=$(yoetz models list -s image --format json | jq -r '.models[] | .id | select(startswith("gemini/")) | sub("^gemini/"; "")' | head -1)
yoetz generate image \
  -p "A clean product diagram of a terminal-first LLM router" \
  --provider gemini \
  --model "$IMAGE_MODEL_ID" \
  --format json

VIDEO_MODEL_ID=$(yoetz models list -s veo --format json | jq -r '.models[] | .id | select(startswith("gemini/")) | sub("^gemini/"; "")' | head -1)
yoetz generate video \
  -p "A short UI walkthrough" \
  --provider gemini \
  --model "$VIDEO_MODEL_ID" \
  --format json
```

Generation still requires a provider-specific `--provider` plus a model that
that provider accepts. List or resolve live models before pinning a script.

## Agent Usage

Yoetz is designed to be called by agents and scripts.

```bash
export YOETZ_AGENT=1
yoetz ask -p "Return JSON only" -f src/lib.rs --format json --output-final /tmp/yoetz-result.json
```

Useful agent-facing guarantees:

- `--format json` keeps stdout parseable.
- `--response-format json` and `--response-schema` request model-side
  structured output; `--format json` only controls the Yoetz CLI envelope.
- Progress and diagnostics use stderr where possible.
- `--output-final` writes the final response to a stable path.
- `ask`, `bundle`, `council`, and `review` create replayable session artifacts.
- Budget flags such as `--max-cost-usd` and `--daily-budget-usd` are local
  preflight/accounting aids, not provider-side hard limits.

Agent skill installation options are listed in [Agent Skills](#agent-skills).

## Browser Recipes And ChatGPT Pro

Browser recipes let Yoetz use web-only model surfaces from the terminal. The
built-in ChatGPT recipe targets ChatGPT Pro with Extended enabled and is
fail-closed: if Yoetz cannot prove the requested surface is available, it stops
instead of silently downgrading.

```bash
yoetz browser check --format json
yoetz browser recipe --recipe chatgpt --bundle ~/.yoetz/sessions/<id>/bundle.md --format json
```

The default browser stack is extension-free unless the ChatGPT native extension
is installed and connected. When connected, the ChatGPT recipe selects
`chrome-extension-native` as the only default transport. Use an explicit
`--transport <name>` when you want a different browser transport.

Native extension happy path:

```bash
yoetz browser extension setup --chatgpt --open-chrome
yoetz browser extension doctor --chatgpt
yoetz browser extension status --chatgpt --format json
yoetz browser check --transport chrome-extension-native --format json
yoetz browser recipe --recipe chatgpt --transport chrome-extension-native --bundle bundle.md --format json
```

The native-host extension transport is currently macOS/Linux-only. Windows CLI
and Scoop installs work for the API-backed Yoetz flows, but
`chrome-extension-native` setup fails closed until Windows native messaging host
registration is implemented.

For multiple loaded Chrome profiles, select the connected bridge with
`--var extension_instance_id=<id>` from `yoetz browser extension status
--chatgpt`, or opt into `profile_email` routing with `yoetz browser extension
grant-identity --chatgpt`.

The detailed browser transport model lives in [ARCHITECTURE.md](ARCHITECTURE.md).

## Provider Support

Capabilities vary by model and provider. Use `yoetz models frontier`,
`yoetz models list`, and `yoetz models resolve` against the live registry before
pinning examples.

| Provider | Text | Vision | Image Gen | Video Gen | Video Understanding |
| --- | --- | --- | --- | --- | --- |
| OpenRouter | Yes | Model-dependent | No | No | No |
| OpenAI | Yes | Yes | Yes | Yes (Sora) | No |
| Gemini | Yes | Yes | No | Yes (Veo) | Yes |
| LiteLLM-compatible | Yes | Model-dependent | No | No | No |

Anthropic and xAI models are commonly reached through OpenRouter, but can also
be configured as direct providers when you need provider-specific routing.

Common API key variables:

| Variable | Used for |
| --- | --- |
| `OPENROUTER_API_KEY` | OpenRouter |
| `OPENAI_API_KEY` | OpenAI |
| `GEMINI_API_KEY` | Gemini |
| `ANTHROPIC_API_KEY` | Direct Anthropic-compatible provider configs |
| `XAI_API_KEY` | Direct xAI/OpenAI-compatible provider configs |
| `LITELLM_API_KEY` | LiteLLM proxy |

## Safety And Trust

Bundles are prompt-input artifacts, not trusted control channels. Treat bundled
repository content, issues, logs, and pasted browser output as untrusted input.
Keep intent in explicit CLI flags and the user prompt, avoid bundling secrets,
and review generated changes before applying them.

Project trust signals:

- CI covers Rust tests, formatting, linting, dependency policy, secret scanning,
  MSRV, extension script tests, and browser smoke checks.
- Release archives ship with `SHA256SUMS.txt`.
- Security policy: [SECURITY.md](SECURITY.md).
- Code of conduct: [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Development

```bash
git clone https://github.com/avivsinai/yoetz.git
cd yoetz
cargo build
cargo test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Optional browser-extension checks:

```bash
./scripts/build-chatgpt-native-extension.sh --check
node --test extensions/chatgpt-native/tests/*.test.js
```

The Rust workspace has two crates:

- `crates/yoetz-core`: pure core types, bundling, config, registry, sessions.
- `crates/yoetz-cli`: async CLI, providers, browser transports, budgets.

See [ARCHITECTURE.md](ARCHITECTURE.md) and [CONTRIBUTING.md](CONTRIBUTING.md)
for design and contribution details.

## Release Model

Releases are cut from `main` through `./scripts/release.sh X.Y.Z` and the
resulting release PR. The merged release commit drives the tag, GitHub release
artifacts, Homebrew formula, Scoop manifest, and agent skill publication.

## License

[MIT](LICENSE)

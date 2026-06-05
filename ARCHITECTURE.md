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
│       ├── browser.rs        # Browser automation (CDP via agent-browser)
│       ├── browser_extension_native.rs  # ChatGPT native extension bridge
│       ├── chatgpt_recipe.rs # ChatGPT recipe spec assembly
│       ├── chatgpt_web.rs    # ChatGPT DOM/web helpers
│       ├── chrome_devtools_mcp/
│       │   ├── client.rs     # CDP transport client
│       │   ├── chatgpt.rs    # ChatGPT CDP recipe flow
│       │   └── mod.rs
│       ├── dev_browser.rs    # QuickJS/WASM browser recipe runner
│       ├── fuzzy.rs          # Lightweight matching helpers
│       ├── live_attach.rs    # Live Chrome attach daemon
│       ├── live_cdp_daemon.rs # Local CDP daemon integration
│       ├── budget.rs         # Daily spend tracking (file-based)
│       ├── registry.rs       # Runtime model resolution
│       └── http.rs           # Shared HTTP utilities
│
├── recipes/                  # Browser automation YAML recipes
├── skills/                   # Agent skill definitions
├── scripts/                  # Helper scripts
└── docs/                     # Configuration examples and ADRs
```

## Design Decisions

### Core vs CLI Split

`yoetz-core` contains pure, synchronous logic with no network dependencies. This makes it testable without mocking HTTP and reusable as a library. `yoetz-cli` owns all async runtime, network calls, and user interaction.

### Provider Abstraction

Rather than a trait-based provider abstraction, yoetz uses [litellm-rust](https://github.com/avivsinai/litellm-rust) as its unified LLM SDK. litellm-rust handles provider-specific API differences (auth, endpoints, request/response formats) behind a single `LiteLLM::completion()` interface.

Provider-specific code in `providers/` exists only for features not yet in litellm-rust (e.g., Gemini video generation, OpenAI image generation with specific parameters).

### Model Routing

Models use `provider/model` format (e.g., `openai/gpt-5.2`). OpenRouter models use nested format: `openrouter/anthropic/claude-sonnet-4`. The registry resolves these to the correct API endpoint and configuration.

### Session Management

Every `ask`, `council`, `review`, and `bundle` command creates a session under `~/.yoetz/sessions/<id>/`. Sessions store:
- `bundle.md` - the assembled context
- `response.json` - raw provider responses
- `metadata.json` - timing, cost, model info

This enables replay, debugging, and the `apply` command for code review suggestions.

Bundles are prompt-input artifacts, not trusted control channels. Repository
files, logs, issues, and browser transcripts inside `bundle.md` can contain
prompt-injection text, so command intent must come from explicit CLI flags and
the user's prompt; generated edits still need review before application.

### Budget Tracking

Daily spend is tracked in a local JSON file. The `--max-cost-usd` flag estimates cost before sending (using the pricing registry) and aborts if over budget. `--daily-budget-usd` accumulates across `ask`, `council`, and `review`. Generation commands do not expose budget flags yet, and multimodal `ask` currently rejects strict preflight budget enforcement until media pricing can be estimated accurately.

### Browser Mode

For models without API access (e.g., ChatGPT Pro), yoetz bundles files into markdown, then connects to the user's running Chrome via CDP (Chrome DevTools Protocol) and submits the bundle through the web UI.

Browser integrations are extension-free by default. Yoetz prefers to act as a wrapper over the underlying transport rather than reimplementing transport logic itself:

- `chrome-devtools-mcp` is the primary live-Chrome transport for ChatGPT recipes.
- `dev-browser` is the secondary live-Chrome transport.
- `agent-browser` remains the tertiary / legacy fallback when the first two transports are unavailable or a non-ChatGPT path still depends on it.
- `manual` is the explicit final fallback; it tells the user to complete the web flow manually and does not need CDP.
- Explicit CDP endpoints are forwarded to the transport unchanged; the transport owns `/json/version`, `DevToolsActivePort`, and related connection logic.

Connection priority remains connect-first: explicit CDP endpoint > auto-connect > cookie state > profile fallback.

See [ADR-001: Live Attach Owner](docs/decisions/ADR-001-live-attach-owner.md)
for the ownership model behind the live Chrome attach path.

Chrome 146+ may show a one-time "Allow remote debugging?" approval dialog for a new CDP session. The acceptance criterion is one approval per browser session, not per yoetz invocation, so yoetz avoids silently tearing down live-attach daemons in normal attach/check/recipe flows. Recovery is explicit via `yoetz browser reset`.

The experimental `chrome-extension-native` transport is the only browser
extension exception. It is ChatGPT-only, opt-in via
`yoetz browser recipe --recipe chatgpt --transport chrome-extension-native`, and
is not part of the default transport order. Its lifecycle is explicit:
`yoetz browser extension install-host --chatgpt`,
`doctor --chatgpt`, `status --chatgpt`, `reconnect --chatgpt`, and
`update --chatgpt`; `inspect --chatgpt --run-id <id>` is the read-only
diagnostic path for failed runs, `canary --chatgpt --live` is reserved for
explicit diagnostic probes, and `grant-identity --chatgpt` opts into Chrome's
`identity.email` permission when `profile_email` routing is required.
Release builds package it as a separate versioned extension zip so the CLI
archives do not make the extension path implicit. Manual Chrome-side
install/update is intentionally explicit: unzip the release artifact, load the
extracted extension via `chrome://extensions` Developer mode, reload the
extension after replacing files, then use `reconnect` and `doctor` to confirm the
native bridge and extension protocol version agree. The native-host install and
runtime are currently macOS/Linux-only; Windows requires registry-based native
messaging host registration before this transport can run there.

The native host manifest follows Chrome's Native Messaging lookup rules: the
default Google Chrome profile is automatic, while custom `--user-data-dir`,
Chrome for Testing, and Chromium profiles can be targeted with
`YOETZ_CHROME_NATIVE_MESSAGING_DIR`. Each loaded extension profile publishes a
separate local bridge instance under the Yoetz state directory. If one instance
is connected, the CLI uses it. If several are connected, recipe execution must
specify `profile_email` or the stable `extension_instance_id` published by
`status --chatgpt` so the CLI can route to the matching Chrome profile. It fails
closed if no selector matches; when Chrome does not expose a verifiable profile
email, `extension_instance_id` remains the deterministic selector. The local CLI
bridge sockets normally live under the Yoetz state directory, but fall back to
short per-state `/tmp` paths when macOS/Linux Unix socket path limits would
reject the bind.

#### V1 contract for chrome-extension-native

The extension transport ships a typed correctness contract that the recipe
runner relies on. A run never returns success unless every gate below holds.

- Capability token: every `job_*` envelope after `job_start` must carry the
  job's capability token. Mismatches fail with `capability_mismatch` and
  preserve `phase` + `side_effect_started` from the live job.
- Duplicate-job rejection: a `job_start` for a `job_id` that is already
  active or in the `terminalJobIds` tombstone TTL is rejected with
  `duplicate_job` (no side effect). Tombstones survive long enough to
  reject stale chunks and cancels for completed runs.
- Connection-generation fence: each connect to the native host increments a
  generation counter that is captured on the job. Async-resume sites verify
  the generation before any state mutation, so a job started under
  generation N cannot post `job_complete` after a `state_lost` was emitted
  on generation N+1.
- Conversation pinning: `sendPrompt` returns `conversation_id` and
  `submitted_user_count`. Subsequent extractions are gated against both —
  a tab navigation to another `/c/<id>` mid-run, or an extraction whose
  `preceding_user_count` precedes the submitted user turn, fails with
  `conversation_changed`. Late-pinning fills in `conversation_id` once
  ChatGPT redirects from `/` to `/c/<id>` after the first streamed token.
- Storage shape: `chrome.storage.session` is sharded as `jobs.<id>` keys
  with a one-time legacy `jobs` map migration on restart. The on-disk job
  shape strips `last_response_progress_text` to an 8KB tail; the in-memory
  job retains the full text for `response_delta` calculation. The TTL
  sweep runs on the heartbeat alarm tick, not per save.
- Completion gate: `extractResponse` rejects "thought/status chrome only"
  bodies (`Thought for ...`, `Reasoned for ...`, `Analyzing...`, etc.) and
  the SW refuses completion when extracted text is chrome-only, even if a
  copy button is visible. Stable-idle completion still requires the
  90-second `MIN_STABLE_IDLE_MS` floor; copy-button completion now also
  requires that floor so an early-arriving Copy cannot bypass safety.
- Send acceptance: when `clickSend` commits but `waitForSendAccepted`
  cannot confirm a post-click signal within budget, the run fails with
  `send_acceptance_unknown` carrying `side_effect_started: true`. The
  recipe runner treats this as a terminal Send-phase error and does not
  fall back to another transport, since ChatGPT may still process the
  prompt asynchronously. The error message tells the caller not to rerun
  blindly.
- Cancel: `cancelJob` clicks ChatGPT's stop control via the content
  script (best-effort), removes the owned tab, marks the job terminal,
  and adds it to `terminalJobIds`. Subsequent extracts for the cancelled
  `job_id` surface `unknown_job`.
- Identity: `identity.email` is an optional permission. Default routing
  uses `extension_instance_id` (per-Chrome-profile, persisted in
  `chrome.storage.local`). Pass `--var profile_email` only when you want
  a fail-closed verifier; the user must run
  `yoetz browser extension grant-identity --chatgpt` first.

The runner's contract under this transport: it never returns success on
partial or chrome-only ChatGPT output, never automatically retries via a
different transport once a side effect has landed in the user's tab, and
never silently re-submits a prompt that ChatGPT may already be processing.

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
    ├─ litellm-rust: send request to provider API
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

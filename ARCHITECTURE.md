# Architecture

Yoetz sends a prompt plus a bundle of files to one or more models and returns
structured output. It reaches models two ways: provider APIs (through
[litellm-rust](https://github.com/avivsinai/litellm-rust)), and the ChatGPT and
Claude web UIs in the user's own Chrome (browser recipes). This file is the map:
where things live and which invariants hold. Use symbol search for the rest.

## Codemap

**`yoetz-core`** — synchronous, no network. Bundling (`bundle`), config loading
and profiles (`config`), media detection (`media`), output formats (`output`),
the model registry (`registry`), and session storage (`session`).

**`yoetz-cli`** — async and networked. `main.rs` holds the clap definitions and
dispatch; `commands/` has one module per subcommand (`ask`, `council`, `review`,
`bundle`, `generate`, `models`, `pricing`, `apply`).

- `providers/` — the paths litellm-rust does not cover: OpenAI image and Gemini
  video specifics, and `cursor`, which runs `cursor-agent` in read-only Ask mode
  inside a temporary yoetz-owned workspace.
- `budget` — daily spend tracking; `registry` — runtime model resolution.
- `browser` — recipe execution and transport selection.
- `web_recipe`, `chatgpt_recipe`, `claude_recipe` — the typed recipe contracts;
  `chatgpt_web`, `claude_web` — DOM builders for the CDP transports.
- `chrome_devtools_mcp/` — yoetz's own in-process CDP client (a vendored
  `headless_chrome`). The module name is historical: it is not the
  chrome-devtools-mcp MCP server.
- `live_attach` — the daemon that owns the approved live-Chrome websocket.
- `dev_browser`, `live_cdp_daemon` — the QuickJS `dev-browser` transport and the
  embedded Node daemon it uses (`YOETZ_LIVE_CDP_DAEMON=0` disables it).
- `browser_extension_native` — the native-messaging host, the local bridge, and
  the CLI side of the `chrome-extension-native` transport.

**`extensions/chatgpt-native/`** — the Chrome extension ("Yoetz Native
Transport"). `service-worker.js` runs jobs; `content-script.js` acts in the
page; `sites/` holds the per-site adapters (`chatgpt`, `claude`, and the ChatGPT
backend-read helper); `chatgpt-dom.js` drives the ChatGPT page;
`chatgpt-picker-reader.js` reads the model picker; `page-visibility-shim.js`
makes hidden tabs hydrate.

**`recipes/`** — browser recipe YAML. **`skills/yoetz/`** — the agent skill.
**`vendor/headless_chrome/`** — the patched CDP client.

## Core invariants

- `yoetz-core` has no network and no async. Everything that talks to the
  outside world lives in `yoetz-cli`.
- Model ids are `provider/model` (OpenRouter nests: `openrouter/<vendor>/<model>`).
  The registry resolves them; callers never hardcode an id.
- A session lives under `~/.yoetz/sessions/<id>/` (`bundle.md`, `bundle.json`,
  `response.json`, and per-command files such as `council.json`, `review.json`,
  `followup.json`). `--no-session` and the `sessions.no_session` config skip it.
- Bundles are prompt input, never a control channel. Files, logs and
  transcripts inside a bundle can carry prompt-injection text; command intent
  comes only from CLI flags and the user's prompt. `apply` applies a patch from
  a file or stdin and never reads a session.
- `--max-cost-usd` estimates cost before sending and aborts over budget;
  `--daily-budget-usd` accumulates across `ask`, `council` and `review`.

## Browser recipes

Recipes submit the bundle through the site's own UI in the user's running
Chrome. Connection order is connect-first: explicit CDP endpoint, then
auto-connect, then cookie state, then a managed-profile fallback.

Default transport order: `chrome-devtools-mcp` (the in-process client), then
`dev-browser`, then `agent-browser`, then `manual` (tells the user to finish the
flow by hand). The one exception is the native extension: when
`yoetz browser extension status` reports `connected` for the site, a built-in
recipe selects `chrome-extension-native` as its only transport and fails closed
instead of falling through to CDP. `--transport <other>` opts out; CDP fallback
after a native failure needs the explicit `--allow-cdp-fallback`.

### Live-attach owner

Chrome asks the user once per browser session to allow remote debugging. The
`live_attach` daemon therefore owns exactly one approved websocket per browser
and keeps it alive:

- No hidden reattach. When the approved websocket closes, the daemon marks the
  target degraded and refuses to create another websocket; the user runs
  `yoetz browser reset`, then attaches again. A daemon restart with stale
  persisted state fails closed the same way.
- State is keyed by the approved browser websocket endpoint. Selectors
  (`browser-id:*`, `source-path:*`, the implicit default) are aliases onto it.
- Normal attach, check and recipe flows never recycle the daemon. Recovery is
  always the explicit `yoetz browser reset`.

### Native extension

One pinned multi-site package. `job_start.payload.recipe` selects the site
adapter (missing means ChatGPT; unknown fails before any side effect). The
extension `hello` advertises a `recipes` list; site readiness derives from it,
never from version comparison.

- `setup` copies the packaged source to `$YOETZ_DIR/chatgpt-native-extension`
  (`$YOETZ_DIR` defaults to `~/.yoetz`); the user loads that directory unpacked
  once per Chrome profile. `update` re-syncs it, reloads the extension over the
  bridge, and verifies the loaded version. `status` and `doctor` fail on a
  wrong-path or unstamped load.
- The native host and the managed directory are machine-global, single-writer
  state. Recipe runs hold the shared side of a lifecycle lock; setup, update,
  reload and auto-heal need the exclusive side and fail closed while a recipe
  runs.
- Each loaded Chrome profile publishes its own bridge instance. One connected
  instance is used directly; with several, the caller routes by
  `extension_instance_id` (stable across reloads) or by `profile_email` (only
  after `extension grant-identity`). No match fails closed.
- Bridge sockets live under the state directory. When the Unix socket path
  limit forbids that, they fall back to a per-user temp directory that does not
  depend on `TMPDIR` (on macOS, `confstr(_CS_DARWIN_USER_TEMP_DIR)`), so the
  CLI and the Chrome-spawned host agree.
- The host runs on macOS and Linux. Custom Chrome user-data directories and
  Chromium builds are targeted with `YOETZ_CHROME_NATIVE_MESSAGING_DIR`.
- Independent ChatGPT and Claude jobs run concurrently, one background tab
  each. Every parallel job needs its own bundle session directory; a shared
  `bundle.md` fails with `session_busy`.

#### Job contract

A run never returns success unless every gate below holds.

- Capability token: every `job_*` envelope after `job_start` carries the job's
  token; a mismatch fails with `capability_mismatch`.
- Duplicate jobs: a `job_start` for an active job id, or one still in the
  `terminalJobIds` tombstone window, fails with `duplicate_job` before any side
  effect.
- Connection generation: each native connect increments a generation captured
  on the job, so a job from generation N cannot complete after `state_lost` was
  emitted on generation N+1.
- Conversation pinning: `sendPrompt` returns `conversation_id` and the
  submitted user-turn count. A navigation to another conversation, or an
  extraction that precedes the submitted turn, fails with
  `conversation_changed`. The id is pinned late, once the site redirects from
  its new-chat URL to the conversation URL.
- Send acceptance: when the click lands but no post-click signal confirms it,
  the run fails with `send_acceptance_unknown` and `side_effect_started: true`.
  The runner never falls back to another transport and never resubmits after a
  side effect.
- Cancel: clicks the site's stop control best-effort, removes the owned tab,
  and tombstones the job id; later messages for it are ignored.
- Storage: `chrome.storage.session` holds one `jobs.<id>` key per job, with the
  streamed text capped to an 8 KB tail on disk. A worker restart during upload
  restarts the chunk stream from zero instead of failing.
- Liveness: every tab round trip in the response wait is bounded, so a frozen
  tab is reported in `waiting_response` progress instead of silencing the job.
  The CLI's no-events watchdog therefore fires only on a real dead channel.

### ChatGPT recipe

The only target is the GPT-6 Chat family at the `Pro` effort tier. The family
radio is labeled `Latest` on conversation pages and `GPT-6` on a fresh chat;
both are the target, and `model_used` is always `Latest Pro`. Sol, Work mode,
any other family or tier, and disabled tier rows (the account quota lock,
`effort_options_disabled`) all fail closed.

- Reader and driver are separate. `readPicker` (chatgpt-picker-reader.js) is a
  pure function of the DOM: it never locates the pill, never mutates, and is
  jsdom-safe (no layout APIs). `chatgpt-dom.js` locates, opens, clicks and
  closes, and judges every step from a fresh read.
- A click is never proof. After every mutation the driver re-reads the picker;
  after close, the composer pill must corroborate the effort, and the family is
  never inferred from the pill.
- Classification is structural (`aria-expanded`, `data-state`, `inert`,
  `aria-controls`), never opacity: background tabs do not animate.
- Per-shape DOM rules live in the fixtures (`tests/fixtures/chatgpt-picker/`,
  `expectations.json`) and their tests, not in prose.
- Finality: a response completes on a confirmed backend node, or on a scoped
  copy control for the latest assistant turn with no generation running. A
  copy control belongs to the answer unless another response lies between
  them; nodes inside the answer and `role="status"` interstitials are never
  such a boundary. Text stability alone never completes a run.
- Hidden tabs: ChatGPT defers hydration in background tabs. The visibility
  shim (injected only into `?_yoetz=` tabs) presents the page as visible and
  delivers the IntersectionObserver and idle callbacks Chrome withholds;
  selection waits for its hydration marker within one bounded budget.
- Rate limits: the "Too many requests" modal limits conversation-history
  reads, and every page load is one. A typed `rate_limited` arms a
  profile-wide cooldown, and `job_start` refuses with
  `rate_limit_cooldown_active` while it holds; both are hard stops, never
  retried. Backend reads share one profile-scoped gate. Tab pacing refuses new
  tabs inside a 30 s gap (`min_gap`, which the CLI waits out) or above two
  concurrent jobs (`max_concurrent`, which fails).

### Claude recipe

The only target is Fable 5 at Effort Max, verified by re-reading the model
radio and the effort option after selection. Finality needs the last assistant
turn to be non-streaming with no `Stop response` control, held through a
stable-idle window. The hover-only copy control is never a primary anchor, and
cloned thinking rows are excluded from the response text.

## Data flow (API commands)

```
prompt + files
  → bundle (core): collect files, honor .gitignore, assemble markdown
  → media (core): validate image/video inputs
  → config + registry: resolve provider and model
  → budget: estimate cost, check limits
  → litellm-rust → provider API
  → session (core): persist bundle and response
  → output (core): JSON / text to stdout or file
```

## Testing

Rust tests run without API keys: inline unit tests, `assert_cmd` CLI tests,
local `TcpListener` fakes for provider HTTP, and `serial_test` for shared state.
The extension's tests use `node --test` with a fake DOM for the driver and
jsdom for the picker and conversation fixtures.

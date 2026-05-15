# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.5.9] - 2026-05-15
### Changed

- The `chatgpt` browser recipe now auto-promotes the `chrome-extension-native`
  transport when the Yoetz Chrome extension is installed and reports
  `connected`. Pass `--transport <other>` or pin `transports:` in the recipe
  to opt out; non-ChatGPT recipes and unhealthy extensions stay
  extension-free.


## [0.5.8] - 2026-05-15
### Fixed

- Built-in browser recipes such as `chatgpt` now resolve by name even when the
  caller's working directory contains a same-name file or directory.
- ChatGPT native-extension auto model selection now reports the current
  Pro/Extended Pro model as selected when that label appears only after opening
  the personal ChatGPT model menu.

### Changed

- ChatGPT native-extension wait progress now distinguishes total page copy
  buttons from the scoped final assistant copy button required for completion.
- ChatGPT native-extension response completion is now driven only by scoped
  assistant DOM structure and final controls, not response-content heuristics.
- ChatGPT browser recipes now default to a 90-minute response wait for
  Pro/Extended Pro runs and include the owned-tab inspect command on timeout.


## [0.5.7] - 2026-05-15
### Changed

- Clarified Yoetz skill guidance for ChatGPT native-extension completion: the
  extension waits for final assistant controls instead of accepting stable
  partial page text.

## [0.5.6] - 2026-05-15
### Fixed

- ChatGPT native-extension recipes now wait for final assistant response
  affordances before completing, reducing premature completion on transient page
  text while preserving valid short responses.

## [0.5.5] - 2026-05-15
### Added

- Added `yoetz browser extension setup --chatgpt` to streamline ChatGPT native
  extension host installation and open Chrome with the expected setup page.
- Added personal ChatGPT UI model-selection support for Pro and Extended Pro,
  alongside the existing enterprise ChatGPT selectors.

### Changed

- Reduced routine GitHub Actions runner spend by limiting ordinary PR/main
  builds to Ubuntu while keeping the full cross-platform matrix for release
  branches and manual CI runs.

### Fixed

- ChatGPT native-extension recipes now prefer Pro/Extended Pro by default and
  avoid falling back to Instant unless the user explicitly asks for it.
- ChatGPT native-extension response extraction no longer completes a job early
  on transient one-character streaming snapshots.

## [0.5.4] - 2026-05-14
### Added

- Added `yoetz browser check --transport chrome-extension-native` to verify the
  ChatGPT native extension bridge without exercising CDP or triggering Chrome's
  remote-debugging approval dialog.

### Changed

- Updated user and contributor documentation for ChatGPT native-extension
  setup, extension readiness checks, configuration paths, environment variables,
  release flow, browser transport architecture, and packaging metadata.

### Fixed

- Removed stale source-install, Scoop bucket, docs.rs, and root `prompt.md`
  documentation/artifact drift from the repository.


## [0.5.3] - 2026-05-14
### Fixed

- ChatGPT browser recipes now default the composer prompt to the prompt stored
  in a Yoetz `bundle.md` session, while preserving explicit `--var prompt=...`
  overrides, so native-extension runs are governed by the user's task instead
  of the recipe's generic fallback text.
- ChatGPT `chrome-extension-native` response extraction now keeps copy-button
  completion evidence scoped to the current assistant response, avoids detached
  transcript copy-count shortcuts, and can return valid short model responses
  after normal stable-idle completion.
- ChatGPT `chrome-extension-native` now treats manual-handoff states detected
  from page metadata or no-transcript fallback pages during response wait as
  terminal instead of polling until timeout, without classifying user prompt or
  model response text.

### Changed

- Documented the unattended ChatGPT Pro native-extension review loop in the
  Yoetz skill, including durable JSON outputs, intentional re-review after
  patches, and the `stable_idle` completion reason.


## [0.5.2] - 2026-05-13
### Fixed

- ChatGPT `chrome-extension-native` now fails fast with
  `response_extraction_failed` when ChatGPT shows final assistant affordances
  but Yoetz cannot extract scoped assistant text, instead of burning the full
  wait timeout and tempting duplicate reruns.
- ChatGPT `chrome-extension-native` completion now handles responses whose text
  first appears while ChatGPT is still generating and then becomes idle without
  further text growth.

### Changed

- ChatGPT `chrome-extension-native` now emits low-noise lifecycle and
  `waiting_response` progress to stderr, including in `--format json` mode,
  while keeping stdout parseable for agents.
- Documented that realistic unattended ChatGPT Pro review jobs can run for
  15-20 minutes, should keep the original native-extension run attached, and
  should inspect terminal failed runs before any duplicate rerun.


## [0.5.1] - 2026-05-11
### Fixed

- ChatGPT `chrome-extension-native` completion no longer rejects valid
  post-send answers when ChatGPT cannot report `preceding_user_count`, and
  final copy-button affordance completion now uses a shorter stable window.
- `yoetz browser extension inspect --chatgpt --run-id <id>` can inspect a
  completed ChatGPT conversation by conversation id when the Yoetz run marker is
  no longer available.
- `terminal_delivery_lost` jobs remain terminal across service-worker restores
  instead of being overwritten as `state_lost`.
- Dead-PID ChatGPT native-extension instance records are ignored for routing
  and can be pruned via `yoetz browser reset`.
- Successful `chrome-extension-native` recipe runs now write `response.json`
  beside Yoetz `bundle.md` / `bundle.json` session artifacts.


## [0.5.0] - 2026-05-10
### Added

- ChatGPT `chrome-extension-native` transport reaches V1: opt-in extension +
  Native Messaging bridge for ChatGPT Pro that bypasses CDP approval prompts
  and avoids touching the default extension-free browser stack. Lifecycle:
  `yoetz browser extension install-host|status|doctor|reconnect|reload|canary
  |inspect|grant-identity --chatgpt`, then
  `yoetz browser recipe --recipe chatgpt --transport chrome-extension-native
  --bundle <file>` with optional `--var profile_email=...` or
  `--var extension_instance_id=...` selectors.
- Sharded per-job storage (`jobs.<id>` keys in `chrome.storage.session`) with
  legacy `jobs` map migration, plus 8KB on-disk tail cap for streaming
  response text. Long Pro responses no longer threaten the 10MB session
  quota; the in-memory job retains the full text for `response_delta` calc.
- Cancel side effects: `cancelJob` now clicks ChatGPT's stop control via the
  content script, removes the owned tab, and evicts the job from the
  in-memory map plus a TTL tombstone so a stale `job_start` cannot
  resurrect it.
- `inspect_run` envelope: read-only diagnostic that enumerates Yoetz-owned
  ChatGPT tabs, returns extraction + privacy-scoped diagnostics. Used by the CLI as
  `yoetz browser extension inspect --chatgpt --run-id <id>` to debug failed
  runs without restarting them.
- `request_identity_permission` envelope and matching
  `yoetz browser extension grant-identity --chatgpt` so `identity.email` can
  be granted on-demand instead of being mandatory at install. Doctor now
  surfaces whether the optional permission is granted.

### Changed

- `identity.email` moved from required `permissions` to
  `optional_permissions` in the extension manifest. Extension installs no
  longer prompt for "Read your email address" by default; routing relies on
  the per-Chrome-profile `extension_instance_id` published in
  `chrome.storage.local`. Pass `profile_email` only when you want a
  fail-closed verifier and have run `grant-identity` first.
- Job correlation hardened end-to-end: capability-token gate on every
  follow-on envelope, duplicate `job_start` rejection (with
  `terminalJobIds` tombstones), `connection_generation` fence so
  pre-disconnect async work cannot post `job_complete` after a `state_lost`
  has been emitted, and `conversation_id` + `submitted_user_count` pinning
  so a tab navigation mid-run cannot satisfy completion with an unrelated
  assistant turn.
- Completion safety: copy-button completion now requires the production 90s
  `MIN_STABLE_IDLE_MS` floor in addition to the affordance and stable text.
  `extractResponse` strips ChatGPT thought/status chrome ("Thought for...",
  "Reasoned for...", "Analyzing...", "Show reasoning", etc.) so a thinking
  marker by itself can never satisfy a successful response, even with a
  copy-button visible.
- `send_acceptance_unknown` is a terminal Send-phase error with
  `side_effect_started: true`: when ChatGPT accepts the click but Yoetz
  cannot confirm the prompt was acknowledged, the recipe runner refuses
  fallback to another transport so the user is never silently re-submitted.
- `chrome-extension-native` socket falls back to a short
  `/tmp/yoetz-cen-<hash>/<hash>.sock` path when the state-directory socket
  would exceed the Unix `sun_path` length limit. The native host refuses to
  steal an active socket, removes only stale ones, and tears its own socket
  file down on normal exit.

### Fixed

- Upload completion no longer false-positives on composer-scoped span/div
  nodes that merely contain the bundle filename. The pre-upload baseline
  now captures the same broad selector set as the per-tick check, so
  pre-existing filename-bearing nodes are excluded from completion
  candidates.
- Extension `service-worker.js` lifecycle events (`onInstalled`,
  `onStartup`) no longer overwrite a successful top-level
  `connectNative()` status with a stale "disconnected" state.
- `inspect_run` no longer leaks unrelated tab url/title for tabs that
  rejected the inspection with `run_mismatch`. URL and title are now
  suppressed for those entries; the `code` field is surfaced for tooling.
  Broad page text tails and diagnostic body text tails are omitted by
  default, so read-only inspection does not expose unrelated ChatGPT sidebar
  or transcript content.

### Known caveats

- ChatGPT Pro's file analyzer can stall or return truncated answers on
  large real-review attachments. Live testing observed failures around 60k
  and 220k effective tokens, while tiny sentinel canaries completed, so this
  is not documented as a stable token ceiling. Use focused per-directory
  slices for autonomous review jobs and raise `wait_timeout_ms` for expected
  long file-analysis runs; Yoetz fails terminally rather than returning
  partial answers.
- The Extended thinking toggle selector is a moving target on ChatGPT Pro.
  `--var extended=false` currently emits an "Extended toggle was not
  found" warning when the chip cannot be located but does not block the
  run; the user can flip the toggle manually if it matters for the
  request.


## [0.4.0] - 2026-05-07
### Changed

- Hardened the ChatGPT Pro browser recipe across live transports: each run now
  opens and refocuses a run-marked ChatGPT tab, reports
  `model_selection_status` separately from `model_used`, and avoids automatic
  fallback after upload/send/wait phases that may already have side effects.

### Fixed

- Replaced the macOS clipboard upload path with composer-scoped file input
  attachment handling, including stable attachment readiness checks and
  recipe-controlled `upload_timeout_ms` for both dev-browser and
  chrome-devtools-mcp transports.
- Added run-id/manual recovery details to terminal ChatGPT transport errors so
  interrupted runs can be continued from the yoetz-owned tab without blindly
  submitting a duplicate request.
- Tightened ChatGPT model selection verification to require checked/current
  menu state instead of accepting selector-label text alone, scoped response
  polling indicators to the active assistant turn, and scales upload timeouts
  with bundle size for large attachments.
- Fixed the live ChatGPT browser canary on macOS bash 3.2 and made explicit
  ChatGPT model selection wait briefly for the selector button on fresh tabs.

### Security

- Fixed a critical same-machine live-CDP daemon exposure by requiring a private
  runtime directory, same-user peer credentials, and token-protected RPCs before
  accepting browser JavaScript execution requests.
- Prefer `XDG_RUNTIME_DIR` for the live-CDP daemon on Unix and automatically
  tighten existing owner-matched `~/.yoetz` directories to `0700` during
  upgrade so the hardened socket and token checks do not reject normal users.


## [0.3.0] - 2026-05-03

## [0.2.59] - 2026-05-03

## [0.2.58] - 2026-05-02

## [0.2.57] - 2026-05-02

## [0.2.56] - 2026-04-24
### Added

- Yoetz-owned live-CDP Node daemon bundled into the yoetz binary as
  `~/.yoetz/live-cdp-daemon.mjs`, communicating with the Rust side via
  JSONL over a Unix socket / named pipe. Used automatically for
  dev-browser script execution against a live Chrome attach (Chrome 147
  default profile + `chrome://inspect` remote debugging). Avoids the
  Playwright `Target.setAutoAttach` hang that the upstream dev-browser
  daemon still trips on Chrome 147. External dev-browser remains the
  managed/profile path. New `YOETZ_LIVE_CDP_DAEMON=0` env var disables
  the bundled daemon for forced fallback.
- Embedded-bundle SHA256 version handshake between the Rust side and the
  running daemon — on version mismatch `ensure_daemon` auto-stops and
  respawns the daemon so upgraded yoetz binaries never drive a stale
  daemon build. Spawn is serialized behind an advisory lockfile at
  `~/.yoetz/live-cdp-daemon.lock`, so concurrent yoetz invocations do
  not race to unlink each other's socket.
- `yoetz browser doctor --live` now surfaces the yoetz live-CDP daemon
  as its own entry alongside upstream dev-browser and agent-browser,
  and `yoetz browser reset` cleans up both daemons.
- `scripts/build-live-cdp-daemon.sh` plus pinned
  `crates/yoetz-cli/assets/live-cdp-daemon-src/{package.json,package-lock.json,build.mjs}`
  reproduce the committed daemon bundle from its TypeScript sources. CI
  runs `./scripts/build-live-cdp-daemon.sh --check` so the committed
  `.mjs` cannot drift from the sources.

### Fixed

- Widened the error classifiers used by the Chrome 147 transport
  waterfall so the bundled live-CDP daemon's error strings land in the
  right code paths. `is_dev_browser_connect_failure` now recognizes
  `Target.getTargets`, `initializing live CDP browser`,
  `initializing live CDP targets`, and `remote-debugging consent`
  alongside the existing `connectOverCDP` / `Target.setAutoAttach` /
  `auto-connect` tokens.
- `is_chrome_approval_wait_error` and `allow_dialog_error` now also
  match `remote-debugging consent` and `remote debugging consent` so
  approval-pending errors from the bundled daemon still route to the
  "click Allow" fallthrough instead of a generic transport failure.
  `allow_dialog_error` now reads the full anyhow chain and matches
  case-insensitively so approval errors nested under context wrappers
  or emitted with mixed case are still detected.
- `should_retry_dev_browser_connect_failure` no longer retries
  `remote-debugging consent` timeouts — retrying a human-consent wait
  was re-triggering the Chrome "Allow remote debugging?" popup while
  the original dialog was still open.


## [0.2.55] - 2026-04-24
### Fixed

- Running-profile ChatGPT recipe and `yoetz browser check` no longer trigger
  multiple Chrome "Allow remote debugging?" approval popups on Chrome 147
  default profile. `chrome-devtools-mcp` is now preferred first when
  `prefer_auto_connect=true` (it is the only transport that works against a
  logged-in Chrome 147 default profile; Playwright-based `dev-browser` hangs
  on root `Target.setAutoAttach` and `agent-browser` inherits the same gating
  today). `dev-browser` remains available as a fallback for Chrome ≤146 and
  Chrome for Testing. Classify `Target.setAutoAttach` timeouts as connect
  failures that are *not* retryable so we stop firing a second
  `connectOverCDP` and triggering a second approval popup.
- `yoetz browser check` now surfaces a structured
  `browser_check_exhausted_error` when every attempted transport fails,
  replacing an `unreachable!` panic that could fire after the running-profile
  transport set was narrowed.
- `live_attach_owner_present` no longer counts a Healthy daemon with zero
  sessions as an owner — only `Busy`, or `Healthy + session_count > 0`. This
  stops the implicit `browser attach` / recipe fallbacks from assuming an
  owner is holding a Chrome approval when no live session exists.
- `BrowserCommand::Attach`'s implicit `ensure_chatgpt_session(None, ...)` is
  now gated behind `!prefer_auto_connect`, so it only runs when there is an
  existing live-attach owner to refresh, not as the first raw-attach
  fallback.
- Remove the agent-browser recipe path's `try_cdp_attach_lite` /
  `try_auto_connect_lite` probe subprocesses, approval locks, and per-recipe
  approval prints. Approval messaging now flows through the daemon layer so
  duplicate prompts do not stack.
- Narrow the dev-browser ChatGPT auth probe to the exact named page instead
  of searching all pages for `chatgpt.com`, preventing reuse of a user-owned
  tab and the associated transport fanout.


## [0.2.54] - 2026-04-20
### Added

- `yoetz browser verify-cdp --cdp <url>` — a thin CI-friendly smoke command
  that attaches to a given CDP endpoint and opens a throwaway tab, without
  any ChatGPT auth probing. Backs the new real-browser CI lane that spins up
  Chrome for Testing on every pull request.
- Deterministic fake-ChatGPT fixture tests plus a gated
  `scripts/chatgpt-browser-canary.sh` live canary so browser changes can be
  exercised against both a scripted DOM and a real authenticated ChatGPT tab.

### Changed

- ChatGPT browser transports now share one typed request/output contract.
  `chrome-devtools-mcp`, `dev-browser`, and the generic browser recipe all
  return the same top-level JSON fields (`transport`, `backend`, `response`,
  `model_used`, `warnings`, `fallback_used`, `delivery_mode`, and
  `auto_paste_fallback`) instead of transport-specific shapes.
- The generic `recipes/chatgpt.yaml` flow now delegates model selection,
  attachment UI open, and send-button activation to shared Rust actions so the
  transport-specific implementations stay aligned with the same DOM contract.

### Removed

- `--var thread=reuse` on `yoetz browser recipe --recipe chatgpt`. The reuse
  mode introduced in 0.2.50 and updated in 0.2.53 had no safe path that
  avoided hijacking an active ChatGPT Pro run. Every yoetz request now opens
  a fresh, yoetz-owned ChatGPT tab marked with `?_yoetz=<run-id>`; passing
  `thread=reuse` is rejected with a migration message pointing at this
  behavior. Parallel yoetz runs continue to work because each run owns its
  own tab.

### Fixed

- The vendored `headless_chrome` transport now uses flat CDP sessions and lazy
  single-target attachment on modern Chrome, avoiding the `Page.enable`
  `-32601` failures that showed up when a second client auto-attached existing
  targets on Chrome 147.
- `yoetz browser recipe --recipe chatgpt` now reconnects once and reattaches to
  the same ChatGPT page target if Chrome drops the websocket during long
  stable-idle response polling, instead of failing the run immediately.
- `yoetz browser recipe --recipe chatgpt` now prefers ChatGPT's visible tier
  labels (`Pro`, `Thinking`, `Instant`) over stale `gpt-5-3-*` testid
  suffixes when picking a model, and reopens the selector menu to verify the
  radio state if the button label stays generic `ChatGPT`. Fixes `model=pro`
  and `model=auto` falling back to Instant on the updated ChatGPT UI.
- `scripts/setup-chatgpt-profile.sh` now prefers the repo-built yoetz binary,
  and the global config selector is `--config-profile`, avoiding collisions
  with browser subcommands that already use `--profile`.

### Security

- Untrusted repo-local config can no longer set global defaults or model
  aliases, and browser profile/CDP defaults remain restricted to trusted config
  paths only.


## [0.2.53] - 2026-04-14
### Fixed

- `yoetz browser recipe --recipe chatgpt` now honors `--var thread=reuse` in the primary `chrome-devtools-mcp` transport instead of always opening a fresh ChatGPT tab. Reuse now fails fast when the candidate tab is ambiguous or still streaming, instead of stealing an in-flight ChatGPT Pro run.
- `yoetz browser check` no longer opens extra ChatGPT probe tabs on repeated checks when a ChatGPT tab is already open. The auth probe reuses an existing ChatGPT tab read-only when possible and only falls back to a temporary background tab when nothing is open.
- Reuse-related failures now stop browser-transport fallback immediately, preventing a second CDP attach attempt and the extra Chrome “Allow remote debugging?” popup that used to appear after an initial reuse failure.
- `yoetz browser check` now uses the intended transport order again (`chrome-devtools-mcp` before `dev-browser`) and reports the chosen transport in its output.
- The ChatGPT recipe now auto-selects the strongest available ChatGPT model by default, preferring GPT-5/Pro when present, and surfaces `model_used` in JSON output.
- `yoetz bundle` now handles explicit ignored/untracked files reliably, rejects comma-separated `-f` values with a clear error, and allows prompt-only bundles without walking the repo.


## [0.2.52] - 2026-04-13
### Fixed

- Browser target auto-discovery now rejects unhealthy `DevToolsActivePort` endpoints, keeps env/config auto-selection best-effort, restores managed-profile fallback on live auth/challenge failures, and stops leaking page body text in ChatGPT probe errors.
- Remembered browser-target state is now best-effort and no longer able to turn a successful attach or recipe run into a post-success failure.

### Changed

- Release automation now re-verifies the merged release commit before tagging and publishes GitHub build provenance attestations for the shipped archives.


## [0.2.51] - 2026-04-13
### Fixed

- `yoetz browser recipe --recipe chatgpt` now actually attempts the `chrome-devtools-mcp` transport instead of silently skipping it when `chrome-devtools-mcp` and `npx` are both absent from `PATH`. The availability gate was a leftover from v0.2.48 when the external binary was required for DOM snapshots; v0.2.49 removed that dependency, but the gate stayed. Each transport attempt now logs `info: attempting <name> transport` so skipped tiers are no longer invisible.
- CDP-unreachable errors on the `chrome-devtools-mcp` transport now surface actionable guidance — enable `chrome://inspect/#remote-debugging`, pass `--cdp`, or use Chrome for Testing — instead of leaking the raw reqwest error. Chrome 136+ ignores `--remote-debugging-port` on the default profile, so this is the most common failure mode.
- CDP-unreachable failures at tier 1 now skip remaining pure live-CDP transports (chrome-devtools-mcp, dev-browser) instead of cascading into dev-browser's `Target.setAutoAttach` hang (upstream Playwright bug). `agent-browser` still runs — it transparently falls back from live-attach to a managed profile with stored cookies, so it works without CDP. `manual` remains the final fallback.

### Clarified

- The `chrome-devtools-mcp` transport is a **direct CDP client** using vendored `headless_chrome` (`crates/yoetz-cli/src/chrome_devtools_mcp/client.rs`). It does NOT bridge to the chrome-devtools-mcp MCP server, despite the name. yoetz CLI runs as its own subprocess and cannot proxy MCP calls through a parent agent. Documentation updated in `recipes/chatgpt.yaml`.


## [0.2.50] - 2026-04-13
### Fixed

- ChatGPT recipe no longer captures the streaming preamble ("I'm reviewing the bundle now...") as the final answer. The completion heuristic now requires either a copy button on the new assistant message (strong signal) or a real-time stable-idle window of `max(90s, 3 × wait_interval_ms)` with unchanged length. Removed the first-poll false-positive from the old `prev_dom=None => true` branch.

### Added

- `--var thread=fresh|reuse` on `yoetz browser recipe --recipe chatgpt` (default `fresh`, byte-identical to prior behavior). `thread=reuse` keeps follow-up turns in the currently active ChatGPT conversation instead of opening a fresh tab on every call. Fail-fasts when the attached tab is not on chatgpt.com.
- `completion_reason` (`copy_button` | `stable_idle_fallback`), `elapsed_ms`, `stable_for_ms`, and `stable_idle_threshold_ms` in the ChatGPT wait response JSON payload for observability.


## [0.2.49] - 2026-04-12
### Added

- Chrome 147 live-attach via vendored `headless_chrome` — primary transport for the ChatGPT recipe that bypasses the Playwright/Puppeteer `Target.setAutoAttach` hang on Chrome 147's default-profile built-in remote debugging
- Three-tier browser transport waterfall: `headless_chrome` (primary) > `dev-browser` (second tier) > `agent-browser` (third tier), with each tier working end-to-end against Chrome 147
- `PW_CHROMIUM_ATTACH_TO_OTHER=1` injected into dev-browser child process for Chrome 147 Playwright compat (upstream issue filed: SawyerHood/dev-browser#103)
- Custom DOM snapshot engine in `chrome_devtools_mcp/client.rs` for uid-based element targeting without external MCP server dependency
- Stable-idle ChatGPT response polling ported from v0.2.33 Pro Extended heuristic
- Vendored `headless_chrome` 1.0.21 with pre-generated CDP bindings — zero GPL in the dependency tree

### Fixed

- Auth probe tab close now targets the probe tab explicitly instead of blind-closing the user's active tab
- Explicit `--cdp` is terminal on `browser attach`, `browser check`, and `browser login` — no silent fallback to auto-connect/cookies/profile
- ChatGPT completion poller scopes copy-button detection to the latest assistant message only
- Full error chain surfaced in recipe transport errors (PR #129)
- Agent-browser ChatGPT recipe uses real keyboard typing instead of ProseMirror-incompatible `fill()`


## [0.2.48] - 2026-04-11
### Fixed

- Keep the ChatGPT dev-browser page stable across recipe runs so Chrome does not need a fresh remote-debugging approval for each new session request; the shared ChatGPT tab now persists between runs and yoetz serializes access to it
- Stop re-probing explicit dev-browser CDP endpoints in yoetz; explicit `--cdp` values now pass through unchanged so dev-browser owns Chrome 146+ DevToolsActivePort fallback behavior
- Stop silently recycling legacy live-attach daemons during normal browser flows; stale recovery is now explicit via `yoetz browser reset`


## [0.2.47] - 2026-04-05
### Fixed

- Reuse stable CDP browser name across ChatGPT recipe invocations to avoid repeated Chrome "Allow remote debugging?" dialogs


## [0.2.46] - 2026-04-04
### Changed

- **Security**: Pin all GitHub Actions to commit SHAs across ci.yml, release.yml, publish-skill.yml
- Remove broken `push: tags: v*` trigger from standalone publish-skill.yml (GITHUB_TOKEN tag-push bug); keep only `workflow_dispatch` with `tag` input
- Add `skip-downstream` dispatch input to release.yml for selective rerun of release without re-triggering publish-skills, update-homebrew, update-scoop
- Scope permissions per job in release.yml (read for prepare/build, write for release)
- Add concurrency and timeout-minutes to notify-marketplace.yml
- Add missing timeout-minutes to check-skills and changes jobs in ci.yml


## [0.2.45] - 2026-04-03
### Added

- Micro-script ChatGPT recipe architecture for dev-browser (prepare/send/poll/cleanup)
- Clipboard-based file upload for ChatGPT on macOS via osascript
- Chrome approval dialog cascade prevention with cooperative file lock
- Explicit `--browser` slot per recipe run to isolate daemon state
- Council now preserves partial results when individual models fail

### Fixed

- **Security**: Gemini upload/download URLs now validated against trusted host
- **Security**: Untrusted repo-local config blocked from setting browser_cdp/browser_profile
- **Security**: YOETZ_CONFIG_PATH, YOETZ_REGISTRY_PATH, YOETZ_BROWSER_CDP protected from .env override
- **Security**: Stale-daemon PID kill now verifies process identity before SIGKILL
- Removed pre-flight auth check that blocked recipes on Chrome 144+ approval flow
- Send micro-script re-verifies composer readiness before typing

### Changed

- Replaced deprecated `serde_yaml` with `serde_yml`
- Pinned `litellm-rust` to specific Git revision instead of tracking `main` branch
- Dropped `ureq` dependency; sync CDP probe uses `reqwest` blocking client
- Release script now runs cargo test, clippy, and fmt check before committing
- Release script bumps all `skills/*/SKILL.md` frontmatter, not just yoetz

### Documentation

- Fixed stale WireMock test tooling claim in CLAUDE.md
- Updated command reference to match actual CLI surface
- Replaced hardcoded model IDs in skill examples with resolution placeholders
- Corrected dev-browser upload guidance to match clipboard paste implementation


## [0.2.44] - 2026-04-02
### Documentation

- Make CLAUDE the master agent guide

### Changed

- Switched releases to the shared PR-based `scripts/release.sh` flow, with `CHANGELOG.md` supplying the GitHub release notes and CI creating the version tag only after the merged release commit verifies.

### Fixed

- Removed deprecated release shims so there is exactly one supported release entrypoint.


## [0.2.43] - 2026-04-01

### Miscellaneous

- Release metadata-only cut; no additional user-facing changes

## [0.2.42] - 2026-04-01

### Miscellaneous

- Release metadata-only cut; no additional user-facing changes

## [0.2.41] - 2026-04-01

### CI/CD

- **release**: Sign macOS artifacts and align release prep

## [0.2.40] - 2026-04-01

### Bug Fixes

- Stage optional codex plugin manifest in release script
- Skip invalid skill aliases in publish workflow
- **ci**: Narrow release-only detection (#109)
- Include all version files in release script

### CI/CD

- Notify marketplace on default-branch pushes

### Features

- Make dev-browser the default web transport (#110)

### Miscellaneous

- Harden release versioning
- **deps**: Bump toml in the minor-and-patch group across 1 directory (#107)
- **deps**: Bump sha2 from 0.10.9 to 0.11.0 (#104)

## [0.2.38] - 2026-03-30

### Miscellaneous

- Add tag-based skill release flow
- Release skills v0.2.38

## [0.2.37] - 2026-03-29

### Bug Fixes

- Update Cargo.lock for v0.2.37

## [0.2.36] - 2026-03-29

### Bug Fixes

- **browser**: ChatGPT upload selector + dev-browser connection retry (#102)

## [0.2.35] - 2026-03-29

### Bug Fixes

- Harden ChatGPT dev-browser recipe (#100)

## [0.2.34] - 2026-03-28

### Bug Fixes

- Stabilize flaky socket test + auto-bump plugin.json in release (#97)
- **browser**: Dev-browser recipe overhaul (#98)

## [0.2.33] - 2026-03-27

### Bug Fixes

- **browser**: Auto-poll for ChatGPT Extended Pro + review fixes (#92)

### Features

- Add Codex interface metadata to plugin manifest

### Miscellaneous

- Bump plugin.json version to 0.2.33 (#95)

### Refactoring

- Eliminate skill duplication, add Codex plugin manifest

### Reconcile

- Add missing references/commands.md to canonical skills/yoetz

## [0.2.32] - 2026-03-26

### Documentation

- Update browser docs for CDP auto-connect and Chrome 146 (#90)

### Features

- **browser**: Dev-browser backend + review bug fixes (#91)

## [0.2.31] - 2026-03-24

### Bug Fixes

- **browser**: Bound all Chrome 146 live-attach paths + fix test flakiness (#88)

## [0.2.30] - 2026-03-24

### Bug Fixes

- **browser**: Chrome 146 CDP dialog handling + faster response polling (#86)

## [0.2.29] - 2026-03-23

### Features

- **browser**: Default to file attachment delivery in ChatGPT recipe (#84)

## [0.2.28] - 2026-03-23

### Bug Fixes

- **browser**: Remove npx fallback env-var gate (#82)

## [0.2.27] - 2026-03-23

### Features

- **browser**: ChatGPT size-based delivery + upload polling (#80)

## [0.2.26] - 2026-03-22

### Bug Fixes

- **browser**: ChatGPT model selector and response completion detection (#76)
- **ci**: Strip squash-merge PR suffix from release tag parsing (#78)

### CI/CD

- Merge auto-tag into release.yml, eliminate PAT requirement (#75)

## [0.2.24] - 2026-03-22

### CI/CD

- Fast release pipeline — auto-tag, CI fast path, release script (#72)

### Refactoring

- Simplify interpolation and fix CI fast path gaps (#73)

## [0.2.23] - 2026-03-22

### Bug Fixes

- **browser**: Chatgpt recipe ProseMirror fill bypass (#70)

## [0.2.22] - 2026-03-22

### Bug Fixes

- **browser**: Chatgpt recipe parse error, stale thread, model selector

## [0.2.21] - 2026-03-22

### Features

- **browser**: Prioritize auto-connect, add Chrome 136+ CDP warning (#65)
- Upload chatgpt bundles and poll for completion (#66)

## [0.2.20] - 2026-03-21

### Features

- **models**: Add models frontier — live-derived rankings (#63)

## [0.2.19] - 2026-03-19

### Bug Fixes

- **browser**: Fix ChatGPT Pro auto-connect integration e2e (#61)

### Miscellaneous

- **deps**: Bump the minor-and-patch group across 1 directory with 2 updates (#58)
- **deps**: Bump jsonschema from 0.44.1 to 0.45.0 (#51)
- **deps**: Bump actions/setup-node from 4 to 6 in the actions group (#49)

## [0.2.18] - 2026-03-18

### Bug Fixes

- **security**: Harden trust boundaries, budget accounting, and browser recipe (#59)

## [0.2.17] - 2026-03-18

### Bug Fixes

- **bundle**: Handle tilde and absolute paths in -f flag (#57)

## [0.2.16] - 2026-03-17

### Bug Fixes

- **recipe**: Add model selection, preserve Extended Pro, use send button (#55)

## [0.2.15] - 2026-03-16

### Bug Fixes

- **ci**: Regenerate CHANGELOG.md for v0.2.14
- Harden browser automation, security, and release engineering (v0.2.15) (#53)

## [0.2.14] - 2026-03-16

### Bug Fixes

- **ci**: Align v0.2.13 with format and changelog checks
- Live-attach blank tab and ChatGPT recipe selector

## [0.2.13] - 2026-03-15

### Bug Fixes

- Increase auth check timeout for live-attach browser connections

## [0.2.12] - 2026-03-15

### Bug Fixes

- **ci**: Regenerate CHANGELOG.md via git-cliff for v0.2.12

### Features

- Browser recipe auto_connect + ChatGPT upload fix

## [0.2.11] - 2026-03-14

### Bug Fixes

- Pause for manual captcha solve in browser flows (#46)

### Features

- CDP browser attach — replace cookie sync with direct Chrome session access (#47)

### Miscellaneous

- Update SKILL.md model references to current versions (#44)
- Use grok-4.20-multi-agent-beta in SKILL.md examples (#45)

## [0.2.10] - 2026-03-14

### Features

- Bundle browser cookie extractor and improve auth polling (#42)

## [0.2.9] - 2026-03-14

### Features

- Dynamic model registry with auto-sync and config aliases (#39)

## [0.2.8] - 2026-03-12

### Miscellaneous

- **deps**: Bump the minor-and-patch group across 1 directory with 3 updates (#37)
- **deps**: Bump litellm-rust from `178e728` to `241c57b` (#36)
- **deps**: Bump the actions group with 2 updates (#34)

## [0.2.7] - 2026-03-12

### Bug Fixes

- Harden browser cookie sync and recipe defaults

### CI/CD

- Decouple MSRV rust-toolchain action ref from Rust version

### Miscellaneous

- **deps**: Bump litellm-rust from `c6c7553` to `178e728`
- **deps**: Bump jsonschema from 0.41.0 to 0.42.1
- **deps**: Bump toml from 0.9.11+spec-1.1.0 to 1.0.3+spec-1.1.0
- **deps**: Bump the minor-and-patch group across 1 directory with 6 updates

## [0.2.6] - 2026-02-23

### Bug Fixes

- **browser**: Ship scripts/recipes in Homebrew and resolve by name (#30)

## [0.2.5] - 2026-02-19

### Features

- Fuzzy model resolution and discovery CLI (#23)

## [0.2.4] - 2026-02-14

### Bug Fixes

- Em-dash parsing and model discovery (#12, #13, #14) (#15)

## [0.2.3] - 2026-02-12

### Bug Fixes

- Revert MSRV toolchain to 1.88 (Dependabot misidentified Rust version as action version)
- Update rand 0.9 API (rng(), distr module, value semantics)
- Update jsonschema 0.41 API (JSONSchema → Validator, validate returns Result)

### Miscellaneous

- **deps**: Bump toml from 0.8.23 to 0.9.11+spec-1.1.0 (#7)
- **deps**: Bump thiserror from 1.0.69 to 2.0.18
- **deps**: Bump the actions group across 1 directory with 2 updates
- **deps**: Bump rand from 0.8.5 to 0.9.2
- **deps**: Bump jsonschema from 0.17.1 to 0.41.0
- Allow MIT-0 license (borrow-or-share dependency of jsonschema 0.41)
- OSS polish — topics, changelog, README, CI (#10)

## [0.2.2] - 2026-02-09

### Features

- Make max_output_tokens optional to use provider defaults (#9)

## [0.2.1] - 2026-02-09

### Bug Fixes

- Gemini Pro 3 model names, JSON stdout bloat, and token limits

### Features

- Add auto-bootstrap install section to skill definitions

## [0.2.0] - 2026-02-07

### Bug Fixes

- Use HOMEBREW_TAP_GITHUB_TOKEN secret name
- Use macos-14 for x86_64 build (macos-13 retired)

### Documentation

- Add Homebrew and Scoop installation to README

### Miscellaneous

- Update workflows to SOTA patterns

### Styling

- Fix rustfmt in CLI integration tests

## [0.1.0] - 2026-02-07

### Bug Fixes

- CI issues - update MSRV to 1.78, allow noisy lints
- CI issues - MSRV 1.85, update deny.toml format
- CI issues - update MSRV to 1.88, fix deny.toml format
- Add missing licenses to deny.toml allow list
- Update bytes to 1.11.1 (RUSTSEC-2026-0007)
- Normalize gemini model names and persist artifacts
- Enforce openrouter namespaced models
- Normalize openrouter model ids for registry
- Utf-8 truncation, bundle hashing, and defaults
- Release workflow — drop openssl dep, fix macOS runner

### Documentation

- Fix response-format flag

### Features

- Add multimodal ask and generation
- Add litellm-rs sdk crate
- Multimodal content support in litellm-rs and yoetz-cli
- Add anthropic streaming and tool params
- Gemini tool roles and mime inference
- Route yoetz CLI through litellm-rs
- Model validation and capability gating

### Miscellaneous

- Add SOTA project infrastructure
- Add workflow_dispatch to CI

### Testing

- Utf8 truncation and media url mime
- Bundle determinism and hash

### Core

- Add media types scaffold

### Hardening

- Gemini inline limit and config

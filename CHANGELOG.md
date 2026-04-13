# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

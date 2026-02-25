# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.2.6] - 2026-02-25

### Miscellaneous

- **deps**: Bump litellm-rust from `c6c7553` to `178e728`

## [0.2.6] - 2026-02-25

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


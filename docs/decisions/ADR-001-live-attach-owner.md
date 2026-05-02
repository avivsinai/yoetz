# ADR-001: Make `chrome-devtools-mcp` the Live Attach Owner

## Status
Accepted

## Date
2026-04-22

## Context
Yoetz had the right primitives for live Chrome work, but ownership was split:

- `yoetz browser attach` used the older live-attach path in `browser.rs`
- `yoetz browser check` partly used `chrome-devtools-mcp`
- `yoetz browser recipe --recipe chatgpt` still opened its own fresh live attachment
- ChatGPT auth probing still opened transient probe tabs instead of reusing one durable yoetz-owned tab
- approval prompts and attach attempts were spread across multiple call sites

That split increased reconnect churn and made the recurring Chrome "Allow remote debugging?" prompt worse than it needed to be.

## Decision
Make `chrome-devtools-mcp` the single yoetz-owned live-attach owner for ChatGPT-oriented live Chrome work.

The concrete changes are:

- add a daemon-backed owner in `crates/yoetz-cli/src/live_attach.rs`
- route `attach`, the primary `check` path, and the ChatGPT `recipe` path through that owner
- keep one durable yoetz-owned ChatGPT control tab per resolved browser context by persisting a stable `_yoetz=<run-id>` marker
- reconnect from the logical target identity (`source-path`, `browser-id`, or implicit default discovery), not by blindly reusing the last websocket URL
- preserve daemon metadata on bounded ping timeouts so a busy owner is not mistaken for a dead one, while `yoetz browser reset` still forcefully clears a wedged owner
- keep `dev-browser` and `agent-browser` as fallback transports instead of peer live-session owners
- treat Chrome's approval-only remote debugging mode as a browser-websocket-only
  mode: `/json/*` endpoints may be unavailable before approval, so
  `DevToolsActivePort` discovery must not require `/json/version` before the
  first attach; it may accept the endpoint only when the listening localhost
  port is owned by a local Chromium process
- open ChatGPT recipe tabs from the durable yoetz-owned control tab after the
  daemon is attached, rather than issuing a fresh browser-level
  `Target.createTarget` for every recipe run
- forbid automatic CDP reconnect inside daemon-owned ChatGPT flows after the
  first approved websocket exists. A closed websocket is terminal for the
  current request and requires an explicit `yoetz browser reset` before any new
  attach attempt.

## Alternatives Considered

### Keep three peer session owners
- Pros: no refactor
- Cons: repeated attach logic, repeated approval churn, transient probe tabs, unclear reset behavior
- Rejected: this is the direct cause of the current fragmentation

### Make `dev-browser` the live owner
- Pros: existing named-page reuse
- Cons: Playwright `connectOverCDP` remains the unstable transport on current Chrome default-profile flows
- Rejected: wrong transport foundation for the default live path

### Make `agent-browser` the live owner
- Pros: existing daemon/session mechanics
- Cons: the auto-connect path is intentionally not the canonical session owner for real-tab visibility, and managed/profile fallbacks are a different ownership model
- Rejected: useful fallback, wrong primary owner

### Build the full daemonized CDP supervisor now
- Pros: closest to the final design
- Cons: larger IPC and lifecycle change, higher regression risk, slower path to reducing actual churn
- Rejected initially: the first safe step was to prove the owner boundary before pushing every flow onto it

## Consequences

- `attach`, `check`, and the ChatGPT `recipe` share one yoetz-owned CDP owner
- repeated auth checks and recipe launches reuse the same control-tab identity for a resolved browser context instead of creating throwaway probe tabs
- daemonized ChatGPT requests fail closed on transport loss instead of creating
  a second CDP websocket that could trigger another Chrome approval dialog
- Chrome still requires the first native approval after Chrome starts. Yoetz
  cannot persist or bypass that browser consent; the invariant is that the
  daemon keeps one approved websocket alive so subsequent ChatGPT recipe runs
  do not create new attach prompts until Chrome or the daemon is restarted
- a busy daemon times out cleanly instead of wedging `doctor`, `reset`, or later attach/check calls forever
- `dev-browser` and `agent-browser` remain available, but only as fallback executors when the primary live owner is unavailable or unsuitable

## 2026-05-02 Amendment: No Hidden Reattach

The live-attach daemon is an owner, not a retry wrapper. Once Chrome has
approved the daemon's websocket, daemon-owned code must not create another CDP
websocket as hidden recovery for `ensure-session`, recipe page-open, or
stable-idle polling failures.

When the approved websocket closes, the daemon records the target as degraded,
blocks automatic reattach for that target, and returns an error telling the
operator to run `yoetz browser reset` before reattaching. Standalone
non-daemon recipe code may keep its explicitly scoped one-retry fallback, but
the daemon path always passes a no-reconnect policy to the recipe engine.

If a daemon restarts while persisted live-attach state still says a target was
previously attached, recipe requests also fail closed instead of creating a new
websocket from that stale state. A new owner may be established only through an
explicit `yoetz browser attach` flow or by clearing the stale state with
`yoetz browser reset`.

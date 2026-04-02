# Browser Transport Prototype

Minimal comparison between two candidate browser transports for Yoetz:

- `chromiumoxide` over the browser WebSocket read from `DevToolsActivePort`
- `chrome-devtools-mcp` spawned as an MCP subprocess

## Commands

Build:

```bash
cargo check
```

Chromiumoxide existing-tab probe:

```bash
cargo run -- chromiumoxide-navigate-existing-tab
```

Chrome DevTools MCP navigate probe:

```bash
cargo run -- mcp-navigate-page --mode auto-connect
cargo run -- mcp-navigate-page --mode ws-endpoint
```

## Local Findings On 2026-04-02

Chrome state at test time:

```text
$ cat ~/Library/Application\ Support/Google/Chrome/DevToolsActivePort
9222
/devtools/browser/535fdef2-8721-4de5-949f-b5bb3aa01959

$ curl -i http://127.0.0.1:9222/json/version
HTTP/1.1 404 Not Found
Content-Length:0
Content-Type:text/html
```

Observed transport behavior:

1. `chromiumoxide` currently times out on `Browser::connect` against the live `DevToolsActivePort` websocket.

```text
$ cargo run -- chromiumoxide-navigate-existing-tab
Error: chromiumoxide Browser::connect timed out
```

2. `chrome-devtools-mcp` starts, completes MCP initialization, and exposes tools, but the first real browser operations time out in both attach modes.

`--mode auto-connect`:

```json
{
  "list_pages": {
    "status": "error",
    "error": "tool `list_pages` timed out after 10s: deadline has elapsed"
  },
  "navigate_page": {
    "status": "error",
    "error": "tool `navigate_page` timed out after 25s: deadline has elapsed"
  }
}
```

`--mode ws-endpoint`:

```json
{
  "list_pages": {
    "status": "error",
    "error": "tool `list_pages` timed out after 10s: deadline has elapsed"
  },
  "navigate_page": {
    "status": "error",
    "error": "tool `navigate_page` timed out after 25s: deadline has elapsed"
  }
}
```

Relevant MCP server logs:

```text
2026-04-02T14:08:41.744Z mcp:log list_pages request: {}
2026-04-02T14:08:41.744Z mcp:log Connecting Puppeteer to  {"defaultViewport":null,"handleDevToolsAsPage":true,"channel":"chrome"}

2026-04-02T14:08:41.650Z mcp:log list_pages request: {}
2026-04-02T14:08:41.650Z mcp:log Connecting Puppeteer to  {"defaultViewport":null,"handleDevToolsAsPage":true,"browserWSEndpoint":"ws://127.0.0.1:9222/devtools/browser/535fdef2-8721-4de5-949f-b5bb3aa01959"}
```

## Revised Assessment

The initial runs above are real, but they were all fresh-connection probes. After checking the transport implementations, the more important conclusion is architectural:

- Chrome approval on modern local CDP appears to be per new browser connection, not a one-time persistent grant.
- `dev-browser` succeeds because its daemon keeps one approved WebSocket connection alive and reuses it for later actions.
- The prototype commands here spawn a fresh process and create a fresh browser connection each run, so they repeatedly hit the approval boundary.

What the source says:

- `chromiumoxide` can be used as a persistent daemon. `Browser::connect()` returns a `Browser` plus a `Handler`; if a daemon keeps both alive, it can reuse that same connection for multiple later actions instead of reconnecting each time.
- `chrome-devtools-mcp` can also be used persistently. In the published `chrome-devtools-mcp@0.21.0` package, `build/src/browser.js` keeps a module-level `browser` and `ensureBrowserConnected()` returns the existing connected browser when the same server process is still alive.

So the current transport question is narrower than it first looked:

- the Chrome 136+ approval problem is primarily about daemon architecture and connection lifetime
- it is not yet evidence that one of `chromiumoxide` or `chrome-devtools-mcp` is fundamentally incapable

Current practical assessment:

- `chrome-devtools-mcp` already has the shape of a persistent daemon/server; if we keep one MCP server process alive across requests, it should be capable of reusing a single approved Puppeteer connection
- `chromiumoxide` could support the same strategy, but Yoetz would need to build and own that daemon layer directly around a long-lived `Browser` + handler task

The next useful experiment is no longer “fresh process vs fresh process”. It is:

1. keep one `chrome-devtools-mcp` server alive and test whether a single approval unlocks multiple subsequent tool calls without reconnecting
2. build the equivalent long-lived `chromiumoxide` daemon probe and compare behavior under the same conditions

That will separate transport quality from the current per-connection approval behavior.

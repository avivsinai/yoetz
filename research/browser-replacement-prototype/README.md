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

## Assessment

These runs do not yet justify choosing one transport over the other, because the live Chrome endpoint itself appears unhealthy:

- `DevToolsActivePort` exists
- the legacy HTTP probe returns `404`
- `chromiumoxide` times out on direct WebSocket attach
- `chrome-devtools-mcp` reaches MCP/tool discovery, but browser actions stall once it tries to attach Puppeteer

The next useful experiment is to run the same prototype against a known-good Chrome instance, ideally Chrome for Testing or a fresh Chrome profile launched with an explicit remote-debugging port and user-data-dir. That would separate transport quality from the current live-Chrome half-state.

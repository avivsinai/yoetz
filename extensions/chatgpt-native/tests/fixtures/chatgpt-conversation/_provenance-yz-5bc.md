# Provenance

Captured 2026-09-14T09:31:54Z via read-only AppleScript `execute javascript`
on the personal Chrome profile, conversation
`6aa7a061-5fd8-83eb-8dd0-a8a19d763437`, extension 0.5.72.

Run: 20260914T072024Z_1846dc — after ~2 h of Pro extended thinking, the
agent turn became "This content may violate our usage policies. | Called tool"
(56 chars). Zero `[data-message-author-role="assistant"]` nodes, no streaming
marker, no Stop control, composer submit button absent, no markdown.

This is a terminal server-side outcome: ChatGPT will not produce an answer
in this turn. The fixture captures the last agent-turn outerHTML.

## Files

- `2026-09-14-usage-policy-flagged-agent-turn.html` — the flagged agent-turn.
  Contains a `text-token-text-error` div with "This content may violate our
  usage policies." and a tool-message span "Called tool". No assistant role
  marker, no streaming marker, no stop control.

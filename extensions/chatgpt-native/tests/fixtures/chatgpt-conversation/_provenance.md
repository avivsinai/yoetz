# Provenance

Captured 2026-09-14T09:09:03Z via read-only AppleScript `execute javascript`
on the personal Chrome profile (ext_937d13fd52980082ed530899), conversation
`6aa7a061-5fd8-83eb-8dd0-a8a19d763437`, extension 0.5.72.

Run: 20260914T072024Z_1846dc — ChatGPT Pro long think. The job failed at the
90 min deadline with `is_generating: false` while ChatGPT was still generating
(the Stop button was present but `isResponseGenerating` did not match it).

## Files

- `2026-09-14-stop-button.html` — the Stop button (`data-testid="stop-button"`,
  `aria-label="Stop answering"`). `isResponseGenerating` matched only
  `button[aria-label*="Stop generating" i]`, missing this button.
- `2026-09-14-pro-long-think-agent-turn.html` — the agent-turn containing a
  `[data-streaming-response-status]` interstitial ("Our systems are thinking a
  bit more about this request before responding..."). A positive "still
  streaming" signal independent of the composer button.
- `2026-09-14-user-action-bar.html` — the user turn's action bar carrying the
  only copy button on the page. The `stable_idle_unscoped_copy_button`
  recovery leg must NOT complete on this DOM because the copy control belongs
  to the user turn, not an assistant answer.

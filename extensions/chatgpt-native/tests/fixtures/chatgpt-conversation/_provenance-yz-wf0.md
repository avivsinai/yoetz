# Provenance (yz-wf0)

`2026-10-07-turn-key-finished-answer.html`: live capture, 2026-10-07, read-only
AppleScript `execute javascript` on the personal Chrome profile
(ext_937d13fd52980082ed530899), extension 0.5.80.1. The capture serialized
`<main>` with the shared `capture-sanitizer.js` + `conversation-serializer.js`.
Run 20261007T130518Z_0f17f6 timed out after 10 min "waiting for final
assistant controls" while the answer was finished and visible.

The single `data-turn-key` holds the user unit and the assistant unit. The
answer's Copy button sits in an action bar outside the `*:assistant` search
unit, and the answer contains inline code rendered as
`span.InlineMarkdownIsolate-*`.

`2026-10-08-status-interstitial-finished-answer.html`: live capture, 2026-10-08,
same method, extension 0.5.84.1, run 20261008T084708Z_f87895 (a Pro review run
by the AMQ lead). The answer was finished (Copy button, no Stop) while the job
kept "waiting for final assistant controls". The long-think interstitial "Our
systems are thinking a bit more..." stays mounted as a MarkdownRoot inside a
`role="status"` live region between the answer and its Copy button. UUIDs are
replaced with placeholders.

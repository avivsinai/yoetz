---
name: test-audit
description: "Invoke whenever writing, changing, reviewing, or sweeping yoetz tests (Rust or extensions/chatgpt-native JS). Authoring gate for new/changed tests plus audit workflow for low-value, implementation-coupled, duplicative, or slow tests and the test-only production seams they demand."
---

# Test Audit (yoetz)

Three modes, one value bar. Based on openclaw's `test-audit` skill, retargeted to
this repo's boundaries, commands, and speed discipline.

- **Authoring** — gate every new or changed test at write time.
- **Audit** — focused sweeps of existing tests that re-assert source, duplicate
  stronger proof, couple to implementation, keep test-only seams alive, or burn
  wall-clock.
- **Campaign** — prune one whole subsystem's test surface at a time. For yoetz a
  subsystem is one of: `yoetz-core`, `yoetz-cli`, or one extension area
  (`picker`, `service-worker`, `protocol/chunks`, `sites`).

Continue audits as separate coherent PRs; optimize for confidence, not deletion
count. Never weaken an assertion or delete a test to make a run pass.

## Where the value lives (yoetz map)

Each contract has ONE primary test owner at the strongest boundary. A second
layer needs its own distinct risk (a transport, lifecycle, or platform failure
the owner cannot reach).

| Contract | Primary owner |
| --- | --- |
| CLI user-visible behavior | `crates/yoetz-cli/tests/cli.rs` (+ `council.rs`, `cursor_cli.rs`) |
| Native host protocol end-to-end | `crates/yoetz-cli/tests/native_extension_host.rs` |
| Session / bundle / registry semantics | `crates/yoetz-core/src/*.rs` unit tests |
| Native envelope / message contract | `extensions/chatgpt-native/src/protocol.js` + `tests/protocol.test.js` |
| Chunk + file transport | `extensions/chatgpt-native/src/chunks.js` + `tests/chunks.test.js` |
| Picker classification (pure read) | `src/chatgpt-picker-reader.js` + `tests/chatgpt-picker-reader.test.js` + `tests/fixtures/chatgpt-picker/*.html` + `expectations.json` |
| Picker driving / choreography | `tests/fake-chatgpt.test.js` |
| Claude driving | `tests/fake-claude.test.js` |
| Service-worker lifecycle / tab routing | `tests/service-worker.test.js` |
| Extension manifest / permissions / ID | `tests/manifest.test.js` (guards a security + packaging contract; static is the cheapest independent guard) |

`extensions/chatgpt-native/tests/fixtures/` and `expectations.json` are the
shared corpus. Prefer extending the corpus/table over adding a near-duplicate
test file. Read `CLAUDE.md` (Browser Architecture, Drift procedure) before
touching picker coverage.

## Authoring gate

Before adding any test, answer four questions; a missing answer means do not add
it yet:

1. What observable behavior, invariant, or independent contract does it protect?
2. What credible regression makes it fail?
3. Why does existing coverage not already catch that failure? If the same
   contract is already covered, extend the existing table/fixture instead.
4. Does it need a production seam (export, flag, wrapper, injection hook) that no
   production caller needs? If yes, move the test to the real boundary.

Then check the test against every [junk pattern](#junk-patterns). A match fails
the gate unless the [retention bar](#retention-bar) names the contract it
independently guards. A test that breaks under behavior-preserving refactoring is
asserting implementation, not behavior: rewrite it at the owning boundary.

Bug regression tests must fail on the pre-fix code for the intended reason and
pass after the repair. A regression test that never demonstrably failed proves
the mock, not the fix. One regression at the owner boundary covers the bug; do
not replay the same scenario at every layer it crosses.

## Junk patterns

The shared checklist for both modes. The authoring gate rejects a new test that
matches one; audits hunt for existing tests that do.

- assertion-free coverage probes; tests with no negative case a naive wrong
  implementation would fail;
- self-comparisons and identity copiers (`assert.equal(f(x), f(x))`), and
  expected values produced by the helper or renderer under test;
- copied fixtures, inventories, manifests, or export lists (e.g. a test that
  hardcodes the same `MESSAGE_TYPES` array declared in `protocol.js`; a manifest
  list duplicated from `manifest.json` with no independent source of truth);
- exact source, import, or string greps used as a stand-in for behavior;
- private predicate / call-shape tests duplicated at a real boundary;
- duplicate invocations of the same contract at the same layer (e.g. the same
  picker shape asserted in both `chatgpt-picker-reader.test.js` and a second
  reader-shaped unit file);
- provider-local replays of shared helpers (a `sites/claude.js` test re-testing
  `protocol.js` envelope logic);
- tests whose only purpose is preserving test-only exports, globals, or wrappers;
- dead production code whose only callers are tests;
- mocks that implement the asserted behavior, or one identical mock standing in
  for different APIs;
- fixtures that supply the receipt, admission, or callback ordering the owner
  should produce, or persistence asserted against a store the path never writes;
- capability tests that restate declared flags instead of exercising the
  delivery or acknowledgement the flag promises;
- negative controls that pass for an unrelated reason (a denial from a different
  guard, a path production never reaches);
- names or fixtures that promise more than the input exercises.
- **unbounded wall-clock**: a test that runs a real settle/retry/timeout loop at
  its production default budget. See [speed discipline](#speed-discipline).

## Retention bar

Keep a test when it independently enforces a public CLI/API, native protocol,
manifest/permission/security, storage, migration, platform, default, prompt-byte,
generated cross-language, package, or release contract. Also keep:

- call ordering and tab routing when the order is observable behavior;
- regressions with a credible failure mode;
- source inspection when it is the cheapest independent guard: it fails when the
  contract changes (a key, byte, path, or permission) and survives an
  identifier-only refactor;
- a retained test that fails on baseline: treat it as a possible product bug,
  reproduce it, and repair the owner rather than deleting it.

Static or slow is not by itself a deletion reason. A test that resembles
implementation may still be the independent contract; prove otherwise first.

## Speed discipline

The extension JS suite is the wall-clock hotspot (one file, ~264 tests, real
settles). "Faster" here means fewer real waits, not fewer assertions.

- Real timer loops belong to production code and must be reachable from tests.
  Any test that exercises model/picker selection must pass small explicit
  budgets (`picker_timeout_ms`, `picker_settle_ms`, `picker_open_attempts`,
  `model_selection_*`). A budget that is parsed but never threaded to the loop is
  a bug: #551 saw 4 tests burn ~29s each because `modelSelectionOptionsForJob`
  dropped the option. Verify the option reaches the loop; do not just set it.
- A test whose body exceeds ~2s of real wall time is a smell. Find the settle
  loop and bound it; do not `--test-skip`.
- Prefer a fake clock / injected timer over `sleep` when the owner supports it.
- Keep one fixture per picker shape and drive it through the real boundary; do
  not clone a fixture to avoid sharing setup.
- `node --test` parallelizes across files, not within a file. If one file
  dominates, splitting it into coherent files (`fake-chatgpt-*.test.js`) is a
  legitimate speed change, but keep the contract owner obvious.
- Locally run only the focused file you changed plus its discrimination run
  against `main` (see `CLAUDE.md`); full suites run in CI. Never claim a full
  suite ran when only the focused file ran.

## Audit workflow

Keep discovery read-only and report evidence before editing.

1. Read the complete test, its production owner (entry point, callers, callees,
   sibling implementations), overlapping tests, CI routing, and relevant
   history (`git log -p --follow`). Read `CLAUDE.md` and any scoped `AGENTS.md`
   first.
2. Hunt the [junk patterns](#junk-patterns). For slow candidates, measure before
   cutting: `node --test --test-reporter=spec tests/<file>.test.js` and find the
   top durations.
3. Record [candidate evidence](#candidate-evidence) for each.
4. Choose one coherent owner-boundary batch. Delete obsolete test-only exports,
   globals, and wrappers instead of aliasing them. Move retained regressions to
   the canonical owner. Prefer net-negative production LOC.
5. Run [validation](#validation), then report [handoff](#handoff).

## Candidate evidence

Record every field before editing. A missing field means the candidate is not
ready for deletion:

- exact test name and file;
- what failure it can actually detect;
- non-test callers of the covered production or support seam;
- the stronger remaining owner-boundary proof, or why none is needed;
- relevant history and why the test or seam exists;
- production or test-support deletion it unlocks;
- risk and the focused validation command.

## Validation

1. Focused JS: `node --test --test-reporter=spec tests/<file>.test.js` from
   `extensions/chatgpt-native/`.
2. Full JS (before claiming suite parity):
   `node --test extensions/chatgpt-native/tests/*.test.js`.
3. Lint: `npm run lint --prefix extensions/chatgpt-native`.
4. Rust focused: `cargo test -p yoetz <filter>` /
   `cargo test -p yoetz-core <filter>`. Full: `cargo test --workspace`
   (CI owns the full run; run it when you touched shared code).
5. Picker changes: `node --test tests/chatgpt-picker-reader.test.js` and
   `node ../../scripts/picker-reader-parity.mjs` (from
   `extensions/chatgpt-native`; must exit 0).
6. Change routing: `./scripts/ci-classify-changes.sh ...` behavior is pinned by
   `./scripts/test-ci-classify-changes.sh`; run it when editing routing.
7. `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.
8. Inspect `git diff --numstat`; report production/tooling separately from tests
   and test support.

## Landing

Commit/push/PR only when authorized, and only after peer review per
`CLAUDE.md`. Then remove your worktree and prune the merged branch. Land one
coherent PR at a time; after landing, refresh from `main` and rerun read-only
discovery for the next high-confidence batch.

## Handoff

Report:

- root cause and removed low-value categories;
- production owner simplifications (net production LOC, not deletion count);
- retained false positives and why they remain valuable;
- focused and full proof actually run, with wall-clock before/after for speed
  work;
- production versus test LOC;
- PR and merge state;
- named follow-ups.

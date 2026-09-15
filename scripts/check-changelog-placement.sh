#!/usr/bin/env bash
# yz-lc1: fail when a PR's CHANGELOG.md diff ADDS lines under any heading
# other than ## [Unreleased]. Release notes are generated from Unreleased, so
# an entry misfiled under an already-released section silently disappears from
# the next release's notes (observed on the yz-eld merge, fixed by #538).
#
# Usage: check-changelog-placement.sh <base-sha>
# Exempt (callers skip before invoking): release PRs whose title or head
# commit message starts with chore(release): — release.sh moves the Unreleased
# section itself.
set -euo pipefail

if (($# != 1)); then
  echo "usage: $0 <base-sha>" >&2
  exit 2
fi

base="$1"
changed=0
section=""

# Walk the unified diff of CHANGELOG.md and track which ## heading each added
# line falls under. Blank lines and headings themselves are not "additions".
# Context lines (-U0 still emits the hunk header's pre-image anchor) are not
# needed; only + lines are additions.
while IFS= read -r line; do
  case "$line" in
    '+++'* | '---'*)
      # File header; nothing to track.
      ;;
    '@@'*)
      # Position unknown at hunk start; the next '## ' added or context line
      # establishes it. -U0 drops context, so also accept ' ' lines in case
      # callers raise the context level.
      section=""
      ;;
    '+'*)
      line="${line#+}"
      if [[ -z "$line" ]]; then
        continue
      fi
      if [[ "$line" == '## '* ]]; then
        section="$line"
        continue
      fi
      if [[ "$section" != '## [Unreleased]' ]]; then
        echo "CHANGELOG.md: added line under '${section:-<no section>}' (must be under '## [Unreleased]'):" >&2
        echo "  + $line" >&2
        changed=1
      fi
      ;;
    ' '*)
      line="${line# }"
      if [[ "$line" == '## '* ]]; then
        section="$line"
      fi
      ;;
  esac
done < <(git diff -U0 "$base" -- CHANGELOG.md)

if ((changed)); then
  echo "" >&2
  echo "PR adds CHANGELOG.md content outside ## [Unreleased] (yz-lc1)." >&2
  echo "Release notes are generated from Unreleased only; move the entry." >&2
  exit 1
fi

echo "CHANGELOG.md placement ok (no additions outside ## [Unreleased])."

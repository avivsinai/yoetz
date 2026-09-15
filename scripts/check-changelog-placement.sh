#!/usr/bin/env bash
# yz-lc1: fail when a PR's CHANGELOG.md diff ADDS lines under any heading
# other than ## [Unreleased]. Release notes are generated from Unreleased, so
# an entry misfiled under an already-released section silently disappears from
# the next release's notes (observed on the yz-eld merge, fixed by #538).
#
# Known false positive: any added line under a released heading trips this,
# including a legitimate typo fix in old release notes. Acceptable: the check
# is advisory (not a required check), so a false positive only surfaces the
# edit for review.
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

# Map each ## heading to its 1-based line number in the PRE-IMAGE (base)
# CHANGELOG.md, then walk the unified diff (-U0): for each hunk, use the
# pre-image start line to find the heading the added lines fall under.
# Blank lines and headings themselves are not "additions".
declare -a heading_lines=()
declare -a heading_names=()
old_line=0
while IFS= read -r line; do
  old_line=$((old_line + 1))
  if [[ "$line" == '## '* ]]; then
    heading_lines+=("$old_line")
    heading_names+=("$line")
  fi
done < <(git show "$base":CHANGELOG.md)

section_for_line() {
  local target="$1" i
  local result=""
  for ((i = 0; i < ${#heading_lines[@]}; i++)); do
    if ((heading_lines[i] <= target)); then
      result="${heading_names[i]}"
    fi
  done
  printf '%s' "$result"
}

while IFS= read -r line; do
  case "$line" in
    '+++'* | '---'*)
      # File header; nothing to track.
      ;;
    '@@'*)
      # "@@ -old_start,old_count +new_start,new_count @@ ..." — take the
      # pre-image start (first number after '-'), defaulting to 1 for
      # single-number form. Added lines land at/after this line's section.
      hunk_old_start="${line#@*-}"
      hunk_old_start="${hunk_old_start%%,*}"
      hunk_old_start="${hunk_old_start%% *}"
      hunk_old_start="${hunk_old_start%%@*}"
      if [[ -z "$hunk_old_start" || "$hunk_old_start" == 0 ]]; then
        hunk_old_start=1
      fi
      section="$(section_for_line "$hunk_old_start")"
      ;;
    '+'*)
      line="${line#+}"
      if [[ -z "$line" ]]; then
        continue
      fi
      if [[ "$line" == '## '* ]]; then
        # An added heading itself shifts subsequent lines into it.
        section="$line"
        continue
      fi
      if [[ "$section" != '## [Unreleased]' ]]; then
        echo "CHANGELOG.md: added line under '${section:-<no section>}' (must be under '## [Unreleased]'):" >&2
        echo "  + $line" >&2
        changed=1
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

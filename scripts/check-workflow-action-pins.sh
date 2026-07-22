#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

trim() {
  local value="$1"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  printf '%s' "$value"
}

files=()
if (($# > 0)); then
  files=("$@")
else
  while IFS= read -r workflow; do
    files+=("$workflow")
  done < <(find "${ROOT_DIR}/.github/workflows" -type f \( -name '*.yml' -o -name '*.yaml' \) | LC_ALL=C sort)
fi

violations=0
for workflow in "${files[@]}"; do
  if [[ ! -f "$workflow" ]]; then
    echo "${workflow}: workflow file not found" >&2
    violations=$((violations + 1))
    continue
  fi

  line_number=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    line_number=$((line_number + 1))
    if [[ ! "$line" =~ ^[[:space:]]*(-[[:space:]]*)?uses:[[:space:]]*(.*)$ ]]; then
      continue
    fi

    action="$(trim "${BASH_REMATCH[2]%%#*}")"
    if [[ "$action" == \"*\" || "$action" == \'*\' ]]; then
      action="${action:1:${#action}-2}"
    fi

    case "$action" in
      ./*)
        continue
        ;;
      docker://*)
        if [[ "$action" =~ ^docker://[^@[:space:]]+@sha256:[0-9a-fA-F]{64}$ ]]; then
          continue
        fi
        echo "${workflow}:${line_number}: Docker action must use an immutable sha256 digest: ${action}" >&2
        ;;
      *)
        if [[ "$action" =~ ^[^/@[:space:]]+/[^@[:space:]]+@[0-9a-fA-F]{40}$ ]]; then
          continue
        fi
        echo "${workflow}:${line_number}: remote action must use a full 40-hex commit SHA: ${action}" >&2
        ;;
    esac
    violations=$((violations + 1))
  done <"$workflow"
done

if ((violations > 0)); then
  echo "Found ${violations} mutable or invalid workflow action reference(s)." >&2
  exit 1
fi

echo "Workflow action pins verified."

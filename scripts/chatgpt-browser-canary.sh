#!/usr/bin/env bash
#
# Gated live ChatGPT browser canary.
#
# This verifies the real logged-in browser recipe path and the standardized
# JSON contract, but only when the operator opts in explicitly. It is not safe
# for default CI because it requires a live authenticated ChatGPT session.

set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

[[ "${YOETZ_CHATGPT_CANARY:-}" == "1" ]] || die "set YOETZ_CHATGPT_CANARY=1 to run the live ChatGPT canary"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
yoetz_bin="${YOETZ_BIN:-${repo_root}/target/debug/yoetz}"
[[ -x "${yoetz_bin}" ]] || die "yoetz binary not found or not executable: ${yoetz_bin}"

cdp_endpoint="${YOETZ_CHATGPT_CDP:-${YOETZ_BROWSER_CDP:-}}"
[[ -n "${cdp_endpoint}" ]] || die "set YOETZ_CHATGPT_CDP (or YOETZ_BROWSER_CDP) to the live Chrome CDP endpoint"

command -v jq >/dev/null 2>&1 || die "required command not found: jq"

export YOETZ_AGENT=1
export YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB="${YOETZ_ALLOW_MUTATE_EXISTING_CHATGPT_TAB:-1}"
export YOETZ_ALLOW_USER_TAB_ANCHOR="${YOETZ_ALLOW_USER_TAB_ANCHOR:-1}"

bundle_path="$("${yoetz_bin}" bundle -p 'Reply with exactly OK.' --format json | jq -r '.artifacts.bundle_md')"
[[ -n "${bundle_path}" && -f "${bundle_path}" ]] || die "failed to create canary bundle"

echo "==> running live ChatGPT browser canary via ${yoetz_bin}"
result="$("${yoetz_bin}" browser recipe \
  --recipe chatgpt \
  --bundle "${bundle_path}" \
  --cdp "${cdp_endpoint}" \
  --var model=pro \
  --var wait_timeout_ms="${YOETZ_CHATGPT_WAIT_TIMEOUT_MS:-2400000}" \
  --format json)"

echo "${result}"

for field in status transport backend response model_used warnings fallback_used delivery_mode auto_paste_fallback; do
  printf '%s' "${result}" | jq -e --arg field "${field}" 'has($field)' >/dev/null || die "missing JSON field: ${field}"
done

status="$(printf '%s' "${result}" | jq -r '.status')"
model_used="$(printf '%s' "${result}" | jq -r '.model_used // empty')"
delivery_mode="$(printf '%s' "${result}" | jq -r '.delivery_mode')"

[[ "${status}" == "ok" ]] || die "recipe did not return status=ok"
[[ -n "${model_used}" ]] || die "recipe did not report model_used"
[[ "${delivery_mode}" == "file_upload" || "${delivery_mode}" == "paste" ]] || die "unexpected delivery_mode: ${delivery_mode}"

echo "==> live ChatGPT canary passed"

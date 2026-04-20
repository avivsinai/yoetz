#!/usr/bin/env bash
#
# Real-browser CI smoke test.
#
# Launches a fresh Chrome for Testing instance on a random loopback port, then
# drives the locally-built `yoetz` binary against it over CDP. Exits non-zero
# on any failure so it can gate CI (review finding #13).
#
# Requires:
#   - YOETZ_CHROME_BIN pointing at a Chrome (or Chrome for Testing) binary
#   - `yoetz` binary built in release mode at the expected target path
#     (override with YOETZ_BIN=...)
#   - curl, jq
#
# The fixture is a throwaway user-data-dir under $TMPDIR so we never touch the
# operator's real Chrome profile.

set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

chrome_bin="${YOETZ_CHROME_BIN:-}"
if [[ -z "${chrome_bin}" ]]; then
  die "YOETZ_CHROME_BIN is not set; point it at a Chrome for Testing binary"
fi
if [[ ! -x "${chrome_bin}" ]]; then
  die "Chrome binary is not executable: ${chrome_bin}"
fi

yoetz_bin="${YOETZ_BIN:-}"
if [[ -z "${yoetz_bin}" ]]; then
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  yoetz_bin="${repo_root}/target/release/yoetz"
fi
if [[ ! -x "${yoetz_bin}" ]]; then
  die "yoetz binary not found or not executable: ${yoetz_bin} (build with \`cargo build --release --bin yoetz\` or set YOETZ_BIN)"
fi

for cmd in curl jq; do
  command -v "${cmd}" >/dev/null 2>&1 || die "required command not found: ${cmd}"
done

# Allocate a free TCP port without leaking the listener into Chrome's namespace.
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
[[ -n "${port}" ]] || die "failed to allocate a free port"

data_dir="$(mktemp -d -t yoetz-ci-chrome.XXXXXXXX)"
chrome_log="${data_dir}/chrome.log"

cleanup() {
  if [[ -n "${chrome_pid:-}" ]]; then
    kill "${chrome_pid}" >/dev/null 2>&1 || true
    wait "${chrome_pid}" 2>/dev/null || true
  fi
  rm -rf "${data_dir}" 2>/dev/null || true
}
trap cleanup EXIT

chrome_extra_args=()
if [[ "$(uname -s)" == "Linux" && ( -n "${CI:-}" || -n "${GITHUB_ACTIONS:-}" ) ]]; then
  # Hosted Linux runners often install Chrome-for-Testing without a usable
  # setuid sandbox helper, so headless launch fails unless we opt out here.
  chrome_extra_args+=(--no-sandbox)
fi

echo "==> launching Chrome for Testing on port ${port}"
"${chrome_bin}" \
  --headless=new \
  "${chrome_extra_args[@]}" \
  --remote-debugging-port="${port}" \
  --user-data-dir="${data_dir}" \
  --no-first-run \
  --no-default-browser-check \
  --disable-features=Translate,MediaRouter \
  --hide-scrollbars \
  --mute-audio \
  about:blank \
  >"${chrome_log}" 2>&1 &
chrome_pid=$!

endpoint="http://127.0.0.1:${port}"
deadline=$((SECONDS + 30))
while (( SECONDS < deadline )); do
  if curl -fsS --max-time 2 "${endpoint}/json/version" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "${chrome_pid}" >/dev/null 2>&1; then
    echo "--- chrome log ---"
    cat "${chrome_log}" >&2 || true
    die "Chrome process exited before CDP became reachable"
  fi
  sleep 0.25
done

if ! curl -fsS --max-time 2 "${endpoint}/json/version" >/dev/null 2>&1; then
  echo "--- chrome log ---"
  cat "${chrome_log}" >&2 || true
  die "Chrome CDP did not become reachable on ${endpoint} within 30s"
fi

echo "==> Chrome reachable at ${endpoint}"
echo "==> exercising \`yoetz browser verify-cdp\` against the live CDP endpoint"

export YOETZ_AGENT=1
verify_output="$("${yoetz_bin}" browser verify-cdp --cdp "${endpoint}" --format json)"
echo "${verify_output}"

status="$(printf '%s' "${verify_output}" | jq -r '.status // empty')"
page_id="$(printf '%s' "${verify_output}" | jq -r '.page_id // empty')"
if [[ "${status}" != "ok" ]]; then
  die "yoetz browser verify-cdp did not return status=ok (got ${status:-<empty>})"
fi
if [[ -z "${page_id}" ]]; then
  die "yoetz browser verify-cdp did not report a page_id"
fi
echo "==> attach + fresh-tab creation verified; page_id=${page_id}"

# Smoke: attaching twice must remain stable — no leaked daemons / wedged
# sessions on the second call.
"${yoetz_bin}" browser verify-cdp --cdp "${endpoint}" --format json >/dev/null
echo "==> second attach also succeeded"

echo "==> CI real-browser smoke passed"

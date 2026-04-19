#!/bin/bash
set -euo pipefail

# Setup script for ChatGPT browser profile
# Delegates to the CLI login command for consistent behavior
# Preserves AGENT_BROWSER_PROFILE for backward compatibility

PROFILE_DIR="${AGENT_BROWSER_PROFILE:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

resolve_yoetz_bin() {
    if [ -n "${YOETZ_BIN:-}" ]; then
        printf '%s\n' "${YOETZ_BIN}"
        return 0
    fi

    for candidate in \
        "${REPO_ROOT}/target/release/yoetz" \
        "${REPO_ROOT}/target/debug/yoetz"
    do
        if [ -x "${candidate}" ]; then
            printf '%s\n' "${candidate}"
            return 0
        fi
    done

    if command -v yoetz >/dev/null 2>&1; then
        command -v yoetz
        return 0
    fi

    return 1
}

YOETZ_CMD="$(resolve_yoetz_bin)" || {
    echo "error: could not find a yoetz binary. Build this repo first or set YOETZ_BIN." >&2
    exit 1
}

if [ -n "$PROFILE_DIR" ]; then
    exec "$YOETZ_CMD" browser login --profile "$PROFILE_DIR" "$@"
else
    exec "$YOETZ_CMD" browser login "$@"
fi

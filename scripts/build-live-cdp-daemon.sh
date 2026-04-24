#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/crates/yoetz-cli/assets/live-cdp-daemon-src"

if [ ! -d "$SRC/node_modules" ]; then
  npm ci --prefix "$SRC" --ignore-scripts
fi

if [ "${1:-}" = "--check" ]; then
  npm run --prefix "$SRC" check
else
  npm run --prefix "$SRC" build
fi

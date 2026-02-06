#!/bin/bash
set -euo pipefail

# Setup script for ChatGPT browser profile
# Delegates to the CLI login command for consistent behavior
# Preserves AGENT_BROWSER_PROFILE for backward compatibility

PROFILE_DIR="${AGENT_BROWSER_PROFILE:-}"

if [ -n "$PROFILE_DIR" ]; then
    yoetz browser login --profile "$PROFILE_DIR" "$@"
else
    yoetz browser login "$@"
fi

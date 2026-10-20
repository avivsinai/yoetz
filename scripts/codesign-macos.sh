#!/usr/bin/env bash
set -euo pipefail

binary_path="${1:?usage: ./scripts/codesign-macos.sh /path/to/binary identifier [target-os]}"
identifier="${2:?usage: ./scripts/codesign-macos.sh /path/to/binary identifier [target-os]}"
target_os="${3:-darwin}"

if [[ "$target_os" != "darwin" ]]; then
  exit 0
fi

if [[ ! -f "$binary_path" ]]; then
  echo "error: binary not found: $binary_path" >&2
  exit 1
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  exit 0
fi

# Release builds: sign with the Developer ID that release.yml imports, then
# notarize, so Gatekeeper accepts a downloaded binary. Keep the default DR: it
# anchors to the Apple-issued certificate of our team, not only to the
# identifier, and Developer ID code gets the stable Keychain partition ID
# `teamid:<TEAM_ID>` instead of a per-build `cdhash:<hash>`.
if [[ -n "${YOETZ_MACOS_TEAM_ID:-}" ]]; then
  : "${YOETZ_MACOS_KEYCHAIN:?YOETZ_MACOS_KEYCHAIN must be set with YOETZ_MACOS_TEAM_ID}"
  : "${YOETZ_MACOS_SIGN_IDENTITY:?YOETZ_MACOS_SIGN_IDENTITY must be set with YOETZ_MACOS_TEAM_ID}"
  : "${YOETZ_NOTARY_KEY_PATH:?YOETZ_NOTARY_KEY_PATH must be set with YOETZ_MACOS_TEAM_ID}"
  : "${YOETZ_NOTARY_KEY_ID:?YOETZ_NOTARY_KEY_ID must be set with YOETZ_MACOS_TEAM_ID}"
  : "${YOETZ_NOTARY_ISSUER_ID:?YOETZ_NOTARY_ISSUER_ID must be set with YOETZ_MACOS_TEAM_ID}"

  codesign \
    --force \
    --options runtime \
    --timestamp \
    --keychain "$YOETZ_MACOS_KEYCHAIN" \
    --sign "$YOETZ_MACOS_SIGN_IDENTITY" \
    --identifier "$identifier" \
    "$binary_path"

  team_id="$(codesign -dv "$binary_path" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  if [[ "$team_id" != "$YOETZ_MACOS_TEAM_ID" ]]; then
    echo "error: $binary_path has TeamIdentifier '$team_id', expected '$YOETZ_MACOS_TEAM_ID'" >&2
    exit 1
  fi

  # notarytool takes a zip; a bare Mach-O cannot be stapled, so Gatekeeper
  # checks the ticket online.
  work_dir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/yoetz-notary.XXXXXX")"
  trap 'rm -rf "$work_dir"' EXIT
  ditto -c -k --keepParent "$binary_path" "$work_dir/yoetz.zip"
  submit_rc=0
  xcrun notarytool submit "$work_dir/yoetz.zip" \
    --key "$YOETZ_NOTARY_KEY_PATH" \
    --key-id "$YOETZ_NOTARY_KEY_ID" \
    --issuer "$YOETZ_NOTARY_ISSUER_ID" \
    --wait \
    --timeout 20m \
    --output-format json >"$work_dir/result.json" || submit_rc=$?

  status="$(plutil -extract status raw -o - "$work_dir/result.json" 2>/dev/null || echo "unknown (notarytool exit $submit_rc)")"
  if [[ "$status" != "Accepted" ]]; then
    submission_id="$(plutil -extract id raw -o - "$work_dir/result.json" 2>/dev/null || true)"
    echo "error: notarization of $binary_path ended with status '$status' (submission $submission_id)" >&2
    if [[ -n "$submission_id" ]]; then
      xcrun notarytool log "$submission_id" \
        --key "$YOETZ_NOTARY_KEY_PATH" \
        --key-id "$YOETZ_NOTARY_KEY_ID" \
        --issuer "$YOETZ_NOTARY_ISSUER_ID" >&2 || true
    fi
    exit 1
  fi
  exit 0
fi

# Local, fork and unconfigured release builds: sign ad-hoc with a stable
# identifier, as release builds did before Developer ID signing.
codesign --force --sign - --identifier "$identifier" "$binary_path"

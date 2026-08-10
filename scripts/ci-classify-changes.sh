#!/usr/bin/env bash
set -euo pipefail

LINUX_MATRIX='["ubuntu-latest"]'
FULL_MATRIX='["ubuntu-latest","macos-latest","windows-latest"]'

emit_outputs() {
  printf 'light_only=%s\n' "$1"
  printf 'browser_changed=%s\n' "$2"
  printf 'build_os_matrix=%s\n' "$3"
  printf 'release_push=%s\n' "$4"
  printf 'dependency_changed=%s\n' "$5"
  printf 'workflow_changed=%s\n' "$6"
}

if (($# < 3)); then
  emit_outputs false true "$LINUX_MATRIX" false false false
  exit 0
fi

event_name="$1"
head_ref="$2"
subject="$3"
shift 3
files=("$@")

case "$event_name" in
  workflow_dispatch)
    emit_outputs false true "$FULL_MATRIX" false false false
    exit 0
    ;;
  push)
    if [[ "$subject" == chore\(release\):\ v* ]]; then
      emit_outputs true false "$LINUX_MATRIX" true false false
      exit 0
    fi
    ;;
  pull_request) ;;
  *)
    emit_outputs false true "$LINUX_MATRIX" false false false
    exit 0
    ;;
esac

if ((${#files[@]} == 0)); then
  emit_outputs false true "$LINUX_MATRIX" false false false
  exit 0
fi

light_only=true
browser_changed=false
dependency_changed=false
workflow_changed=false
release_metadata_only=true

for file in "${files[@]}"; do
  case "$file" in
    Cargo.toml | Cargo.lock)
      dependency_changed=true
      light_only=false
      ;;
    .github/*)
      workflow_changed=true
      light_only=false
      ;;
    CHANGELOG.md | .claude-plugin/plugin.json | .codex-plugin/plugin.json | skills/*/SKILL.md)
      ;;
    *.md | docs/* | README* | .gitignore | .gitleaks.toml | LICENSE*)
      ;;
    *)
      light_only=false
      ;;
  esac

  case "$file" in
    CHANGELOG.md | Cargo.toml | Cargo.lock | .claude-plugin/plugin.json | .codex-plugin/plugin.json | extensions/chatgpt-native/manifest.json | skills/*/SKILL.md)
      ;;
    *)
      release_metadata_only=false
      ;;
  esac

  case "$file" in
    extensions/* | recipes/* | scripts/build-chatgpt-native-extension.sh | scripts/build-live-cdp-daemon.sh | scripts/ci-real-browser-smoke.sh)
      browser_changed=true
      ;;
    crates/yoetz-cli/src/*browser* | crates/yoetz-cli/src/*chatgpt* | crates/yoetz-cli/src/live_* | crates/yoetz-cli/src/chrome_* | crates/yoetz-cli/tests/*browser* | crates/yoetz-cli/tests/*chatgpt*)
      browser_changed=true
      ;;
  esac
done

build_os_matrix="$LINUX_MATRIX"
if [[ "$dependency_changed" == "true" ]]; then
  build_os_matrix="$FULL_MATRIX"
fi

release_tag="${head_ref#release/}"
if [[ "$event_name" == "pull_request" &&
  "$head_ref" =~ ^release/v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z]+)*$ &&
  "$subject" == "chore(release): ${release_tag}" &&
  "$release_metadata_only" == "true" ]]; then
  emit_outputs true false "$build_os_matrix" false "$dependency_changed" false
  exit 0
fi

emit_outputs "$light_only" "$browser_changed" "$build_os_matrix" false "$dependency_changed" "$workflow_changed"

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLASSIFIER="${ROOT_DIR}/scripts/ci-classify-changes.sh"
LINUX_MATRIX='["ubuntu-latest"]'
FULL_MATRIX='["ubuntu-latest","macos-latest","windows-latest"]'

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

run_case() {
  local name="$1"
  local event_name="$2"
  local head_ref="$3"
  local subject="$4"
  local expected_light_only="$5"
  local expected_browser_changed="$6"
  local expected_build_os_matrix="$7"
  local expected_release_push="$8"
  local expected_dependency_changed="$9"
  local expected_workflow_changed="${10}"
  shift 10

  local output
  if ! output="$(${CLASSIFIER} "$event_name" "$head_ref" "$subject" "$@")"; then
    fail "${name}: classifier exited non-zero"
  fi

  local output_count
  output_count="$(printf '%s\n' "$output" | sed -n '/^[a-z_][a-z_]*=/p' | wc -l | tr -d ' ')"
  [[ "$output_count" == "6" ]] || fail "${name}: expected 6 outputs, got ${output_count}: ${output}"

  local actual_light_only actual_browser_changed actual_build_os_matrix
  local actual_release_push actual_dependency_changed actual_workflow_changed
  actual_light_only="$(printf '%s\n' "$output" | sed -n 's/^light_only=//p')"
  actual_browser_changed="$(printf '%s\n' "$output" | sed -n 's/^browser_changed=//p')"
  actual_build_os_matrix="$(printf '%s\n' "$output" | sed -n 's/^build_os_matrix=//p')"
  actual_release_push="$(printf '%s\n' "$output" | sed -n 's/^release_push=//p')"
  actual_dependency_changed="$(printf '%s\n' "$output" | sed -n 's/^dependency_changed=//p')"
  actual_workflow_changed="$(printf '%s\n' "$output" | sed -n 's/^workflow_changed=//p')"

  [[ "$actual_light_only" == "$expected_light_only" ]] || fail "${name}: light_only=${actual_light_only}, want ${expected_light_only}"
  [[ "$actual_browser_changed" == "$expected_browser_changed" ]] || fail "${name}: browser_changed=${actual_browser_changed}, want ${expected_browser_changed}"
  [[ "$actual_build_os_matrix" == "$expected_build_os_matrix" ]] || fail "${name}: build_os_matrix=${actual_build_os_matrix}, want ${expected_build_os_matrix}"
  [[ "$actual_release_push" == "$expected_release_push" ]] || fail "${name}: release_push=${actual_release_push}, want ${expected_release_push}"
  [[ "$actual_dependency_changed" == "$expected_dependency_changed" ]] || fail "${name}: dependency_changed=${actual_dependency_changed}, want ${expected_dependency_changed}"
  [[ "$actual_workflow_changed" == "$expected_workflow_changed" ]] || fail "${name}: workflow_changed=${actual_workflow_changed}, want ${expected_workflow_changed}"

  echo "PASS: ${name}"
}

run_case "docs only stays light" \
  pull_request feature/docs "docs: refresh guide" \
  true false "$LINUX_MATRIX" false false false \
  README.md docs/guide.md

run_case "Cargo.lock gets the full build matrix" \
  pull_request dependabot/cargo/time-0.3.47 "chore(deps): bump time" \
  false false "$FULL_MATRIX" false true false \
  Cargo.lock

run_case "Cargo.toml plus docs stays dependency-heavy" \
  pull_request feature/dependency "build: change dependency" \
  false false "$FULL_MATRIX" false true false \
  Cargo.toml docs/dependencies.md

run_case "workflow changes fail out of the light path" \
  pull_request fix/ci "ci: fix classifier" \
  false false "$LINUX_MATRIX" false false true \
  .github/workflows/ci.yml

run_case "browser changes retain browser coverage" \
  pull_request feature/claude "feat: update Claude adapter" \
  false true "$LINUX_MATRIX" false false false \
  extensions/chatgpt-native/src/sites/claude.js

run_case "ordinary Rust changes keep the Linux build matrix" \
  pull_request fix/parser "fix: reject invalid input" \
  false false "$LINUX_MATRIX" false false false \
  crates/yoetz-core/src/lib.rs

run_case "valid release metadata uses the release fast path" \
  pull_request release/v0.5.43 "chore(release): v0.5.43" \
  true false "$FULL_MATRIX" false true false \
  CHANGELOG.md Cargo.toml Cargo.lock \
  .codex-plugin/plugin.json .claude-plugin/plugin.json \
  extensions/chatgpt-native/manifest.json skills/yoetz/SKILL.md

run_case "Cargo.lock alone on a valid release branch stays release-light" \
  pull_request release/v0.5.43 "chore(release): v0.5.43" \
  true false "$FULL_MATRIX" false true false \
  Cargo.lock

run_case "release branch with source fails closed" \
  pull_request release/v0.5.43 "chore(release): v0.5.43" \
  false false "$FULL_MATRIX" false true false \
  Cargo.lock crates/yoetz-core/src/lib.rs

run_case "release branch without the matching release title fails closed" \
  pull_request release/v0.5.43 "chore: unrelated metadata" \
  false false "$FULL_MATRIX" false true false \
  Cargo.lock

run_case "release commit push keeps its dedicated fast path" \
  push "" "chore(release): v0.5.43" \
  true false "$LINUX_MATRIX" true false false

run_case "workflow dispatch requests the full matrix" \
  workflow_dispatch "" "" \
  false true "$FULL_MATRIX" false false false

run_case "empty file lists fail closed" \
  pull_request feature/empty "test: empty diff" \
  false true "$LINUX_MATRIX" false false false

run_case "unknown events fail closed" \
  schedule "" "" \
  false true "$LINUX_MATRIX" false false false \
  README.md

WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"
grep -Fq "dependency_changed: \${{ steps.detect.outputs.dependency_changed }}" "$WORKFLOW" || fail "workflow does not publish dependency_changed"
grep -Fq "workflow_changed: \${{ steps.detect.outputs.workflow_changed }}" "$WORKFLOW" || fail "workflow does not publish workflow_changed"
grep -Fq './scripts/test-ci-classify-changes.sh' "$WORKFLOW" || fail "workflow does not run the classifier table tests"
grep -Fq "./scripts/ci-classify-changes.sh \\" "$WORKFLOW" || fail "workflow does not invoke the classifier"

echo "All CI classifier tests passed."

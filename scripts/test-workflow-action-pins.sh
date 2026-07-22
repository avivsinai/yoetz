#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY="${ROOT_DIR}/scripts/check-workflow-action-pins.sh"
FIXTURE_DIR="$(mktemp -d)"
trap 'rm -rf -- "$FIXTURE_DIR"' EXIT
fixture_counter=0

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

write_fixture() {
  fixture_counter=$((fixture_counter + 1))
  fixture_path="${FIXTURE_DIR}/fixture-${fixture_counter}.yml"
  printf '%s\n' "$1" >"$fixture_path"
}

expect_pass() {
  local name="$1"
  local source="$2"
  local output
  write_fixture "$source"
  if ! output="$(${POLICY} "$fixture_path" 2>&1)"; then
    fail "${name}: expected pass, got: ${output}"
  fi
  echo "PASS: ${name}"
}

expect_fail() {
  local name="$1"
  local source="$2"
  local output
  write_fixture "$source"
  if output="$(${POLICY} "$fixture_path" 2>&1)"; then
    fail "${name}: expected failure, got: ${output}"
  fi
  echo "PASS: ${name}"
}

expect_pass "40-hex GitHub action pin" \
  '    - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7.0.0'

expect_pass "quoted 40-hex reusable workflow pin" \
  '    uses: "owner/repo/.github/workflows/reuse.yml@0123456789abcdef0123456789abcdef01234567"'

expect_pass "local action path" \
  '    - uses: ./.github/actions/check'

expect_pass "Docker image digest" \
  '    - uses: docker://rhysd/actionlint:1.7.12@sha256:b1934ee5f1c509618f2508e6eb47ee0d3520686341fec936f3b79331f9315667'

expect_fail "mutable GitHub action tag" \
  '    - uses: actions/checkout@v7'

expect_fail "short GitHub action SHA" \
  '    - uses: actions/checkout@9c091bb'

expect_fail "mutable Docker image tag" \
  '    - uses: docker://rhysd/actionlint:1.7.12'

expect_fail "short Docker digest" \
  '    - uses: docker://rhysd/actionlint@sha256:b1934ee5'

expect_fail "missing remote action ref" \
  '    - uses: actions/checkout'

if ! current_output="$(${POLICY} 2>&1)"; then
  fail "current workflows violate the pin policy: ${current_output}"
fi
echo "PASS: current workflows"

CI_WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"
grep -Fq 'workflow-policy:' "$CI_WORKFLOW" || fail "CI is missing the workflow-policy job"
grep -Fq 'name: Workflow Policy' "$CI_WORKFLOW" || fail "CI is missing the stable Workflow Policy check name"
grep -Fq './scripts/check-workflow-action-pins.sh' "$CI_WORKFLOW" || fail "Workflow Policy does not enforce action pins"
grep -Fq 'docker://rhysd/actionlint:1.7.12@sha256:b1934ee5f1c509618f2508e6eb47ee0d3520686341fec936f3b79331f9315667' "$CI_WORKFLOW" || fail "Workflow Policy does not use the approved immutable actionlint image"

echo "All workflow action pin tests passed."

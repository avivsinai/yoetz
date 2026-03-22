#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/release.sh <version>

Examples:
  ./scripts/release.sh 0.2.24
  ./scripts/release.sh v0.2.24

This script:
1. Verifies you are on a clean, up-to-date main branch
2. Creates release/vX.Y.Z
3. Bumps [workspace.package].version in Cargo.toml
4. Runs cargo check --workspace
5. Commits the release bump as chore(release): vX.Y.Z
6. Pushes the branch
7. Creates a GitHub PR with gh

After the PR merges, the Release workflow creates vX.Y.Z automatically,
publishes artifacts, and generates release notes.
EOF
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

normalize_version() {
  local raw="$1"
  if [[ "$raw" =~ ^v ]]; then
    raw="${raw#v}"
  fi
  if [[ ! "$raw" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z]+)*$ ]]; then
    echo "error: version must look like 0.2.24 or v0.2.24" >&2
    exit 1
  fi
  printf '%s\n' "$raw"
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 1
fi

require_command git
require_command cargo
require_command perl

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

VERSION="$(normalize_version "$1")"
TAG="v${VERSION}"
BRANCH="release/${TAG}"

if [[ -n "$(git diff --stat HEAD)" ]]; then
  echo "error: working tree has uncommitted changes" >&2
  exit 1
fi

CURRENT_BRANCH="$(git branch --show-current)"
if [[ "$CURRENT_BRANCH" != "main" ]]; then
  echo "error: release prep must start from main (current: ${CURRENT_BRANCH})" >&2
  exit 1
fi

git fetch origin main --tags

LOCAL_MAIN="$(git rev-parse HEAD)"
REMOTE_MAIN="$(git rev-parse origin/main)"
if [[ "$LOCAL_MAIN" != "$REMOTE_MAIN" ]]; then
  echo "error: main is not up to date with origin/main; run git pull --ff-only first" >&2
  exit 1
fi

if git show-ref --verify --quiet "refs/heads/${BRANCH}"; then
  echo "error: branch already exists locally: ${BRANCH}" >&2
  exit 1
fi
if git ls-remote --exit-code --heads origin "${BRANCH}" >/dev/null 2>&1; then
  echo "error: branch already exists on origin: ${BRANCH}" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null 2>&1; then
  echo "error: tag already exists locally: ${TAG}" >&2
  exit 1
fi
if git ls-remote --exit-code --tags origin "refs/tags/${TAG}" >/dev/null 2>&1; then
  echo "error: tag already exists on origin: ${TAG}" >&2
  exit 1
fi

git switch -c "${BRANCH}"

YOETZ_VERSION="${VERSION}" perl -0pi -e '
  s/(\[workspace\.package\]\n(?:[^\[]*\n)*?version = ")[^"]+(")/${1}$ENV{YOETZ_VERSION}$2/s
    or die "failed to update [workspace.package].version\n";
' Cargo.toml

cargo check --workspace

git add Cargo.toml Cargo.lock
if git diff --cached --quiet; then
  echo "error: release prep produced no staged changes" >&2
  exit 1
fi

git commit -m "chore(release): ${TAG}"
git push -u origin "${BRANCH}"

PR_BODY=$(
  cat <<EOF
## Release

- bumps workspace version to \`${VERSION}\`
- merge triggers .github/workflows/release.yml, which creates the tag and
  publishes the release
- release notes are generated in CI from git-cliff during the release workflow
EOF
)

if command -v gh >/dev/null 2>&1; then
  gh pr create \
    --base main \
    --head "${BRANCH}" \
    --title "chore(release): ${TAG}" \
    --body "${PR_BODY}"
else
  cat <<EOF
Branch pushed: ${BRANCH}

Install/authenticate GitHub CLI to create the PR automatically, then run:
  gh pr create --base main --head "${BRANCH}" --title "chore(release): ${TAG}"
EOF
fi

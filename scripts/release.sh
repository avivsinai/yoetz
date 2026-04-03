#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/release.sh <version> [options]

Examples:
  ./scripts/release.sh 0.2.24
  ./scripts/release.sh v0.2.24

This script:
1. Verifies you are on a clean, up-to-date main branch
2. Creates release/vX.Y.Z
3. Moves CHANGELOG.md's Unreleased section into a versioned release entry
4. Bumps [workspace.package].version plus skill/plugin metadata
5. Runs cargo check/test/clippy/fmt release gates
6. Commits the release bump as chore(release): vX.Y.Z
7. Pushes the branch
8. Creates a GitHub PR with gh and enables squash auto-merge

After the PR merges, the Release workflow creates vX.Y.Z automatically,
publishes artifacts, and uses CHANGELOG.md as the release notes source.

Options:
  --date YYYY-MM-DD  Override release date (default: today in UTC)
  --allow-empty      Allow releasing with an empty Unreleased section
  --skip-verify      Skip cargo check/test/clippy/fmt release gates
  --no-auto-merge    Create the PR but do not enable auto-merge
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

require_command git
require_command cargo
require_command gh
require_command perl
require_command python3
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

release_date="$(date -u +%Y-%m-%d)"
allow_empty=0
skip_verify=0
auto_merge=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --date)
      [[ $# -ge 2 ]] || { echo "error: --date requires a value" >&2; exit 1; }
      release_date="$2"
      shift 2
      ;;
    --allow-empty)
      allow_empty=1
      shift
      ;;
    --skip-verify)
      skip_verify=1
      shift
      ;;
    --no-auto-merge)
      auto_merge=0
      shift
      ;;
    --*)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
    *)
      break
      ;;
  esac
done

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 1
fi

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

python3 - "$VERSION" "$release_date" "$allow_empty" <<'PY'
import pathlib
import re
import sys

version, release_date, allow_empty = sys.argv[1], sys.argv[2], sys.argv[3] == "1"

changelog = pathlib.Path("CHANGELOG.md")
text = changelog.read_text()
marker = "## [Unreleased]"
if marker not in text:
    raise SystemExit("error: CHANGELOG.md is missing the Unreleased section")

start = text.index(marker)
after_marker = start + len(marker)
rest = text[after_marker:]
match = re.search(r"(?m)^## \[", rest)
if match:
    unreleased_body = rest[:match.start()]
    suffix = rest[match.start():]
else:
    unreleased_body = rest
    suffix = ""

if not unreleased_body.strip() and not allow_empty:
    raise SystemExit("error: CHANGELOG.md Unreleased section is empty; add release notes first or pass --allow-empty")

release_header = f"\n\n## [{version}] - {release_date}\n"
new_text = text[:start] + marker + release_header + unreleased_body.lstrip("\n")
if suffix:
    new_text += suffix if suffix.startswith("\n") else "\n" + suffix

changelog.write_text(new_text)
PY

YOETZ_VERSION="${VERSION}" perl -0pi -e '
  s/(\[workspace\.package\]\n(?:[^\[]*\n)*?version = ")[^"]+(")/${1}$ENV{YOETZ_VERSION}$2/s
    or die "failed to update [workspace.package].version\n";
' Cargo.toml

# Bump plugin manifests and skill frontmatter.
for PLUGIN_JSON in .codex-plugin/plugin.json .claude-plugin/plugin.json; do
  if [[ -f "$PLUGIN_JSON" ]]; then
    sed -i '' "s/\"version\": \"[^\"]*\"/\"version\": \"${VERSION}\"/" "$PLUGIN_JSON" 2>/dev/null \
      || sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"${VERSION}\"/" "$PLUGIN_JSON"
  fi
done

SKILL_MD="skills/yoetz/SKILL.md"
if [[ -f "$SKILL_MD" ]]; then
  sed -i '' "s/^version: .*/version: ${VERSION}/" "$SKILL_MD" 2>/dev/null \
    || sed -i "s/^version: .*/version: ${VERSION}/" "$SKILL_MD"
fi

<<<<<<< HEAD
if [[ "$skip_verify" -eq 0 ]]; then
  cargo check --workspace
  cargo test --workspace
  cargo clippy --workspace -- -D warnings
  cargo fmt --all -- --check
fi
./scripts/check-release-version.sh "$VERSION"

git add CHANGELOG.md Cargo.toml Cargo.lock
for f in .codex-plugin/plugin.json .claude-plugin/plugin.json skills/yoetz/SKILL.md; do
  [[ -f "$f" ]] && git add "$f"
done
if git diff --cached --quiet; then
  echo "error: release prep produced no staged changes" >&2
  exit 1
fi

git commit -m "chore(release): ${TAG}"
git push -u origin "${BRANCH}"

PR_BODY=$(
  cat <<EOF
## Release

- updates \`CHANGELOG.md\` for \`${TAG}\`
- bumps workspace version to \`${VERSION}\`
- aligns skill/plugin metadata to \`${VERSION}\`
- merge triggers .github/workflows/release.yml, which creates the tag and
  publishes the release
- GitHub release notes come from the committed \`CHANGELOG.md\` entry
EOF
)

pr_url="$(
  gh pr create \
    --base main \
    --head "${BRANCH}" \
    --title "chore(release): ${TAG}" \
    --body "${PR_BODY}"
)"

if [[ "$auto_merge" -eq 1 ]]; then
  gh pr merge --auto --squash --delete-branch "$pr_url" || {
    echo "error: failed to enable auto-merge; verify repository auto-merge support or rerun with --no-auto-merge" >&2
    exit 1
  }
fi

echo ""
echo "Prepared ${TAG}"
echo "Release branch: ${BRANCH}"
echo "Pull request: ${pr_url}"
if [[ "$auto_merge" -eq 1 ]]; then
  echo "Auto-merge: enabled (squash)"
else
  echo "Auto-merge: not enabled"
fi

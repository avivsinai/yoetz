#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXT_DIR="$ROOT/extensions/chatgpt-native"
DIST_DIR="$EXT_DIR/dist"
ZIP_PATH="$DIST_DIR/yoetz-chatgpt-native-extension.zip"
MANIFEST="$EXT_DIR/manifest.json"
CHECK_ONLY=0

if [[ "${1:-}" == "--check" ]]; then
  CHECK_ONLY=1
elif [[ $# -gt 0 ]]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi

if [[ ! -f "$MANIFEST" ]]; then
  echo "missing manifest: $MANIFEST" >&2
  exit 1
fi

node --input-type=module - "$MANIFEST" <<'NODE'
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";

const manifestPath = process.argv.at(-1);
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const extensionDir = dirname(manifestPath);
const root = join(dirname(manifestPath), "../..");
const cargoToml = readFileSync(join(root, "Cargo.toml"), "utf8");
const workspaceVersion = cargoToml.match(/\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1];
const requiredPermissions = ["alarms", "nativeMessaging", "storage"];
const expectedOptionalPermissions = new Set(["identity.email", "tabGroups"]);
const forbidden = new Set(["tabs", "debugger", "history", "bookmarks", "downloads", "identity", "<all_urls>", "file:///*"]);
const permissions = manifest.permissions ?? [];
const optionalPermissions = manifest.optional_permissions ?? [];
const hostPermissions = manifest.host_permissions ?? [];
const expectedIcons = {
  "16": "icons/icon-16.png",
  "32": "icons/icon-32.png",
  "48": "icons/icon-48.png",
  "128": "icons/icon-128.png",
};

if (manifest.manifest_version !== 3) {
  throw new Error("manifest_version must be 3");
}
if (manifest.minimum_chrome_version !== "120") {
  throw new Error("minimum_chrome_version must be 120 for 30-second MV3 alarm cadence");
}
if (!workspaceVersion) {
  throw new Error("could not read workspace version from Cargo.toml");
}
if (manifest.version !== workspaceVersion) {
  throw new Error(`manifest version ${manifest.version} must match workspace version ${workspaceVersion}`);
}
for (const permission of requiredPermissions) {
  if (!permissions.includes(permission)) {
    throw new Error(`missing required permission: ${permission}`);
  }
}
if (JSON.stringify(hostPermissions) !== JSON.stringify(["https://chatgpt.com/*"])) {
  throw new Error(`host_permissions must be exactly ["https://chatgpt.com/*"], got ${JSON.stringify(hostPermissions)}`);
}
if (
  optionalPermissions.length !== expectedOptionalPermissions.size
  || !optionalPermissions.every((permission) => expectedOptionalPermissions.has(permission))
) {
  throw new Error(
    `optional_permissions must contain exactly ${JSON.stringify([...expectedOptionalPermissions])}, got ${JSON.stringify(optionalPermissions)}`
  );
}
if (permissions.includes("identity.email")) {
  throw new Error("identity.email must live in optional_permissions, not required permissions");
}
for (const permission of [...permissions, ...optionalPermissions, ...hostPermissions]) {
  if (forbidden.has(permission)) {
    throw new Error(`forbidden permission requested: ${permission}`);
  }
}
if (!manifest.key || typeof manifest.key !== "string") {
  throw new Error("manifest key is required for stable unpacked/release identity");
}
for (const [size, path] of Object.entries(expectedIcons)) {
  if (manifest.icons?.[size] !== path) {
    throw new Error(`manifest icons.${size} must be ${path}`);
  }
  if (manifest.action?.default_icon?.[size] !== path) {
    throw new Error(`manifest action.default_icon.${size} must be ${path}`);
  }
  readFileSync(join(extensionDir, path));
}
for (const path of [
  "native-host-manifest.template.json",
  manifest.action?.default_popup,
  manifest.background?.service_worker,
  ...(manifest.content_scripts ?? []).flatMap((entry) => entry.js ?? []),
  ...(manifest.web_accessible_resources ?? []).flatMap((entry) => entry.resources ?? [])
]) {
  if (!path || typeof path !== "string") {
    throw new Error("manifest references an empty package path");
  }
  if (path.startsWith("/") || path.includes("..")) {
    throw new Error(`manifest references unsafe package path: ${path}`);
  }
  readFileSync(join(extensionDir, path));
}
const hash = createHash("sha256").update(Buffer.from(manifest.key, "base64")).digest().subarray(0, 16);
const actualId = [...hash]
  .map((byte) => String.fromCharCode(97 + (byte >> 4)) + String.fromCharCode(97 + (byte & 15)))
  .join("");
const expectedId = "njdakhppfigmloihiikbjmheejfndbfa";
if (actualId !== expectedId) {
  throw new Error(`manifest key derives extension id ${actualId}, expected ${expectedId}`);
}
NODE

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

build_zip() {
  local zip_path="$1"
  local package_dir="$TMP_DIR/package"

  rm -rf "$package_dir"
  mkdir -p "$package_dir"
  cp "$EXT_DIR/manifest.json" "$package_dir/"
  cp "$EXT_DIR/native-host-manifest.template.json" "$package_dir/"
  cp "$EXT_DIR/popup.html" "$package_dir/"
  cp "$EXT_DIR/popup.js" "$package_dir/"
  cp -R "$EXT_DIR/icons" "$package_dir/icons"
  cp -R "$EXT_DIR/src" "$package_dir/src"

  find "$package_dir" -exec touch -t 198001010000 {} +
  (
    cd "$package_dir"
    find manifest.json native-host-manifest.template.json popup.html popup.js icons src -type f \
      | LC_ALL=C sort \
      | zip -X -q "$zip_path" -@
  )
}

verify_zip() {
  local zip_path="$1"
  python3 - "$zip_path" <<'PY'
import sys
import zipfile

zip_path = sys.argv[1]
required = {
    "manifest.json",
    "native-host-manifest.template.json",
    "popup.html",
    "popup.js",
    "icons/icon-16.png",
    "icons/icon-32.png",
    "icons/icon-48.png",
    "icons/icon-128.png",
    "src/chatgpt-dom.js",
    "src/chunks.js",
    "src/content-script.js",
    "src/protocol.js",
    "src/service-worker.js",
}
with zipfile.ZipFile(zip_path) as archive:
    names = archive.namelist()
    files = set(names)

if names != sorted(names):
    raise SystemExit("extension zip entries are not sorted deterministically")
missing = sorted(required - files)
if missing:
    raise SystemExit(f"extension zip is missing required files: {missing}")
for name in names:
    if name.startswith("/") or "/../" in f"/{name}/" or name.endswith("/"):
        raise SystemExit(f"extension zip contains unsafe entry: {name}")
    if name == "package.json" or name.startswith("tests/") or name.startswith("dist/"):
        raise SystemExit(f"extension zip contains non-release entry: {name}")
print(f"verified {len(names)} extension package files")
PY
}

if [[ "$CHECK_ONLY" == "1" ]]; then
  check_zip="$TMP_DIR/yoetz-chatgpt-native-extension.zip"
  build_zip "$check_zip"
  verify_zip "$check_zip"
  echo "chatgpt-native extension package check passed"
  exit 0
fi

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
build_zip "$ZIP_PATH"
verify_zip "$ZIP_PATH"

echo "$ZIP_PATH"

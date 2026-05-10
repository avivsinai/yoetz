import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { EXTENSION_ID } from "../src/protocol.js";

const manifest = JSON.parse(await readFile(new URL("../manifest.json", import.meta.url), "utf8"));
const nativeHostManifest = JSON.parse(await readFile(new URL("../native-host-manifest.template.json", import.meta.url), "utf8"));

test("manifest is MV3 and scoped only to ChatGPT", () => {
  assert.equal(manifest.manifest_version, 3);
  assert.equal(manifest.minimum_chrome_version, "120");
  assert.deepEqual(manifest.host_permissions, ["https://chatgpt.com/*"]);
  assert.deepEqual(manifest.content_scripts[0].matches, ["https://chatgpt.com/*"]);
});

test("manifest declares the required narrow permission set", () => {
  assert.deepEqual(new Set(manifest.permissions), new Set(["alarms", "nativeMessaging", "storage"]));
  assert.deepEqual(new Set(manifest.optional_permissions), new Set(["identity.email", "tabGroups"]));
});

test("identity.email is optional, not required, to avoid the install-time email warning", () => {
  assert.equal(
    manifest.permissions.includes("identity.email"),
    false,
    "identity.email must not appear in required permissions"
  );
  assert.equal(
    manifest.optional_permissions.includes("identity.email"),
    true,
    "identity.email must appear in optional_permissions"
  );
});

test("manifest does not request the broader 'identity' permission", () => {
  // The narrow `identity.email` scope is the maximum we ever need; the broad
  // `identity` permission would unlock additional account scopes (OAuth tokens)
  // that yoetz must never request.
  assert.equal(manifest.permissions.includes("identity"), false);
  assert.equal((manifest.optional_permissions ?? []).includes("identity"), false);
});

test("manifest does not request forbidden permissions", () => {
  const forbidden = new Set([
    "tabs",
    "debugger",
    "history",
    "bookmarks",
    "downloads",
    "identity",
    "<all_urls>",
    "file:///*"
  ]);
  const requested = [
    ...(manifest.permissions ?? []),
    ...(manifest.optional_permissions ?? []),
    ...(manifest.host_permissions ?? [])
  ];
  for (const permission of requested) {
    assert.equal(forbidden.has(permission), false, `${permission} is forbidden`);
  }
});

test("native host template pins the expected extension origin", () => {
  assert.ok(manifest.key, "manifest key is required for stable dev identity");
  assert.deepEqual(nativeHostManifest.allowed_origins, [`chrome-extension://${EXTENSION_ID}/`]);
});

test("manifest key derives the pinned Chrome extension id", () => {
  const hash = createHash("sha256")
    .update(Buffer.from(manifest.key, "base64"))
    .digest()
    .subarray(0, 16);
  const actualId = [...hash]
    .map((byte) => String.fromCharCode(97 + (byte >> 4)) + String.fromCharCode(97 + (byte & 15)))
    .join("");
  assert.equal(actualId, EXTENSION_ID);
});

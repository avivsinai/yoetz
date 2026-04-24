import { readFile, rm, mkdtemp } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const sourceDir = path.dirname(fileURLToPath(import.meta.url));
const entryPoint = path.join(sourceDir, "daemon.ts");
const committedBundle = path.resolve(sourceDir, "../live-cdp-daemon.mjs");
const check = process.argv.includes("--check");

const tempDir = check ? await mkdtemp(path.join(os.tmpdir(), "yoetz-live-cdp-build-")) : null;
const outfile = tempDir ? path.join(tempDir, "live-cdp-daemon.mjs") : committedBundle;

try {
  await build({
    entryPoints: [entryPoint],
    bundle: true,
    platform: "node",
    format: "esm",
    target: "node22",
    outfile,
    logLevel: "silent",
    sourcemap: false,
    legalComments: "none",
  });

  if (check) {
    const [expected, actual] = await Promise.all([
      readFile(committedBundle, "utf8"),
      readFile(outfile, "utf8"),
    ]);
    if (actual !== expected) {
      console.error(
        "crates/yoetz-cli/assets/live-cdp-daemon.mjs is stale; run ./scripts/build-live-cdp-daemon.sh"
      );
      process.exitCode = 1;
    }
  }
} finally {
  if (tempDir) {
    await rm(tempDir, { recursive: true, force: true });
  }
}

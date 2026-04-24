import { createHash } from "node:crypto";
import { constants } from "node:fs";
import {
  chmod,
  lstat,
  mkdir,
  open,
  readFile,
  unlink,
  writeFile as writeFileFs,
} from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { inspect } from "node:util";
import vm from "node:vm";
import {
  createLiveCdpBrowser,
  getLiveCdpPageTargetId,
  type LiveCdpBrowser,
  type LiveCdpPage,
} from "./live-cdp-browser.js";

type JsonRecord = Record<string, unknown>;

type Request =
  | {
      id: string;
      type: "execute";
      browser?: string;
      script: string;
      connect?: string;
      timeoutMs?: number;
      version?: string;
    }
  | { id: string; type: "status"; version?: string }
  | { id: string; type: "browsers" }
  | { id: string; type: "browser-stop"; browser: string }
  | { id: string; type: "install" }
  | { id: string; type: "stop" };

type Response =
  | { id: string; type: "stdout"; data: string }
  | { id: string; type: "stderr"; data: string }
  | { id: string; type: "complete"; success: true }
  | { id: string; type: "error"; message: string }
  | { id: string; type: "result"; data: unknown };

type BrowserEntry = {
  name: string;
  browser: LiveCdpBrowser;
  context: Awaited<ReturnType<LiveCdpBrowser["newContext"]>>;
  endpoint: string;
  pages: Map<string, LiveCdpPage>;
};

type PageSummary = {
  id: string;
  url: string;
  title: string;
  name: string | null;
};

const BASE_DIR = path.join(os.homedir(), ".yoetz");
const SOCKET_PATH =
  process.platform === "win32"
    ? `\\\\.\\pipe\\yoetz-live-cdp-daemon-${sanitizePipeSegment(
        process.env.USERNAME || process.env.USER || os.userInfo().username || "user"
      )}`
    : path.join(BASE_DIR, "live-cdp-daemon.sock");
const PID_PATH = path.join(BASE_DIR, "live-cdp-daemon.pid");
const DEV_BROWSER_TMP_DIR = path.join(os.homedir(), ".dev-browser", "tmp");
const DISCOVERY_PORTS = [9222, 9223, 9224, 9225, 9226, 9227, 9228, 9229];
const PROBE_TIMEOUT_MS = 750;
const MANUAL_CONNECT_TIMEOUT_MS = 5_000;
const DEFAULT_SCRIPT_TIMEOUT_MS = 30_000;
const PAGE_TITLE_TIMEOUT_MS = 1_500;
const SOCKET_CLOSE_TIMEOUT_MS = 500;
const TARGET_ID_PATTERN = /^[a-f0-9]{16,}$/i;
const SAFE_PATH_SEGMENT_PATTERN = /[^A-Za-z0-9._-]/g;
const NOFOLLOW_FLAG = constants.O_NOFOLLOW ?? 0;
const YOETZ_DAEMON_VERSION = await computeDaemonVersion();
const startedAt = Date.now();

const clients = new Set<net.Socket>();
let manager: LiveCdpBrowserManager;
let server: net.Server | null = null;
let shuttingDown: Promise<void> | null = null;

if (process.platform !== "win32") {
  process.umask(0o077);
}

if (process.argv.includes("--self-test")) {
  if (typeof globalThis.WebSocket !== "function") {
    throw new Error("Node.js runtime does not expose global WebSocket");
  }
  process.stdout.write("yoetz live-cdp daemon ok\n");
  process.exit(0);
}

async function computeDaemonVersion(): Promise<string> {
  const source = await readFile(fileURLToPath(import.meta.url));
  return createHash("sha256").update(source).digest("hex");
}

class LiveCdpBrowserManager {
  readonly #browsers = new Map<string, BrowserEntry>();

  async connectBrowser(name: string, endpoint: string | undefined): Promise<BrowserEntry> {
    const resolved = await this.resolveEndpoint(endpoint || "auto");
    const existing = this.#browsers.get(name);
    if (existing?.endpoint === resolved && existing.browser.isConnected()) {
      return existing;
    }
    if (existing) {
      await this.stopBrowser(name);
    }

    const browser = await createLiveCdpBrowser(resolved);
    const context = browser.contexts()[0] ?? (await browser.newContext());
    const entry: BrowserEntry = {
      name,
      browser,
      context,
      endpoint: resolved,
      pages: new Map(),
    };
    browser.on("disconnected", () => {
      const current = this.#browsers.get(name);
      if (current === entry) {
        entry.pages.clear();
        this.#browsers.delete(name);
      }
    });
    this.#browsers.set(name, entry);
    return entry;
  }

  async getPage(browserName: string, pageNameOrId: string): Promise<LiveCdpPage> {
    const entry = this.getBrowserEntry(browserName);
    const existing = entry.pages.get(pageNameOrId);
    if (existing && !existing.isClosed()) {
      return existing;
    }
    entry.pages.delete(pageNameOrId);

    if (TARGET_ID_PATTERN.test(pageNameOrId)) {
      const page = await this.findPageByTargetId(entry, pageNameOrId);
      if (page) {
        return page;
      }
    }

    const page = await entry.context.newPage();
    this.registerNamedPage(entry, pageNameOrId, page);
    return page;
  }

  async newPage(browserName: string): Promise<LiveCdpPage> {
    return await this.getBrowserEntry(browserName).context.newPage();
  }

  async listPages(browserName: string): Promise<PageSummary[]> {
    const entry = this.#browsers.get(browserName);
    if (!entry || !entry.browser.isConnected()) {
      return [];
    }

    await entry.browser.refreshPages();
    this.pruneClosedPages(entry);
    const namesByPage = this.namedPagesByPage(entry);
    const summaries: PageSummary[] = [];
    for (const page of entry.context.pages()) {
      if (page.isClosed()) {
        continue;
      }
      summaries.push({
        id: getLiveCdpPageTargetId(page) ?? "",
        url: page.url(),
        title: await titleWithTimeout(page),
        name: namesByPage.get(page) ?? null,
      });
    }
    return summaries.filter((page) => page.id.length > 0);
  }

  async closePage(browserName: string, pageName: string): Promise<void> {
    const entry = this.getBrowserEntry(browserName);
    const page = entry.pages.get(pageName);
    if (!page || page.isClosed()) {
      entry.pages.delete(pageName);
      throw new Error(`Page "${browserName}/${pageName}" not found`);
    }
    entry.pages.delete(pageName);
    await page.close();
  }

  async stopBrowser(name: string): Promise<void> {
    const entry = this.#browsers.get(name);
    if (!entry) {
      return;
    }
    this.#browsers.delete(name);
    entry.pages.clear();
    await entry.browser.close().catch(() => undefined);
  }

  async stopAll(): Promise<void> {
    const names = Array.from(this.#browsers.keys());
    await Promise.allSettled(names.map((name) => this.stopBrowser(name)));
  }

  browserCount(): number {
    return this.#browsers.size;
  }

  listBrowsers(): Array<{ name: string; type: "connected"; status: string; pages: string[] }> {
    return Array.from(this.#browsers.values())
      .map((entry) => {
        this.pruneClosedPages(entry);
        return {
          name: entry.name,
          type: "connected" as const,
          status: entry.browser.isConnected() ? "connected" : "disconnected",
          pages: Array.from(entry.pages.keys()).sort((left, right) => left.localeCompare(right)),
        };
      })
      .sort((left, right) => left.name.localeCompare(right.name));
  }

  private getBrowserEntry(name: string): BrowserEntry {
    const entry = this.#browsers.get(name);
    if (!entry || !entry.browser.isConnected()) {
      throw new Error(`Browser "${name}" is not connected`);
    }
    return entry;
  }

  private async resolveEndpoint(endpoint: string): Promise<string> {
    if (endpoint === "auto") {
      const discovered = await this.discoverChrome();
      if (discovered) {
        return discovered;
      }
      throw new Error(buildAutoConnectError());
    }

    if (isHttpEndpoint(endpoint)) {
      const resolved = await this.resolveHttpEndpoint(endpoint, MANUAL_CONNECT_TIMEOUT_MS);
      if (resolved) {
        return resolved;
      }
      throw new Error(buildManualConnectError(endpoint));
    }

    if (!isWebSocketEndpoint(endpoint)) {
      throw new Error(`Unsupported live-CDP endpoint: ${endpoint}`);
    }
    return endpoint;
  }

  private async discoverChrome(): Promise<string | null> {
    const activePort = await readDevToolsActivePort();
    if (activePort) {
      return activePort;
    }
    for (const port of DISCOVERY_PORTS) {
      const endpoint = `http://127.0.0.1:${port}`;
      const result = await fetchDebuggerWebSocketUrl(endpoint, PROBE_TIMEOUT_MS);
      if (result.status === "ok") {
        return result.webSocketDebuggerUrl;
      }
      if (result.status === "not-found") {
        const activePortEndpoint = await readDevToolsActivePort(port);
        if (activePortEndpoint) {
          return activePortEndpoint;
        }
      }
    }
    return null;
  }

  private async resolveHttpEndpoint(endpoint: string, timeoutMs: number): Promise<string | null> {
    const result = await fetchDebuggerWebSocketUrl(endpoint, timeoutMs);
    if (result.status === "ok") {
      return result.webSocketDebuggerUrl;
    }
    if (result.status === "not-found") {
      const port = endpointPort(endpoint);
      return port === null ? null : await readDevToolsActivePort(port);
    }
    return null;
  }

  private async findPageByTargetId(
    entry: BrowserEntry,
    targetId: string
  ): Promise<LiveCdpPage | null> {
    await entry.browser.refreshPages();
    return entry.context.pages().find((page) => getLiveCdpPageTargetId(page) === targetId) ?? null;
  }

  private registerNamedPage(entry: BrowserEntry, name: string, page: LiveCdpPage): void {
    entry.pages.set(name, page);
    page.on("close", () => {
      if (entry.pages.get(name) === page) {
        entry.pages.delete(name);
      }
    });
  }

  private pruneClosedPages(entry: BrowserEntry): void {
    for (const [name, page] of entry.pages.entries()) {
      if (page.isClosed()) {
        entry.pages.delete(name);
      }
    }
  }

  private namedPagesByPage(entry: BrowserEntry): Map<LiveCdpPage, string> {
    const names = new Map<LiveCdpPage, string>();
    for (const [name, page] of entry.pages.entries()) {
      if (!page.isClosed() && !names.has(page)) {
        names.set(page, name);
      }
    }
    return names;
  }
}

async function runScript(
  script: string,
  browserName: string,
  output: ReturnType<typeof createMessageQueue>,
  requestId: string,
  timeoutMs: number
): Promise<void> {
  const browserApi = createBrowserApi(browserName);
  const consoleApi = {
    log: (...args: unknown[]) => output.push({ id: requestId, type: "stdout", data: formatArgs(args) }),
    info: (...args: unknown[]) => output.push({ id: requestId, type: "stdout", data: formatArgs(args) }),
    warn: (...args: unknown[]) => output.push({ id: requestId, type: "stderr", data: formatArgs(args) }),
    error: (...args: unknown[]) => output.push({ id: requestId, type: "stderr", data: formatArgs(args) }),
  };
  const context = vm.createContext(
    // node:vm is not a security boundary; recipe scripts run with yoetz user
    // privileges by design and already hold CDP access to the user's browser.
    {
      browser: browserApi,
      console: consoleApi,
      setTimeout,
      clearTimeout,
      saveScreenshot,
      writeFile: writeDevBrowserTempFile,
      readFile: readDevBrowserTempFile,
      Uint8Array,
      URL,
    },
    {
      name: "yoetz-live-cdp-script",
    }
  );
  const compiled = new vm.Script(`"use strict";\n(async () => {\n${script}\n})()`, {
    filename: "yoetz-live-cdp-script.js",
  });

  await withTimeout(
    Promise.resolve(compiled.runInContext(context, { timeout: timeoutMs })),
    timeoutMs,
    "ScriptTimeoutError"
  );
}

function createBrowserApi(browserName: string) {
  return {
    getPage: (nameOrId: string) => manager.getPage(browserName, String(nameOrId)),
    newPage: () => manager.newPage(browserName),
    listPages: () => manager.listPages(browserName),
    closePage: (name: string) => manager.closePage(browserName, String(name)),
  };
}

async function handleExecute(socket: net.Socket, request: Extract<Request, { type: "execute" }>) {
  if (request.version && request.version !== YOETZ_DAEMON_VERSION) {
    await writeMessage(socket, {
      id: request.id,
      type: "error",
      message: `Daemon version mismatch: running ${YOETZ_DAEMON_VERSION}, client expected ${request.version}`,
    });
    return;
  }
  const browserName = request.browser || "default";
  await manager.connectBrowser(browserName, request.connect);
  const output = createMessageQueue(socket);
  try {
    await runScript(
      request.script,
      browserName,
      output,
      request.id,
      request.timeoutMs ?? DEFAULT_SCRIPT_TIMEOUT_MS
    );
    await output.drain();
    await writeMessage(socket, { id: request.id, type: "complete", success: true });
  } catch (error) {
    await output.drain().catch(() => undefined);
    await writeMessage(socket, { id: request.id, type: "error", message: formatError(error) });
  }
}

async function handleRequest(socket: net.Socket, line: string): Promise<void> {
  let request: Request;
  try {
    request = parseRequest(line);
  } catch (error) {
    await writeMessage(socket, {
      id: "unknown",
      type: "error",
      message: error instanceof Error ? error.message : String(error),
    });
    return;
  }

  if (shuttingDown && request.type !== "stop") {
    await writeMessage(socket, { id: request.id, type: "error", message: "Daemon is shutting down" });
    return;
  }

  switch (request.type) {
    case "execute":
      await handleExecute(socket, request);
      break;
    case "browsers":
      await writeMessage(socket, { id: request.id, type: "result", data: manager.listBrowsers() });
      await writeMessage(socket, { id: request.id, type: "complete", success: true });
      break;
    case "browser-stop":
      await manager.stopBrowser(request.browser);
      await writeMessage(socket, {
        id: request.id,
        type: "result",
        data: { browser: request.browser, stopped: true },
      });
      await writeMessage(socket, { id: request.id, type: "complete", success: true });
      break;
    case "status":
      await writeMessage(socket, {
        id: request.id,
        type: "result",
        data: {
          pid: process.pid,
          version: YOETZ_DAEMON_VERSION,
          expectedVersion: request.version ?? null,
          versionMatches: request.version ? request.version === YOETZ_DAEMON_VERSION : null,
          uptimeMs: Date.now() - startedAt,
          browserCount: manager.browserCount(),
          socketPath: SOCKET_PATH,
          browsers: manager.listBrowsers(),
        },
      });
      await writeMessage(socket, { id: request.id, type: "complete", success: true });
      break;
    case "install":
      await writeMessage(socket, { id: request.id, type: "complete", success: true });
      break;
    case "stop":
      await writeMessage(socket, { id: request.id, type: "result", data: { stopping: true } });
      await writeMessage(socket, { id: request.id, type: "complete", success: true });
      shuttingDown = shutdown();
      break;
  }
}

function parseRequest(line: string): Request {
  const value = JSON.parse(line) as JsonRecord;
  const id = typeof value.id === "string" && value.id.length > 0 ? value.id : undefined;
  const type = typeof value.type === "string" ? value.type : undefined;
  if (!id || !type) {
    throw new Error("Request must include string id and type");
  }

  if (type === "execute") {
    if (typeof value.script !== "string") {
      throw new Error("execute request must include script");
    }
    return {
      id,
      type,
      browser: typeof value.browser === "string" ? value.browser : undefined,
      script: value.script,
      connect: typeof value.connect === "string" ? value.connect : undefined,
      timeoutMs:
        typeof value.timeoutMs === "number" && Number.isFinite(value.timeoutMs)
          ? Math.max(1, Math.trunc(value.timeoutMs))
          : undefined,
      version: typeof value.version === "string" ? value.version : undefined,
    };
  }

  if (type === "browser-stop") {
    if (typeof value.browser !== "string" || value.browser.length === 0) {
      throw new Error("browser-stop request must include browser");
    }
    return { id, type, browser: value.browser };
  }

  if (type === "status") {
    return { id, type, version: typeof value.version === "string" ? value.version : undefined };
  }

  if (type === "browsers" || type === "install" || type === "stop") {
    return { id, type };
  }

  throw new Error(`Unsupported request type: ${type}`);
}

function createMessageQueue(socket: net.Socket) {
  let queue = Promise.resolve();
  return {
    push(message: Response): Promise<void> {
      queue = queue.then(() => writeMessage(socket, message)).catch(() => undefined);
      return queue;
    },
    async drain(): Promise<void> {
      await queue;
    },
  };
}

async function writeMessage(socket: net.Socket, message: Response): Promise<void> {
  if (socket.destroyed) {
    return;
  }
  const payload = { version: YOETZ_DAEMON_VERSION, ...message };
  await new Promise<void>((resolve, reject) => {
    socket.write(`${JSON.stringify(payload)}\n`, (error?: Error | null) => {
      if (error) {
        reject(error);
      } else {
        resolve();
      }
    });
  });
}

async function assertNoRunningDaemonFromPidFile(): Promise<void> {
  let contents: string;
  try {
    contents = await readFile(PID_PATH, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return;
    }
    throw error;
  }

  const pid = Number.parseInt(contents.trim(), 10);
  if (!Number.isFinite(pid) || pid <= 0 || pid === process.pid) {
    return;
  }
  if (processIsAlive(pid)) {
    throw new Error(`yoetz live-cdp daemon pid ${pid} is already running`);
  }
}

function processIsAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

async function startServer(): Promise<void> {
  await mkdir(BASE_DIR, { recursive: true, mode: 0o700 });
  if (process.platform !== "win32") {
    await chmod(BASE_DIR, 0o700).catch(() => undefined);
  }
  await assertNoRunningDaemonFromPidFile();
  if (process.platform !== "win32") {
    await unlink(SOCKET_PATH).catch((error) => {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
        throw error;
      }
    });
  }

  server = net.createServer((socket) => {
    if (shuttingDown) {
      socket.end();
      return;
    }

    clients.add(socket);
    socket.setEncoding("utf8");
    let buffer = "";
    let queue = Promise.resolve();
    socket.on("data", (chunk: string) => {
      buffer += chunk;
      for (;;) {
        const newline = buffer.indexOf("\n");
        if (newline < 0) {
          break;
        }
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (!line) {
          continue;
        }
        queue = queue
          .then(() => handleRequest(socket, line))
          .catch(async (error) => {
            if (!socket.destroyed) {
              await writeMessage(socket, {
                id: "unknown",
                type: "error",
                message: formatError(error),
              });
            }
          });
      }
    });
    socket.on("close", () => clients.delete(socket));
    socket.on("error", () => clients.delete(socket));
  });

  await new Promise<void>((resolve, reject) => {
    server!.once("error", reject);
    server!.listen(SOCKET_PATH, () => {
      server!.off("error", reject);
      resolve();
    });
  });

  if (process.platform !== "win32") {
    await chmod(SOCKET_PATH, 0o600);
  }
  await writeFileFs(PID_PATH, `${process.pid}\n`, { mode: 0o600 });
  process.stderr.write("yoetz live-cdp daemon ready\n");
}

async function shutdown(): Promise<void> {
  const serverToClose = server;
  server = null;
  await manager.stopAll().catch(() => undefined);
  if (serverToClose) {
    await new Promise<void>((resolve) => serverToClose.close(() => resolve()));
  }
  await Promise.allSettled(Array.from(clients, (socket) => closeClientSocket(socket)));
  await unlink(PID_PATH).catch(() => undefined);
  if (process.platform !== "win32") {
    await unlink(SOCKET_PATH).catch(() => undefined);
  }
  setImmediate(() => process.exit(0));
}

async function closeClientSocket(socket: net.Socket): Promise<void> {
  if (socket.destroyed) {
    return;
  }
  await new Promise<void>((resolve) => {
    const timeout = setTimeout(() => {
      if (!socket.destroyed) {
        socket.destroy();
      }
    }, SOCKET_CLOSE_TIMEOUT_MS);
    timeout.unref();
    const finish = () => {
      clearTimeout(timeout);
      resolve();
    };
    socket.once("close", finish);
    socket.once("error", finish);
    socket.end();
  });
}

async function fetchDebuggerWebSocketUrl(
  endpoint: string,
  timeoutMs: number
): Promise<{ status: "ok"; webSocketDebuggerUrl: string } | { status: "not-found" | "unavailable" }> {
  try {
    const response = await fetch(toJsonVersionUrl(endpoint), {
      headers: { accept: "application/json" },
      signal: AbortSignal.timeout(timeoutMs),
    });
    if (response.status === 404) {
      return { status: "not-found" };
    }
    if (!response.ok) {
      return { status: "unavailable" };
    }
    const payload = (await response.json()) as { webSocketDebuggerUrl?: unknown };
    return typeof payload.webSocketDebuggerUrl === "string" && payload.webSocketDebuggerUrl.length > 0
      ? { status: "ok", webSocketDebuggerUrl: payload.webSocketDebuggerUrl }
      : { status: "unavailable" };
  } catch {
    return { status: "unavailable" };
  }
}

function toJsonVersionUrl(endpoint: string): URL {
  const url = new URL(endpoint);
  if (url.pathname !== "/json/version") {
    url.pathname = "/json/version";
    url.search = "";
    url.hash = "";
  }
  return url;
}

async function readDevToolsActivePort(expectedPort?: number): Promise<string | null> {
  for (const candidate of devToolsActivePortCandidates()) {
    let contents: string;
    try {
      contents = await readTextFile(candidate);
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (code === "ENOENT" || code === "ENOTDIR" || code === "EACCES") {
        continue;
      }
      throw error;
    }

    const endpoint = parseDevToolsActivePort(contents, expectedPort);
    if (endpoint) {
      return endpoint;
    }
  }
  return null;
}

async function readTextFile(filePath: string): Promise<string> {
  const handle = await open(filePath, constants.O_RDONLY);
  try {
    return await handle.readFile({ encoding: "utf8" });
  } finally {
    await handle.close();
  }
}

function devToolsActivePortCandidates(): string[] {
  const home = os.homedir();
  switch (process.platform) {
    case "darwin":
      return [
        path.join(home, "Library", "Application Support", "Google", "Chrome", "DevToolsActivePort"),
        path.join(home, "Library", "Application Support", "Google", "Chrome Canary", "DevToolsActivePort"),
        path.join(home, "Library", "Application Support", "Chromium", "DevToolsActivePort"),
        path.join(home, "Library", "Application Support", "BraveSoftware", "Brave-Browser", "DevToolsActivePort"),
      ];
    case "linux":
      return [
        path.join(home, ".config", "google-chrome", "DevToolsActivePort"),
        path.join(home, ".config", "chromium", "DevToolsActivePort"),
        path.join(home, ".config", "google-chrome-beta", "DevToolsActivePort"),
        path.join(home, ".config", "google-chrome-unstable", "DevToolsActivePort"),
        path.join(home, ".config", "BraveSoftware", "Brave-Browser", "DevToolsActivePort"),
      ];
    case "win32":
      return [
        path.join(home, "AppData", "Local", "Google", "Chrome", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "Google", "Chrome Beta", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "Google", "Chrome SxS", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "Chromium", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "BraveSoftware", "Brave-Browser", "User Data", "DevToolsActivePort"),
      ];
    default:
      return [];
  }
}

function parseDevToolsActivePort(contents: string, expectedPort?: number): string | null {
  const lines = contents
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const port = Number.parseInt(lines[0] ?? "", 10);
  const webSocketPath = lines[1] ?? "";
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    return null;
  }
  if (expectedPort !== undefined && port !== expectedPort) {
    return null;
  }
  if (!webSocketPath.startsWith("/devtools/browser/")) {
    return null;
  }
  return `ws://127.0.0.1:${port}${webSocketPath}`;
}

async function saveScreenshot(data: unknown, name: string): Promise<string> {
  if (data instanceof Uint8Array) {
    return await writeDevBrowserTempFile(name, data);
  }
  if (typeof data === "string") {
    return await writeDevBrowserTempFile(name, data);
  }
  throw new TypeError("saveScreenshot data must be a string or Uint8Array");
}

async function writeDevBrowserTempFile(fileName: unknown, data: string | Uint8Array): Promise<string> {
  const destination = await resolveDevBrowserTempPath(fileName, true);
  await assertDestinationIsNotSymlink(destination);
  let handle: import("node:fs/promises").FileHandle | undefined;
  try {
    handle = await open(
      destination,
      constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC | NOFOLLOW_FLAG,
      0o600
    );
    await handle.writeFile(data);
  } catch (error) {
    throw normalizeSymlinkError(error, destination);
  } finally {
    await handle?.close();
  }
  return destination;
}

async function readDevBrowserTempFile(fileName: unknown): Promise<string> {
  const destination = await resolveDevBrowserTempPath(fileName, false);
  await assertDestinationIsNotSymlink(destination);
  let handle: import("node:fs/promises").FileHandle | undefined;
  try {
    handle = await open(destination, constants.O_RDONLY | NOFOLLOW_FLAG);
    return await handle.readFile({ encoding: "utf8" });
  } catch (error) {
    throw normalizeSymlinkError(error, destination);
  } finally {
    await handle?.close();
  }
}

async function resolveDevBrowserTempPath(fileName: unknown, createParents: boolean): Promise<string> {
  await mkdir(DEV_BROWSER_TMP_DIR, { recursive: true });
  await assertControlledDirectory(path.dirname(DEV_BROWSER_TMP_DIR), "Dev Browser base directory");
  await assertControlledDirectory(DEV_BROWSER_TMP_DIR, "Dev Browser temp directory");
  const segments = sanitizeRelativePath(fileName);
  const destination = path.resolve(DEV_BROWSER_TMP_DIR, ...segments);
  if (!isWithinDirectory(path.resolve(DEV_BROWSER_TMP_DIR), destination)) {
    throw new Error("Resolved temp file path escapes the controlled temp directory");
  }
  await assertSafeParentDirectories(path.resolve(DEV_BROWSER_TMP_DIR), destination, createParents);
  return destination;
}

function sanitizeRelativePath(fileName: unknown): string[] {
  if (typeof fileName !== "string" || fileName.length === 0) {
    throw new TypeError("File name must be a non-empty string");
  }
  if (fileName.includes("\0") || path.posix.isAbsolute(fileName) || path.win32.isAbsolute(fileName)) {
    throw new Error("Absolute paths and null bytes are not allowed");
  }
  return fileName.replace(/\\/g, "/").split("/").map(sanitizePathSegment);
}

function sanitizePathSegment(segment: string): string {
  if (!segment || segment === "." || segment === ".." || segment.includes("..")) {
    throw new Error("File paths must not contain empty, '.', or '..' segments");
  }
  const sanitized = segment.replace(SAFE_PATH_SEGMENT_PATTERN, "_");
  if (!sanitized || sanitized === "." || sanitized === "..") {
    throw new Error("File paths must resolve to a valid filename");
  }
  return sanitized;
}

async function assertControlledDirectory(directoryPath: string, label: string): Promise<void> {
  const stats = await lstat(directoryPath);
  if (stats.isSymbolicLink()) {
    throw new Error(`${label} must not be a symlink`);
  }
  if (!stats.isDirectory()) {
    throw new Error(`${label} must be a directory`);
  }
}

async function assertSafeParentDirectories(
  rootDir: string,
  destinationPath: string,
  createParents: boolean
): Promise<void> {
  const relativeParent = path.relative(rootDir, path.dirname(destinationPath));
  if (!relativeParent) {
    return;
  }
  let current = rootDir;
  for (const segment of relativeParent.split(path.sep).filter(Boolean)) {
    current = path.join(current, segment);
    if (createParents) {
      await mkdir(current, { recursive: true });
    }
    const stats = await lstat(current);
    if (stats.isSymbolicLink()) {
      throw new Error(`Temp path parent must not be a symlink: ${current}`);
    }
    if (!stats.isDirectory()) {
      throw new Error(`Temp path parent must be a directory: ${current}`);
    }
  }
}

async function assertDestinationIsNotSymlink(destinationPath: string): Promise<void> {
  try {
    const stats = await lstat(destinationPath);
    if (stats.isSymbolicLink()) {
      throw new Error(`Refusing to follow symlinked temp file: ${destinationPath}`);
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
      throw error;
    }
  }
}

function normalizeSymlinkError(error: unknown, destinationPath: string): Error {
  return (error as NodeJS.ErrnoException).code === "ELOOP"
    ? new Error(`Refusing to follow symlinked temp file: ${destinationPath}`)
    : error instanceof Error
      ? error
      : new Error(String(error));
}

function isWithinDirectory(rootDir: string, candidatePath: string): boolean {
  return candidatePath === rootDir || candidatePath.startsWith(rootDir.endsWith(path.sep) ? rootDir : `${rootDir}${path.sep}`);
}

function isHttpEndpoint(endpoint: string): boolean {
  return endpoint.startsWith("http://") || endpoint.startsWith("https://");
}

function isWebSocketEndpoint(endpoint: string): boolean {
  try {
    const url = new URL(endpoint);
    return url.protocol === "ws:" || url.protocol === "wss:";
  } catch {
    return false;
  }
}

function endpointPort(endpoint: string): number | null {
  try {
    const url = new URL(endpoint);
    const raw = url.port || (url.protocol === "https:" ? "443" : url.protocol === "http:" ? "80" : "");
    const port = Number.parseInt(raw, 10);
    return Number.isInteger(port) && port > 0 && port <= 65_535 ? port : null;
  } catch {
    return null;
  }
}

async function titleWithTimeout(page: LiveCdpPage): Promise<string> {
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      page.title(),
      new Promise<string>((resolve) => {
        timeoutId = setTimeout(() => resolve(""), PAGE_TITLE_TIMEOUT_MS);
      }),
    ]);
  } finally {
    if (timeoutId !== undefined) {
      clearTimeout(timeoutId);
    }
  }
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, name: string): Promise<T> {
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<T>((_, reject) => {
        timeoutId = setTimeout(() => {
          const error = new Error(`${name}: exceeded ${timeoutMs}ms`);
          error.name = name;
          reject(error);
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timeoutId !== undefined) {
      clearTimeout(timeoutId);
    }
  }
}

function formatArgs(args: unknown[]): string {
  return `${args
    .map((arg) => {
      if (typeof arg === "string") {
        return arg;
      }
      if (arg instanceof Error) {
        return arg.stack ?? arg.message;
      }
      return inspect(arg, { depth: 4, breakLength: Infinity, colors: false });
    })
    .join(" ")}\n`;
}

function formatError(error: unknown): string {
  return error instanceof Error ? error.stack ?? error.message : String(error);
}

function buildAutoConnectError(): string {
  const launchCommand =
    process.platform === "darwin"
      ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --remote-debugging-port=9222"
      : process.platform === "win32"
        ? "chrome.exe --remote-debugging-port=9222"
        : "google-chrome --remote-debugging-port=9222";
  return [
    "Could not auto-discover a running Chrome instance with remote debugging enabled.",
    "Enable Chrome remote debugging at chrome://inspect/#remote-debugging",
    `or launch Chrome with: ${launchCommand}`,
  ].join("\n");
}

function buildManualConnectError(endpoint: string): string {
  return [
    `Could not resolve a CDP WebSocket endpoint from ${endpoint}.`,
    "If Chrome is using built-in remote debugging, connect with the exact ws://127.0.0.1:<port>/devtools/browser/... URL from DevToolsActivePort.",
  ].join("\n");
}

function sanitizePipeSegment(value: string): string {
  const sanitized = value.replace(/[^A-Za-z0-9._-]/g, "-").replace(/^-+|-+$/g, "").toLowerCase();
  return sanitized || "user";
}

manager = new LiveCdpBrowserManager();
startServer().catch((error) => {
  process.stderr.write(`Failed to start yoetz live-cdp daemon: ${formatError(error)}\n`);
  process.exit(1);
});

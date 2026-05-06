// daemon.ts
import { createHash } from "node:crypto";
import { constants } from "node:fs";
import {
  chmod,
  lstat,
  mkdir,
  open,
  readFile,
  unlink,
  writeFile as writeFileFs
} from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { inspect } from "node:util";
import vm from "node:vm";

// live-cdp-browser.ts
import { EventEmitter } from "node:events";
var LIVE_CDP_BROWSER_INIT_TIMEOUT_MS = 5e3;
var LIVE_CDP_TARGET_INIT_TIMEOUT_MS = 5e3;
var INPUT_FILE_DIAGNOSTIC_LIMIT = 10;
var LiveCdpSetInputFilesError = class extends Error {
  constructor(message, details) {
    super(`${message} ${formatSetInputFilesErrorDetails(details)}`);
    this.details = details;
    this.name = "LiveCdpSetInputFilesError";
  }
};
var WebSocketLiveCdpTransport = class _WebSocketLiveCdpTransport {
  constructor(socket) {
    this.socket = socket;
    this.socket.addEventListener("message", (event) => {
      const data = event.data;
      if (typeof data === "string") {
        this.emitMessage(data);
        return;
      }
      if (data instanceof ArrayBuffer) {
        this.emitMessage(Buffer.from(data).toString("utf8"));
        return;
      }
      if (ArrayBuffer.isView(data)) {
        this.emitMessage(
          Buffer.from(data.buffer, data.byteOffset, data.byteLength).toString("utf8")
        );
      }
    });
    this.socket.addEventListener("close", (event) => {
      const reason = event.reason;
      this.emitClose(typeof reason === "string" && reason.length > 0 ? reason : void 0);
    });
  }
  static async connect(endpoint) {
    const WebSocketConstructor = globalThis.WebSocket;
    if (!WebSocketConstructor) {
      throw new Error("This Node.js runtime does not expose a global WebSocket implementation");
    }
    const socket = new WebSocketConstructor(endpoint);
    return await new Promise((resolve, reject) => {
      let settled = false;
      const timeoutId = setTimeout(() => {
        rejectWith(new Error(`Timed out connecting to CDP endpoint ${endpoint}`));
      }, 5e3);
      const cleanup = () => {
        clearTimeout(timeoutId);
        socket.removeEventListener("open", handleOpen);
        socket.removeEventListener("error", handleError);
        socket.removeEventListener("close", handleClose);
      };
      const rejectWith = (error) => {
        if (settled) {
          return;
        }
        settled = true;
        cleanup();
        try {
          socket.close();
        } catch {
        }
        reject(error);
      };
      const handleOpen = () => {
        if (settled) {
          return;
        }
        settled = true;
        cleanup();
        resolve(new _WebSocketLiveCdpTransport(socket));
      };
      const handleError = (event) => {
        rejectWith(new Error(`Failed to connect to CDP endpoint ${endpoint}: ${String(event)}`));
      };
      const handleClose = () => {
        rejectWith(new Error(`CDP endpoint closed before connection opened: ${endpoint}`));
      };
      socket.addEventListener("open", handleOpen);
      socket.addEventListener("error", handleError);
      socket.addEventListener("close", handleClose, { once: true });
    });
  }
  messageListeners = /* @__PURE__ */ new Set();
  closeListeners = /* @__PURE__ */ new Set();
  send(message) {
    this.socket.send(message);
  }
  close() {
    this.socket.close();
  }
  onMessage(listener) {
    this.messageListeners.add(listener);
    return () => {
      this.messageListeners.delete(listener);
    };
  }
  onClose(listener) {
    this.closeListeners.add(listener);
    return () => {
      this.closeListeners.delete(listener);
    };
  }
  emitMessage(message) {
    for (const listener of this.messageListeners) {
      listener(message);
    }
  }
  emitClose(reason) {
    for (const listener of this.closeListeners) {
      listener(reason);
    }
  }
};
var CdpConnection = class _CdpConnection {
  constructor(transport) {
    this.transport = transport;
    this.transport.onMessage((message) => {
      this.handleMessage(message);
    });
    this.transport.onClose((reason) => {
      this.markClosed(reason);
    });
  }
  static async connect(endpoint, transportFactory = (url) => WebSocketLiveCdpTransport.connect(url)) {
    return new _CdpConnection(await transportFactory(endpoint));
  }
  pending = /* @__PURE__ */ new Map();
  eventListeners = /* @__PURE__ */ new Set();
  nextId = 1;
  closed = false;
  isConnected() {
    return !this.closed;
  }
  send(method, params = {}, sessionId, timeoutMs = 3e4) {
    if (this.closed) {
      return Promise.reject(new Error("CDP connection is closed"));
    }
    const id = this.nextId++;
    const request = { id, method };
    if (Object.keys(params).length > 0) {
      request.params = params;
    }
    if (sessionId) {
      request.sessionId = sessionId;
    }
    const promise = new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP command timed out after ${timeoutMs}ms: ${method}`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timeout });
    });
    try {
      this.transport.send(JSON.stringify(request));
    } catch (error) {
      const pending = this.pending.get(id);
      this.pending.delete(id);
      if (pending) {
        clearTimeout(pending.timeout);
      }
      throw error;
    }
    return promise;
  }
  onEvent(listener) {
    this.eventListeners.add(listener);
    return () => {
      this.eventListeners.delete(listener);
    };
  }
  close(reason) {
    if (this.closed) {
      return;
    }
    this.markClosed(reason);
    this.transport.close();
  }
  handleMessage(message) {
    let payload;
    try {
      payload = JSON.parse(message);
    } catch {
      return;
    }
    if (typeof payload.id === "number") {
      const pending = this.pending.get(payload.id);
      if (!pending) {
        return;
      }
      this.pending.delete(payload.id);
      clearTimeout(pending.timeout);
      if (payload.error) {
        pending.reject(
          new Error(
            payload.error.data ? `${payload.error.message ?? "CDP command failed"}: ${payload.error.data}` : payload.error.message ?? "CDP command failed"
          )
        );
        return;
      }
      pending.resolve(payload.result ?? {});
      return;
    }
    if (typeof payload.method === "string") {
      const event = {
        method: payload.method,
        params: payload.params ?? {},
        sessionId: payload.sessionId
      };
      for (const listener of this.eventListeners) {
        listener(event);
      }
    }
  }
  markClosed(reason) {
    if (this.closed) {
      return;
    }
    this.closed = true;
    const error = new Error(reason ? `CDP connection closed: ${reason}` : "CDP connection closed");
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.pending.clear();
  }
};
var LiveCdpBrowser = class _LiveCdpBrowser extends EventEmitter {
  #connection;
  #context;
  static async connect(endpoint, options = {}) {
    const connection = await CdpConnection.connect(endpoint, options.transportFactory);
    try {
      await sendLiveCdpInitCommand(
        connection,
        "Browser.getVersion",
        {},
        void 0,
        "Browser.getVersion"
      );
      const browser = new _LiveCdpBrowser(connection);
      await browser.refreshPages();
      return browser;
    } catch (error) {
      connection.close("live-CDP initialization failed");
      throw error;
    }
  }
  constructor(connection) {
    super();
    this.#connection = connection;
    this.#context = new LiveCdpBrowserContext(connection);
  }
  contexts() {
    return [this.#context];
  }
  async newContext() {
    return this.#context;
  }
  isConnected() {
    return this.#connection.isConnected();
  }
  async close() {
    this.#connection.close("dev-browser detached");
    await Promise.resolve();
    this.emit("disconnected");
  }
  async refreshPages() {
    await this.#context.refreshPages();
  }
};
var LiveCdpBrowserContext = class {
  constructor(connection) {
    this.connection = connection;
    this.connection.onEvent((event) => {
      this.handleEvent(event);
    });
  }
  #pagesByTargetId = /* @__PURE__ */ new Map();
  #pagesBySessionId = /* @__PURE__ */ new Map();
  pages() {
    return Array.from(this.#pagesByTargetId.values()).filter((page) => !page.isClosed());
  }
  async newPage() {
    const result = await sendLiveCdpInitCommand(
      this.connection,
      "Target.createTarget",
      { url: "about:blank" },
      void 0,
      "Target.createTarget"
    );
    const targetId = typeof result.targetId === "string" ? result.targetId : void 0;
    if (!targetId) {
      throw new Error("Target.createTarget did not return a targetId");
    }
    return await this.attachToTarget({
      targetId,
      type: "page",
      url: "about:blank",
      title: ""
    });
  }
  async close() {
    await Promise.allSettled(this.pages().map(async (page) => page.close()));
  }
  async refreshPages() {
    const result = await sendLiveCdpInitCommand(
      this.connection,
      "Target.getTargets",
      {},
      void 0,
      "Target.getTargets"
    );
    const targetInfos = Array.isArray(result.targetInfos) ? result.targetInfos.map((rawInfo) => normalizeTargetInfo(rawInfo)).filter(
      (targetInfo) => Boolean(targetInfo && isPageLikeTarget(targetInfo))
    ) : [];
    const hasYoetzTarget = targetInfos.some(isYoetzTarget);
    const targetInfosToAttach = targetInfos.filter(
      (targetInfo) => !hasYoetzTarget || isYoetzTarget(targetInfo) || this.#pagesByTargetId.has(targetInfo.targetId)
    ).sort(compareTargetAttachPriority);
    const liveTargetIds = /* @__PURE__ */ new Set();
    const attachErrors = [];
    for (const targetInfo of targetInfosToAttach) {
      const existingPage = this.#pagesByTargetId.get(targetInfo.targetId);
      if (existingPage) {
        liveTargetIds.add(targetInfo.targetId);
        existingPage.updateTargetInfo(targetInfo);
        continue;
      }
      try {
        await this.attachToTarget(targetInfo);
        liveTargetIds.add(targetInfo.targetId);
      } catch (error) {
        attachErrors.push(
          new Error(
            `Failed to attach live-CDP target ${targetInfo.targetId}: ${error instanceof Error ? error.message : String(error)}`
          )
        );
      }
    }
    for (const [targetId, page] of this.#pagesByTargetId.entries()) {
      if (!liveTargetIds.has(targetId)) {
        this.markPageClosed(page);
      }
    }
    if (this.#pagesByTargetId.size === 0 && attachErrors.length > 0) {
      throw attachErrors[0];
    }
  }
  async attachToTarget(targetInfo) {
    const existingPage = this.#pagesByTargetId.get(targetInfo.targetId);
    if (existingPage && !existingPage.isClosed()) {
      return existingPage;
    }
    const attachResult = await sendLiveCdpInitCommand(
      this.connection,
      "Target.attachToTarget",
      {
        targetId: targetInfo.targetId,
        flatten: true
      },
      void 0,
      `Target.attachToTarget(${targetInfo.targetId})`,
      LIVE_CDP_TARGET_INIT_TIMEOUT_MS
    );
    const sessionId = typeof attachResult.sessionId === "string" ? attachResult.sessionId : void 0;
    if (!sessionId) {
      throw new Error(
        `Target.attachToTarget did not return a sessionId for ${targetInfo.targetId}`
      );
    }
    try {
      await sendLiveCdpInitCommand(
        this.connection,
        "Page.enable",
        {},
        sessionId,
        `Page.enable(${targetInfo.targetId})`,
        LIVE_CDP_TARGET_INIT_TIMEOUT_MS
      );
      const mainFrameId = await this.readMainFrameId(sessionId, targetInfo.targetId);
      await sendLiveCdpInitCommand(
        this.connection,
        "Runtime.enable",
        {},
        sessionId,
        `Runtime.enable(${targetInfo.targetId})`,
        LIVE_CDP_TARGET_INIT_TIMEOUT_MS
      );
      await sendLiveCdpInitCommand(
        this.connection,
        "Runtime.runIfWaitingForDebugger",
        {},
        sessionId,
        `Runtime.runIfWaitingForDebugger(${targetInfo.targetId})`,
        LIVE_CDP_TARGET_INIT_TIMEOUT_MS
      ).catch(() => {
      });
      await sendLiveCdpInitCommand(
        this.connection,
        "Network.enable",
        {},
        sessionId,
        `Network.enable(${targetInfo.targetId})`,
        LIVE_CDP_TARGET_INIT_TIMEOUT_MS
      );
      const page = new LiveCdpPage(this.connection, targetInfo, sessionId, mainFrameId, () => {
        this.markPageClosed(page);
      });
      this.#pagesByTargetId.set(targetInfo.targetId, page);
      this.#pagesBySessionId.set(sessionId, page);
      return page;
    } catch (error) {
      await this.connection.send("Target.detachFromTarget", { sessionId }, void 0, 1e3).catch(() => void 0);
      throw error;
    }
  }
  async readMainFrameId(sessionId, targetId) {
    const result = await sendLiveCdpInitCommand(
      this.connection,
      "Page.getFrameTree",
      {},
      sessionId,
      `Page.getFrameTree(${targetId})`,
      LIVE_CDP_TARGET_INIT_TIMEOUT_MS
    ).catch(() => void 0);
    const frameTree = result && typeof result.frameTree === "object" && result.frameTree !== null ? result.frameTree : void 0;
    const frame = typeof frameTree?.frame === "object" && frameTree.frame !== null ? frameTree.frame : void 0;
    return typeof frame?.id === "string" ? frame.id : void 0;
  }
  handleEvent(event) {
    if (event.method === "Target.targetDestroyed") {
      const targetId = typeof event.params.targetId === "string" ? event.params.targetId : void 0;
      const page = targetId ? this.#pagesByTargetId.get(targetId) : void 0;
      if (page) {
        this.markPageClosed(page);
      }
      return;
    }
    if (event.method === "Target.detachedFromTarget") {
      const sessionId = typeof event.params.sessionId === "string" ? event.params.sessionId : event.sessionId;
      const page = sessionId ? this.#pagesBySessionId.get(sessionId) : void 0;
      if (page) {
        this.markPageClosed(page);
      }
      return;
    }
    if (!event.sessionId) {
      return;
    }
    this.#pagesBySessionId.get(event.sessionId)?.handleSessionEvent(event);
  }
  markPageClosed(page) {
    this.#pagesByTargetId.delete(page.targetId);
    this.#pagesBySessionId.delete(page.sessionId);
    page.markClosed();
  }
};
var LiveCdpKeyboard = class {
  constructor(page) {
    this.page = page;
  }
  #modifiers = 0;
  async type(text) {
    for (const char of Array.from(text)) {
      await this.press(char);
    }
  }
  async down(key) {
    const { definition, modifiers } = parseKeyChord(key);
    const keyModifier = modifierMaskForKey(definition.key);
    const activeModifiers = this.#modifiers | modifiers | keyModifier;
    await this.dispatchKeyEvent("keyDown", definition, activeModifiers);
    this.#modifiers = activeModifiers;
  }
  async up(key) {
    const { definition, modifiers } = parseKeyChord(key);
    const keyModifier = modifierMaskForKey(definition.key);
    const activeModifiers = (this.#modifiers | modifiers) & ~keyModifier;
    await this.dispatchKeyEvent("keyUp", definition, activeModifiers);
    this.#modifiers = activeModifiers;
  }
  async press(key) {
    const { definition, modifiers } = parseKeyChord(key);
    const activeModifiers = this.#modifiers | modifiers;
    await this.dispatchKeyEvent("keyDown", definition, activeModifiers);
    if (definition.text && activeModifiers === 0) {
      await this.dispatchKeyEvent("char", definition, activeModifiers);
    }
    await this.dispatchKeyEvent("keyUp", definition, activeModifiers);
  }
  async dispatchKeyEvent(type, definition, modifiers) {
    await this.page.sendSession("Input.dispatchKeyEvent", {
      type,
      key: definition.key,
      code: definition.code,
      windowsVirtualKeyCode: definition.windowsVirtualKeyCode,
      nativeVirtualKeyCode: definition.windowsVirtualKeyCode,
      text: type === "char" ? definition.text : void 0,
      unmodifiedText: definition.text,
      modifiers
    });
  }
};
var LiveCdpMouse = class {
  constructor(page) {
    this.page = page;
  }
  async click(x, y) {
    await this.page.clickPoint({ x, y });
  }
};
var LiveCdpPage = class extends EventEmitter {
  constructor(connection, targetInfo, sessionId, mainFrameId, onClose) {
    super();
    this.connection = connection;
    this.sessionId = sessionId;
    this.onClose = onClose;
    this.targetId = targetInfo.targetId;
    this.#mainFrameId = mainFrameId;
    this.#url = targetInfo.url ?? "about:blank";
    this.#title = targetInfo.title ?? "";
  }
  keyboard = new LiveCdpKeyboard(this);
  mouse = new LiveCdpMouse(this);
  #closed = false;
  #mainFrameId;
  #url;
  #title;
  targetId;
  isClosed() {
    return this.#closed;
  }
  url() {
    return this.#url;
  }
  async title() {
    const title = await this.evaluateSerialized({
      kind: "expression",
      source: "document.title"
    });
    return typeof title === "string" ? title : "";
  }
  async goto(url, options = {}) {
    const waitUntil = options.waitUntil ?? "load";
    const timeout = options.timeout ?? 3e4;
    const waitForNavigation = waitUntil === "commit" ? Promise.resolve() : this.waitForSessionEvent(
      waitUntil === "domcontentloaded" ? "Page.domContentEventFired" : "Page.loadEventFired",
      timeout
    );
    const result = await this.sendSession("Page.navigate", { url });
    const errorText = typeof result.errorText === "string" ? result.errorText : void 0;
    if (errorText) {
      throw new Error(`Navigation to ${url} failed: ${errorText}`);
    }
    this.#url = url;
    await waitForNavigation;
    if (waitUntil === "networkidle") {
      await this.waitForTimeout(500);
    }
    return null;
  }
  async reload(options = {}) {
    const waitUntil = options.waitUntil ?? "load";
    const timeout = options.timeout ?? 3e4;
    const waitForNavigation = waitUntil === "commit" ? Promise.resolve() : this.waitForSessionEvent(
      waitUntil === "domcontentloaded" ? "Page.domContentEventFired" : "Page.loadEventFired",
      timeout
    );
    await this.sendSession("Page.reload", {});
    await waitForNavigation;
    if (waitUntil === "networkidle") {
      await this.waitForTimeout(500);
    }
    return null;
  }
  async close() {
    if (this.#closed) {
      return;
    }
    await this.connection.send("Target.closeTarget", { targetId: this.targetId }).catch(() => void 0);
    this.onClose();
  }
  async evaluate(pageFunction, ...args) {
    if (typeof pageFunction === "string") {
      return await this.evaluateSerialized({
        kind: "expression",
        source: pageFunction
      });
    }
    return await this.evaluateSerialized({
      kind: "function",
      source: pageFunction.toString(),
      hasArg: args.length > 0,
      arg: args[0],
      args
    });
  }
  async evaluateSerialized(payload) {
    if (payload.kind === "expression") {
      const result2 = await this.sendSession("Runtime.evaluate", {
        expression: payload.source,
        awaitPromise: true,
        returnByValue: true
      });
      return readRuntimeResult(result2);
    }
    const args = payload.args ?? (payload.hasArg ? [payload.arg] : []);
    const result = await this.sendSession("Runtime.evaluate", {
      expression: `(() => {
        const fn = (${payload.source});
        const args = [${args.map((value) => serializeRuntimeArgument(value)).join(",")}];
        return fn(...args);
      })()`,
      awaitPromise: true,
      returnByValue: true
    });
    return readRuntimeResult(result);
  }
  async waitForTimeout(timeout) {
    await new Promise((resolve) => {
      setTimeout(resolve, Math.max(0, timeout));
    });
  }
  async click(selector, options = {}) {
    await this.locator(selector).click(options);
  }
  async fill(selector, value) {
    await this.locator(selector).fill(value);
  }
  async type(selector, text) {
    await this.locator(selector).fill("");
    await this.locator(selector).click();
    await this.keyboard.type(text);
  }
  async press(selector, key) {
    await this.locator(selector).click();
    await this.keyboard.press(key);
  }
  async check(selector) {
    await this.locator(selector).setChecked(true);
  }
  async uncheck(selector) {
    await this.locator(selector).setChecked(false);
  }
  async selectOption(selector, value) {
    return await this.locator(selector).selectOption(value);
  }
  async textContent(selector) {
    return await this.locator(selector).textContent();
  }
  async innerText(selector) {
    return await this.locator(selector).innerText();
  }
  async innerHTML(selector) {
    return await this.locator(selector).innerHTML();
  }
  async getAttribute(selector, name) {
    return await this.locator(selector).getAttribute(name);
  }
  async inputValue(selector) {
    return await this.locator(selector).inputValue();
  }
  async isChecked(selector) {
    return await this.locator(selector).isChecked();
  }
  async isVisible(selector) {
    return await this.locator(selector).isVisible();
  }
  async isHidden(selector) {
    return !await this.isVisible(selector);
  }
  async isEnabled(selector) {
    return await this.locator(selector).isEnabled();
  }
  async waitForSelector(selector, options = {}) {
    const state = options.state ?? "visible";
    const deadline = Date.now() + (options.timeout ?? 3e4);
    while (Date.now() <= deadline) {
      const locator = this.locator(selector);
      const count = await locator.count();
      const visible = count > 0 ? await locator.isVisible().catch(() => false) : false;
      if (state === "attached" && count > 0 || state === "detached" && count === 0 || state === "visible" && visible || state === "hidden" && !visible) {
        return null;
      }
      await this.waitForTimeout(100);
    }
    throw new Error(`Timed out waiting for selector "${selector}" to become ${state}`);
  }
  async waitForFunction(pageFunction, arg, options = {}) {
    const deadline = Date.now() + (options.timeout ?? 3e4);
    while (Date.now() <= deadline) {
      const value = typeof pageFunction === "string" ? await this.evaluate(pageFunction) : await this.evaluate(pageFunction, arg);
      if (value) {
        return value;
      }
      await this.waitForTimeout(100);
    }
    throw new Error("Timed out waiting for function");
  }
  locator(selectorOrDescriptor) {
    const descriptor = typeof selectorOrDescriptor === "string" ? { selector: selectorOrDescriptor, index: null, hasText: null } : selectorOrDescriptor;
    return new LiveCdpLocator(this, descriptor);
  }
  async content() {
    const content = await this.evaluateSerialized({
      kind: "expression",
      source: "document.documentElement.outerHTML"
    });
    return typeof content === "string" ? content : "";
  }
  async setContent(html) {
    await this.evaluateSerialized({
      kind: "function",
      source: `(html) => {
        document.open();
        document.write(html);
        document.close();
      }`,
      hasArg: true,
      arg: html
    });
  }
  async setInputFiles(selector, files) {
    const fileList = Array.isArray(files) ? files : [files];
    const scopedMarkerRequirements = findScopedMarkerRequirements(selector);
    const scopedMarkers = scopedMarkerRequirements.map((marker) => marker.description);
    await this.sendSession("DOM.enable", {}).catch(() => {
    });
    const documentResult = await this.sendSession("DOM.getDocument", {
      depth: 0,
      pierce: false
    });
    const root = documentResult.root;
    const nodeId = typeof root?.nodeId === "number" ? root.nodeId : void 0;
    if (nodeId === void 0) {
      throw new Error("DOM.getDocument did not return a root node");
    }
    const queryResult = await this.sendSession("DOM.querySelectorAll", {
      nodeId,
      selector
    });
    const inputNodeIds = Array.isArray(queryResult.nodeIds) ? queryResult.nodeIds.filter((candidate) => typeof candidate === "number") : [];
    const baseDetails = (diagnostics, diagnosticsTruncated = false) => ({
      selector,
      fileCount: fileList.length,
      matchCount: inputNodeIds.length,
      targetId: this.targetId,
      url: this.#url,
      scopedMarkers,
      diagnostics,
      diagnosticsTruncated
    });
    if (inputNodeIds.length === 0) {
      throw new LiveCdpSetInputFilesError(
        `No file input found for selector "${selector}"`,
        baseDetails([])
      );
    }
    let selectedNode;
    let selectedViaScopedMarker = false;
    if (inputNodeIds.length === 1) {
      selectedNode = await describeInputFileCandidate(this, inputNodeIds[0]);
    } else {
      const shouldInspectAllMatches = scopedMarkerRequirements.length > 0;
      const inspectedNodeIds = shouldInspectAllMatches ? inputNodeIds : inputNodeIds.slice(0, INPUT_FILE_DIAGNOSTIC_LIMIT);
      const diagnostics = await Promise.all(
        inspectedNodeIds.map((candidateNodeId) => describeInputFileCandidate(this, candidateNodeId))
      );
      const scopedMarkerMatches = diagnostics.filter(
        (diagnostic) => matchesAnyScopedMarker(diagnostic, scopedMarkerRequirements)
      );
      if (scopedMarkerMatches.length === 1) {
        selectedNode = scopedMarkerMatches[0];
        selectedViaScopedMarker = true;
      } else {
        throw new LiveCdpSetInputFilesError(
          `Ambiguous file input selector "${selector}" matched ${inputNodeIds.length} nodes`,
          baseDetails(diagnostics, inspectedNodeIds.length < inputNodeIds.length)
        );
      }
    }
    if (!isFileInputDiagnostic(selectedNode)) {
      throw new LiveCdpSetInputFilesError(
        `Selector "${selector}" resolved to ${selectedNode.selectorHint}, not an input[type=file]`,
        baseDetails([selectedNode])
      );
    }
    await this.sendSession("DOM.setFileInputFiles", {
      nodeId: selectedNode.nodeId,
      files: fileList
    });
    return {
      selector,
      fileCount: fileList.length,
      matchCount: inputNodeIds.length,
      targetId: this.targetId,
      url: this.#url,
      selectedNode,
      selectedViaScopedMarker,
      scopedMarkers
    };
  }
  async screenshot(options = {}) {
    const params = {
      format: options.type === "jpeg" ? "jpeg" : "png",
      captureBeyondViewport: options.fullPage ?? true
    };
    if (params.format === "jpeg" && typeof options.quality === "number") {
      params.quality = Math.max(0, Math.min(100, Math.round(options.quality)));
    }
    const result = await this.sendSession("Page.captureScreenshot", params);
    if (typeof result.data !== "string") {
      throw new Error("Page.captureScreenshot did not return image data");
    }
    return Buffer.from(result.data, "base64");
  }
  sendSession(method, params = {}) {
    if (this.#closed) {
      return Promise.reject(new Error(`Page target ${this.targetId} is closed`));
    }
    return this.connection.send(method, params, this.sessionId);
  }
  async clickPoint(point) {
    await this.sendSession("Input.dispatchMouseEvent", {
      type: "mousePressed",
      x: point.x,
      y: point.y,
      button: "left",
      clickCount: 1
    });
    await this.sendSession("Input.dispatchMouseEvent", {
      type: "mouseReleased",
      x: point.x,
      y: point.y,
      button: "left",
      clickCount: 1
    });
  }
  async locatorCount(descriptor) {
    const count = await this.evaluate((input) => {
      const { selector, hasText } = input;
      const elements = Array.from(document.querySelectorAll(selector));
      return hasText === null || hasText === void 0 ? elements.length : elements.filter((element) => (element.textContent ?? "").includes(hasText)).length;
    }, normalizeLocatorDescriptor(descriptor));
    return typeof count === "number" ? count : 0;
  }
  async locatorAction(descriptor, action, args = []) {
    return await this.evaluate(
      (input) => {
        const {
          descriptor: descriptor2,
          actionSource,
          args: actionArgs
        } = input;
        const normalized = descriptor2;
        const elements = Array.from(document.querySelectorAll(normalized.selector));
        const filtered = normalized.hasText === null || normalized.hasText === void 0 ? elements : elements.filter(
          (element2) => (element2.textContent ?? "").includes(normalized.hasText ?? "")
        );
        const rawIndex = normalized.index === null || normalized.index === void 0 ? 0 : normalized.index;
        const index = rawIndex < 0 ? filtered.length + rawIndex : rawIndex;
        const element = filtered[index];
        if (!element) {
          throw new Error(`No element found for selector "${normalized.selector}"`);
        }
        const run = (0, eval)(`(${actionSource})`);
        return run(element, actionArgs);
      },
      {
        descriptor: normalizeLocatorDescriptor(descriptor),
        actionSource: action.toString(),
        args
      }
    );
  }
  handleSessionEvent(event) {
    if (event.method === "Page.frameNavigated") {
      const frame = typeof event.params.frame === "object" && event.params.frame !== null ? event.params.frame : void 0;
      if (frame && frame.parentId === void 0 && typeof frame.url === "string") {
        if (typeof frame.id === "string") {
          this.#mainFrameId = frame.id;
        }
        this.#url = frame.url;
      }
    }
    if (event.method === "Page.navigatedWithinDocument" && typeof event.params.url === "string" && (typeof event.params.frameId !== "string" || event.params.frameId === this.#mainFrameId)) {
      this.#url = event.params.url;
    }
    this.emit(`cdp:${event.method}`, event.params);
  }
  updateTargetInfo(targetInfo) {
    this.#url = targetInfo.url ?? this.#url;
    this.#title = targetInfo.title ?? this.#title;
  }
  markClosed() {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    this.emit("close");
  }
  waitForSessionEvent(method, timeout) {
    return new Promise((resolve, reject) => {
      if (this.#closed) {
        reject(new Error(`Page target ${this.targetId} is closed while waiting for ${method}`));
        return;
      }
      const timeoutId = setTimeout(() => {
        cleanup();
        reject(new Error(`Timed out waiting for ${method}`));
      }, timeout);
      const handleEvent = () => {
        cleanup();
        resolve();
      };
      const handleClose = () => {
        cleanup();
        reject(new Error(`Page target ${this.targetId} closed while waiting for ${method}`));
      };
      const cleanup = () => {
        clearTimeout(timeoutId);
        this.off(`cdp:${method}`, handleEvent);
        this.off("close", handleClose);
      };
      this.on(`cdp:${method}`, handleEvent);
      this.on("close", handleClose);
    });
  }
};
var LiveCdpLocator = class _LiveCdpLocator {
  constructor(page, descriptor) {
    this.page = page;
    this.descriptor = descriptor;
  }
  async click(options = {}) {
    await this.waitFor({ state: "visible", timeout: options.timeout });
    const point = await this.page.locatorAction(this.descriptor, (element) => {
      element.scrollIntoView({ block: "center", inline: "center" });
      const rect = element.getBoundingClientRect();
      return {
        x: rect.left + rect.width / 2,
        y: rect.top + rect.height / 2
      };
    });
    await this.page.clickPoint(point);
  }
  async waitFor(options = {}) {
    const state = options.state ?? "visible";
    const deadline = Date.now() + (options.timeout ?? 3e4);
    while (Date.now() <= deadline) {
      const count = await this.count();
      const visible = count > 0 ? await this.isVisible().catch(() => false) : false;
      if (state === "attached" && count > 0 || state === "detached" && count === 0 || state === "visible" && visible || state === "hidden" && !visible) {
        return;
      }
      await this.page.waitForTimeout(100);
    }
    throw new Error(`Timed out waiting for locator "${this.descriptor.selector}" to become ${state}`);
  }
  async pressSequentially(text, options = {}) {
    await this.waitFor({ state: "visible", timeout: options.timeout });
    await this.click();
    if (!options.delay) {
      await this.page.keyboard.type(text);
      return;
    }
    for (const char of text) {
      await this.page.keyboard.type(char);
      await this.page.waitForTimeout(options.delay);
    }
  }
  async fill(value) {
    await this.page.locatorAction(
      this.descriptor,
      (element, [nextValue]) => {
        const htmlElement = element;
        htmlElement.focus();
        if ("value" in htmlElement) {
          htmlElement.value = String(nextValue);
        } else if (htmlElement.isContentEditable) {
          htmlElement.textContent = String(nextValue);
        } else {
          throw new Error("Element cannot be filled");
        }
        htmlElement.dispatchEvent(new Event("input", { bubbles: true }));
        htmlElement.dispatchEvent(new Event("change", { bubbles: true }));
      },
      [value]
    );
  }
  async textContent() {
    return await this.page.locatorAction(
      this.descriptor,
      (element) => element.textContent
    );
  }
  async innerText() {
    return await this.page.locatorAction(
      this.descriptor,
      (element) => element.innerText ?? element.textContent ?? ""
    );
  }
  async innerHTML() {
    return await this.page.locatorAction(this.descriptor, (element) => element.innerHTML);
  }
  async getAttribute(name) {
    return await this.page.locatorAction(
      this.descriptor,
      (element, [attributeName]) => element.getAttribute(String(attributeName)),
      [name]
    );
  }
  async inputValue() {
    return await this.page.locatorAction(
      this.descriptor,
      (element) => element.value ?? ""
    );
  }
  async isChecked() {
    return await this.page.locatorAction(
      this.descriptor,
      (element) => Boolean(element.checked)
    );
  }
  async isVisible() {
    return await this.page.locatorAction(this.descriptor, (element) => {
      const style = window.getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.visibility !== "hidden" && style.display !== "none" && Number(style.opacity) !== 0 && rect.width > 0 && rect.height > 0;
    });
  }
  async isEnabled() {
    return await this.page.locatorAction(
      this.descriptor,
      (element) => !element.disabled
    );
  }
  async setChecked(checked) {
    await this.page.locatorAction(
      this.descriptor,
      (element, [nextChecked]) => {
        const input = element;
        if (input.checked !== Boolean(nextChecked)) {
          input.checked = Boolean(nextChecked);
          input.dispatchEvent(new Event("input", { bubbles: true }));
          input.dispatchEvent(new Event("change", { bubbles: true }));
        }
      },
      [checked]
    );
  }
  async selectOption(value) {
    return await this.page.locatorAction(
      this.descriptor,
      (element, [nextValue]) => {
        const select = element;
        const values = Array.isArray(nextValue) ? nextValue.map(String) : [String(nextValue)];
        for (const option of Array.from(select.options)) {
          option.selected = values.includes(option.value);
        }
        select.dispatchEvent(new Event("input", { bubbles: true }));
        select.dispatchEvent(new Event("change", { bubbles: true }));
        return Array.from(select.selectedOptions).map((option) => option.value);
      },
      [value]
    );
  }
  async count() {
    return await this.page.locatorCount(this.descriptor);
  }
  locator(selector) {
    return new _LiveCdpLocator(this.page, {
      selector: `${this.descriptor.selector} ${selector}`,
      index: null,
      hasText: this.descriptor.hasText ?? null
    });
  }
  first() {
    return this.nth(0);
  }
  last() {
    return this.nth(-1);
  }
  nth(index) {
    return new _LiveCdpLocator(this.page, {
      ...normalizeLocatorDescriptor(this.descriptor),
      index
    });
  }
  filter(options = {}) {
    return new _LiveCdpLocator(this.page, {
      ...normalizeLocatorDescriptor(this.descriptor),
      hasText: options.hasText === void 0 ? null : String(options.hasText)
    });
  }
  async all() {
    const count = await this.count();
    return Array.from({ length: count }, (_, index) => this.nth(index));
  }
};
async function createLiveCdpBrowser(endpoint, options = {}) {
  return await LiveCdpBrowser.connect(endpoint, options);
}
function getLiveCdpPageTargetId(value) {
  return value instanceof LiveCdpPage ? value.targetId : null;
}
async function sendLiveCdpInitCommand(connection, method, params = {}, sessionId, step, timeoutMs = LIVE_CDP_BROWSER_INIT_TIMEOUT_MS) {
  try {
    return await connection.send(method, params, sessionId, timeoutMs);
  } catch (error) {
    if (isCdpCommandTimeout(error)) {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(
        `Timed out after ${timeoutMs}ms initializing live CDP browser during ${step}. Chrome may be waiting for remote-debugging consent or a target may be unresponsive. Last error: ${message}`
      );
    }
    throw error;
  }
}
function isCdpCommandTimeout(error) {
  return error instanceof Error && error.message.startsWith("CDP command timed out after ");
}
function normalizeTargetInfo(value) {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const record = value;
  const targetId = record.targetId;
  const type = record.type;
  if (typeof targetId !== "string" || typeof type !== "string") {
    return null;
  }
  return {
    targetId,
    type,
    url: typeof record.url === "string" ? record.url : void 0,
    title: typeof record.title === "string" ? record.title : void 0
  };
}
function normalizeLocatorDescriptor(descriptor) {
  return {
    selector: descriptor.selector,
    index: descriptor.index ?? null,
    hasText: descriptor.hasText ?? null
  };
}
async function describeInputFileCandidate(page, nodeId) {
  const describeResult = await page.sendSession("DOM.describeNode", {
    nodeId,
    depth: 0,
    pierce: false
  });
  const node = typeof describeResult.node === "object" && describeResult.node !== null ? describeResult.node : void 0;
  const attributes = readDomAttributes(node?.attributes);
  const nodeName = typeof node?.nodeName === "string" ? node.nodeName : void 0;
  const localName = typeof node?.localName === "string" ? node.localName : void 0;
  return {
    nodeId: typeof node?.nodeId === "number" ? node.nodeId : nodeId,
    backendNodeId: typeof node?.backendNodeId === "number" ? node.backendNodeId : void 0,
    nodeName,
    localName,
    attributes,
    selectorHint: buildInputFileSelectorHint(localName ?? nodeName, attributes)
  };
}
function readDomAttributes(attributes) {
  if (!Array.isArray(attributes)) {
    return {};
  }
  const result = {};
  for (let index = 0; index + 1 < attributes.length; index += 2) {
    const name = attributes[index];
    const value = attributes[index + 1];
    if (typeof name === "string" && typeof value === "string") {
      result[name.toLowerCase()] = value;
    }
  }
  return result;
}
function buildInputFileSelectorHint(rawName, attributes) {
  const tagName = rawName ? rawName.toLowerCase() : "node";
  const parts = [tagName];
  if (attributes.id) {
    parts.push(`#${truncateDiagnosticValue(attributes.id, 60)}`);
  }
  const interestingAttributes = [
    "type",
    "name",
    "title",
    "accept",
    "multiple",
    "aria-label",
    "data-testid",
    ...Object.keys(attributes).filter((attributeName) => attributeName.startsWith("data-yoetz-")).sort()
  ];
  const seen = /* @__PURE__ */ new Set();
  for (const attributeName of interestingAttributes) {
    if (seen.has(attributeName) || !(attributeName in attributes)) {
      continue;
    }
    seen.add(attributeName);
    const value = attributes[attributeName];
    if (value === "") {
      parts.push(`[${attributeName}]`);
    } else {
      parts.push(`[${attributeName}=${JSON.stringify(truncateDiagnosticValue(value, 80))}]`);
    }
  }
  return parts.join("");
}
function isFileInputDiagnostic(diagnostic) {
  const tagName = (diagnostic.localName ?? diagnostic.nodeName ?? "").toLowerCase();
  return tagName === "input" && diagnostic.attributes.type?.toLowerCase() === "file";
}
function findScopedMarkerRequirements(selector) {
  const markers = [];
  const attributePattern = /\[\s*([a-zA-Z_][-\w:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\]\s]+))\s*\]/g;
  for (const match of selector.matchAll(attributePattern)) {
    const attributeName = match[1].toLowerCase();
    const value = match[2] ?? match[3] ?? match[4] ?? "";
    if (!isScopedMarkerAttribute(attributeName, value)) {
      continue;
    }
    markers.push({
      attributeName,
      value,
      description: `${attributeName}=${JSON.stringify(value)}`
    });
  }
  return markers;
}
function isScopedMarkerAttribute(attributeName, value) {
  if (attributeName.startsWith("data-yoetz-")) {
    return value.length > 0;
  }
  return attributeName === "title" && value.startsWith("yoetz-");
}
function matchesAnyScopedMarker(diagnostic, requirements) {
  return requirements.some(
    (requirement) => diagnostic.attributes[requirement.attributeName] === requirement.value
  );
}
function formatSetInputFilesErrorDetails(details) {
  const parts = [
    `matches=${details.matchCount}`,
    `files=${details.fileCount}`,
    `target=${details.targetId}`
  ];
  if (details.url) {
    parts.push(`url=${JSON.stringify(truncateDiagnosticValue(details.url, 120))}`);
  }
  if (details.scopedMarkers.length > 0) {
    parts.push(`scopedMarkers=[${details.scopedMarkers.join(", ")}]`);
  }
  if (details.diagnostics.length > 0) {
    const candidates = details.diagnostics.map(formatInputFileCandidateDiagnostic).join("; ");
    parts.push(
      `candidates=[${candidates}${details.diagnosticsTruncated ? "; ..." : ""}]`
    );
  }
  return `(${parts.join(", ")})`;
}
function formatInputFileCandidateDiagnostic(diagnostic) {
  const backend = diagnostic.backendNodeId === void 0 ? "" : ` backendNodeId=${diagnostic.backendNodeId}`;
  return `nodeId=${diagnostic.nodeId}${backend} ${diagnostic.selectorHint}`;
}
function truncateDiagnosticValue(value, maxLength) {
  if (value.length <= maxLength) {
    return value;
  }
  return `${value.slice(0, Math.max(0, maxLength - 3))}...`;
}
function compareTargetAttachPriority(left, right) {
  const leftIsYoetz = isYoetzTarget(left);
  const rightIsYoetz = isYoetzTarget(right);
  if (leftIsYoetz !== rightIsYoetz) {
    return leftIsYoetz ? -1 : 1;
  }
  return 0;
}
function isYoetzTarget(targetInfo) {
  const url = targetInfo.url ?? "";
  if (url.length === 0) {
    return false;
  }
  try {
    return new URL(url).searchParams.has("_yoetz");
  } catch {
    return /[?&]_yoetz(?:=|&|$)/.test(url);
  }
}
function serializeRuntimeArgument(value) {
  if (value === void 0) {
    return "undefined";
  }
  if (typeof value === "number") {
    if (Number.isNaN(value)) {
      return "NaN";
    }
    if (value === Infinity) {
      return "Infinity";
    }
    if (value === -Infinity) {
      return "-Infinity";
    }
  }
  const serialized = JSON.stringify(value);
  if (serialized === void 0) {
    throw new Error(`Cannot pass ${typeof value} to live-CDP evaluation`);
  }
  return serialized;
}
function readRuntimeResult(result) {
  if (result.exceptionDetails) {
    const exception = result.exceptionDetails.exception;
    const message = exception?.description ?? exception?.value ?? result.exceptionDetails.text ?? "Evaluation failed";
    throw new Error(String(message));
  }
  return readRemoteObject(result.result);
}
function readRemoteObject(remoteObject) {
  if (!remoteObject) {
    return void 0;
  }
  if ("value" in remoteObject) {
    return remoteObject.value;
  }
  switch (remoteObject.unserializableValue) {
    case "NaN":
      return Number.NaN;
    case "Infinity":
      return Infinity;
    case "-Infinity":
      return -Infinity;
    case "-0":
      return -0;
    default:
      break;
  }
  if (remoteObject.subtype === "null") {
    return null;
  }
  return void 0;
}
var keyDefinitions = /* @__PURE__ */ new Map([
  ["Enter", { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" }],
  ["Escape", { key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 }],
  ["Tab", { key: "Tab", code: "Tab", windowsVirtualKeyCode: 9, text: "	" }],
  ["Backspace", { key: "Backspace", code: "Backspace", windowsVirtualKeyCode: 8 }],
  ["Delete", { key: "Delete", code: "Delete", windowsVirtualKeyCode: 46 }],
  ["Shift", { key: "Shift", code: "ShiftLeft", windowsVirtualKeyCode: 16 }],
  ["Control", { key: "Control", code: "ControlLeft", windowsVirtualKeyCode: 17 }],
  ["Ctrl", { key: "Control", code: "ControlLeft", windowsVirtualKeyCode: 17 }],
  ["Alt", { key: "Alt", code: "AltLeft", windowsVirtualKeyCode: 18 }],
  ["Option", { key: "Alt", code: "AltLeft", windowsVirtualKeyCode: 18 }],
  ["Meta", { key: "Meta", code: "MetaLeft", windowsVirtualKeyCode: 91 }],
  ["Cmd", { key: "Meta", code: "MetaLeft", windowsVirtualKeyCode: 91 }],
  ["Command", { key: "Meta", code: "MetaLeft", windowsVirtualKeyCode: 91 }],
  ["ArrowLeft", { key: "ArrowLeft", code: "ArrowLeft", windowsVirtualKeyCode: 37 }],
  ["ArrowUp", { key: "ArrowUp", code: "ArrowUp", windowsVirtualKeyCode: 38 }],
  ["ArrowRight", { key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39 }],
  ["ArrowDown", { key: "ArrowDown", code: "ArrowDown", windowsVirtualKeyCode: 40 }]
]);
function isPageLikeTarget(targetInfo) {
  if (targetInfo.type === "page") {
    return true;
  }
  if (targetInfo.type !== "other") {
    return false;
  }
  return /^(https?|about|chrome):/i.test(targetInfo.url ?? "");
}
function parseKeyChord(key) {
  const parts = key.split("+").map((part) => part.trim()).filter(Boolean);
  const keyName = parts.pop() ?? key;
  const modifiers = parts.reduce((mask, modifier) => {
    switch (modifier.toLowerCase()) {
      case "alt":
      case "option":
        return mask | 1;
      case "control":
      case "ctrl":
        return mask | 2;
      case "meta":
      case "cmd":
      case "command":
        return mask | 4;
      case "shift":
        return mask | 8;
      default:
        return mask;
    }
  }, 0);
  return {
    definition: keyDefinitionFor(keyName),
    modifiers
  };
}
function modifierMaskForKey(key) {
  switch (key.toLowerCase()) {
    case "alt":
    case "option":
      return 1;
    case "control":
    case "ctrl":
      return 2;
    case "meta":
    case "cmd":
    case "command":
      return 4;
    case "shift":
      return 8;
    default:
      return 0;
  }
}
function keyDefinitionFor(key) {
  const existing = keyDefinitions.get(key);
  if (existing) {
    return existing;
  }
  if (/^[a-z]$/i.test(key)) {
    const upper = key.toUpperCase();
    return {
      key: key.toLowerCase(),
      code: `Key${upper}`,
      windowsVirtualKeyCode: upper.charCodeAt(0),
      text: key
    };
  }
  return {
    key,
    code: key,
    windowsVirtualKeyCode: key.length === 1 ? key.toUpperCase().charCodeAt(0) : 0,
    text: key.length === 1 ? key : void 0
  };
}

// daemon.ts
var BASE_DIR = path.join(os.homedir(), ".yoetz");
var SOCKET_PATH = process.platform === "win32" ? `\\\\.\\pipe\\yoetz-live-cdp-daemon-${sanitizePipeSegment(
  process.env.USERNAME || process.env.USER || os.userInfo().username || "user"
)}` : path.join(BASE_DIR, "live-cdp-daemon.sock");
var PID_PATH = path.join(BASE_DIR, "live-cdp-daemon.pid");
var DEV_BROWSER_TMP_DIR = path.join(os.homedir(), ".dev-browser", "tmp");
var DISCOVERY_PORTS = [9222, 9223, 9224, 9225, 9226, 9227, 9228, 9229];
var PROBE_TIMEOUT_MS = 750;
var MANUAL_CONNECT_TIMEOUT_MS = 5e3;
var DEFAULT_SCRIPT_TIMEOUT_MS = 3e4;
var PAGE_TITLE_TIMEOUT_MS = 1500;
var SOCKET_CLOSE_TIMEOUT_MS = 500;
var TARGET_ID_PATTERN = /^[a-f0-9]{16,}$/i;
var SAFE_PATH_SEGMENT_PATTERN = /[^A-Za-z0-9._-]/g;
var NOFOLLOW_FLAG = constants.O_NOFOLLOW ?? 0;
var YOETZ_DAEMON_VERSION = await computeDaemonVersion();
var startedAt = Date.now();
var clients = /* @__PURE__ */ new Set();
var manager;
var server = null;
var shuttingDown = null;
if (process.platform !== "win32") {
  process.umask(63);
}
if (process.argv.includes("--self-test")) {
  if (typeof globalThis.WebSocket !== "function") {
    throw new Error("Node.js runtime does not expose global WebSocket");
  }
  process.stdout.write("yoetz live-cdp daemon ok\n");
  process.exit(0);
}
async function computeDaemonVersion() {
  const source = await readFile(fileURLToPath(import.meta.url));
  return createHash("sha256").update(source).digest("hex");
}
var LiveCdpBrowserManager = class {
  #browsers = /* @__PURE__ */ new Map();
  async connectBrowser(name, endpoint) {
    const resolved = await this.resolveEndpoint(endpoint || "auto");
    const existing = this.#browsers.get(name);
    if (existing?.endpoint === resolved && existing.browser.isConnected()) {
      return existing;
    }
    if (existing) {
      await this.stopBrowser(name);
    }
    const browser = await createLiveCdpBrowser(resolved);
    const context = browser.contexts()[0] ?? await browser.newContext();
    const entry = {
      name,
      browser,
      context,
      endpoint: resolved,
      pages: /* @__PURE__ */ new Map()
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
  async getPage(browserName, pageNameOrId) {
    const entry = this.getBrowserEntry(browserName);
    const existing = entry.pages.get(pageNameOrId);
    if (existing && !existing.isClosed()) {
      return existing;
    }
    entry.pages.delete(pageNameOrId);
    if (TARGET_ID_PATTERN.test(pageNameOrId)) {
      const page2 = await this.findPageByTargetId(entry, pageNameOrId);
      if (page2) {
        return page2;
      }
    }
    const page = await entry.context.newPage();
    this.registerNamedPage(entry, pageNameOrId, page);
    return page;
  }
  async newPage(browserName) {
    return await this.getBrowserEntry(browserName).context.newPage();
  }
  async listPages(browserName) {
    const entry = this.#browsers.get(browserName);
    if (!entry || !entry.browser.isConnected()) {
      return [];
    }
    await entry.browser.refreshPages();
    this.pruneClosedPages(entry);
    const namesByPage = this.namedPagesByPage(entry);
    const summaries = [];
    for (const page of entry.context.pages()) {
      if (page.isClosed()) {
        continue;
      }
      summaries.push({
        id: getLiveCdpPageTargetId(page) ?? "",
        url: page.url(),
        title: await titleWithTimeout(page),
        name: namesByPage.get(page) ?? null
      });
    }
    return summaries.filter((page) => page.id.length > 0);
  }
  async closePage(browserName, pageName) {
    const entry = this.getBrowserEntry(browserName);
    const page = entry.pages.get(pageName);
    if (!page || page.isClosed()) {
      entry.pages.delete(pageName);
      throw new Error(`Page "${browserName}/${pageName}" not found`);
    }
    entry.pages.delete(pageName);
    await page.close();
  }
  async stopBrowser(name) {
    const entry = this.#browsers.get(name);
    if (!entry) {
      return;
    }
    this.#browsers.delete(name);
    entry.pages.clear();
    await entry.browser.close().catch(() => void 0);
  }
  async stopAll() {
    const names = Array.from(this.#browsers.keys());
    await Promise.allSettled(names.map((name) => this.stopBrowser(name)));
  }
  browserCount() {
    return this.#browsers.size;
  }
  listBrowsers() {
    return Array.from(this.#browsers.values()).map((entry) => {
      this.pruneClosedPages(entry);
      return {
        name: entry.name,
        type: "connected",
        status: entry.browser.isConnected() ? "connected" : "disconnected",
        pages: Array.from(entry.pages.keys()).sort((left, right) => left.localeCompare(right))
      };
    }).sort((left, right) => left.name.localeCompare(right.name));
  }
  getBrowserEntry(name) {
    const entry = this.#browsers.get(name);
    if (!entry || !entry.browser.isConnected()) {
      throw new Error(`Browser "${name}" is not connected`);
    }
    return entry;
  }
  async resolveEndpoint(endpoint) {
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
  async discoverChrome() {
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
  async resolveHttpEndpoint(endpoint, timeoutMs) {
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
  async findPageByTargetId(entry, targetId) {
    await entry.browser.refreshPages();
    return entry.context.pages().find((page) => getLiveCdpPageTargetId(page) === targetId) ?? null;
  }
  registerNamedPage(entry, name, page) {
    entry.pages.set(name, page);
    page.on("close", () => {
      if (entry.pages.get(name) === page) {
        entry.pages.delete(name);
      }
    });
  }
  pruneClosedPages(entry) {
    for (const [name, page] of entry.pages.entries()) {
      if (page.isClosed()) {
        entry.pages.delete(name);
      }
    }
  }
  namedPagesByPage(entry) {
    const names = /* @__PURE__ */ new Map();
    for (const [name, page] of entry.pages.entries()) {
      if (!page.isClosed() && !names.has(page)) {
        names.set(page, name);
      }
    }
    return names;
  }
};
async function runScript(script, browserName, output, requestId, timeoutMs) {
  const browserApi = createBrowserApi(browserName);
  const consoleApi = {
    log: (...args) => output.push({ id: requestId, type: "stdout", data: formatArgs(args) }),
    info: (...args) => output.push({ id: requestId, type: "stdout", data: formatArgs(args) }),
    warn: (...args) => output.push({ id: requestId, type: "stderr", data: formatArgs(args) }),
    error: (...args) => output.push({ id: requestId, type: "stderr", data: formatArgs(args) })
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
      URL
    },
    {
      name: "yoetz-live-cdp-script"
    }
  );
  const compiled = new vm.Script(`"use strict";
(async () => {
${script}
})()`, {
    filename: "yoetz-live-cdp-script.js"
  });
  await withTimeout(
    Promise.resolve(compiled.runInContext(context, { timeout: timeoutMs })),
    timeoutMs,
    "ScriptTimeoutError"
  );
}
function createBrowserApi(browserName) {
  return {
    getPage: (nameOrId) => manager.getPage(browserName, String(nameOrId)),
    newPage: () => manager.newPage(browserName),
    listPages: () => manager.listPages(browserName),
    closePage: (name) => manager.closePage(browserName, String(name))
  };
}
async function handleExecute(socket, request) {
  if (request.version && request.version !== YOETZ_DAEMON_VERSION) {
    await writeMessage(socket, {
      id: request.id,
      type: "error",
      message: `Daemon version mismatch: running ${YOETZ_DAEMON_VERSION}, client expected ${request.version}`
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
    await output.drain().catch(() => void 0);
    await writeMessage(socket, { id: request.id, type: "error", message: formatError(error) });
  }
}
async function handleRequest(socket, line) {
  let request;
  try {
    request = parseRequest(line);
  } catch (error) {
    await writeMessage(socket, {
      id: "unknown",
      type: "error",
      message: error instanceof Error ? error.message : String(error)
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
        data: { browser: request.browser, stopped: true }
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
          browsers: manager.listBrowsers()
        }
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
function parseRequest(line) {
  const value = JSON.parse(line);
  const id = typeof value.id === "string" && value.id.length > 0 ? value.id : void 0;
  const type = typeof value.type === "string" ? value.type : void 0;
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
      browser: typeof value.browser === "string" ? value.browser : void 0,
      script: value.script,
      connect: typeof value.connect === "string" ? value.connect : void 0,
      timeoutMs: typeof value.timeoutMs === "number" && Number.isFinite(value.timeoutMs) ? Math.max(1, Math.trunc(value.timeoutMs)) : void 0,
      version: typeof value.version === "string" ? value.version : void 0
    };
  }
  if (type === "browser-stop") {
    if (typeof value.browser !== "string" || value.browser.length === 0) {
      throw new Error("browser-stop request must include browser");
    }
    return { id, type, browser: value.browser };
  }
  if (type === "status") {
    return { id, type, version: typeof value.version === "string" ? value.version : void 0 };
  }
  if (type === "browsers" || type === "install" || type === "stop") {
    return { id, type };
  }
  throw new Error(`Unsupported request type: ${type}`);
}
function createMessageQueue(socket) {
  let queue = Promise.resolve();
  return {
    push(message) {
      queue = queue.then(() => writeMessage(socket, message)).catch(() => void 0);
      return queue;
    },
    async drain() {
      await queue;
    }
  };
}
async function writeMessage(socket, message) {
  if (socket.destroyed) {
    return;
  }
  const payload = { version: YOETZ_DAEMON_VERSION, ...message };
  await new Promise((resolve, reject) => {
    socket.write(`${JSON.stringify(payload)}
`, (error) => {
      if (error) {
        reject(error);
      } else {
        resolve();
      }
    });
  });
}
async function assertNoRunningDaemonFromPidFile() {
  let contents;
  try {
    contents = await readFile(PID_PATH, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") {
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
function processIsAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error.code === "EPERM";
  }
}
async function startServer() {
  await mkdir(BASE_DIR, { recursive: true, mode: 448 });
  if (process.platform !== "win32") {
    await chmod(BASE_DIR, 448).catch(() => void 0);
  }
  await assertNoRunningDaemonFromPidFile();
  if (process.platform !== "win32") {
    await unlink(SOCKET_PATH).catch((error) => {
      if (error.code !== "ENOENT") {
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
    socket.on("data", (chunk) => {
      buffer += chunk;
      for (; ; ) {
        const newline = buffer.indexOf("\n");
        if (newline < 0) {
          break;
        }
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (!line) {
          continue;
        }
        queue = queue.then(() => handleRequest(socket, line)).catch(async (error) => {
          if (!socket.destroyed) {
            await writeMessage(socket, {
              id: "unknown",
              type: "error",
              message: formatError(error)
            });
          }
        });
      }
    });
    socket.on("close", () => clients.delete(socket));
    socket.on("error", () => clients.delete(socket));
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(SOCKET_PATH, () => {
      server.off("error", reject);
      resolve();
    });
  });
  if (process.platform !== "win32") {
    await chmod(SOCKET_PATH, 384);
  }
  await writeFileFs(PID_PATH, `${process.pid}
`, { mode: 384 });
  process.stderr.write("yoetz live-cdp daemon ready\n");
}
async function shutdown() {
  const serverToClose = server;
  server = null;
  await manager.stopAll().catch(() => void 0);
  if (serverToClose) {
    await new Promise((resolve) => serverToClose.close(() => resolve()));
  }
  await Promise.allSettled(Array.from(clients, (socket) => closeClientSocket(socket)));
  await unlink(PID_PATH).catch(() => void 0);
  if (process.platform !== "win32") {
    await unlink(SOCKET_PATH).catch(() => void 0);
  }
  setImmediate(() => process.exit(0));
}
async function closeClientSocket(socket) {
  if (socket.destroyed) {
    return;
  }
  await new Promise((resolve) => {
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
async function fetchDebuggerWebSocketUrl(endpoint, timeoutMs) {
  try {
    const response = await fetch(toJsonVersionUrl(endpoint), {
      headers: { accept: "application/json" },
      signal: AbortSignal.timeout(timeoutMs)
    });
    if (response.status === 404) {
      return { status: "not-found" };
    }
    if (!response.ok) {
      return { status: "unavailable" };
    }
    const payload = await response.json();
    return typeof payload.webSocketDebuggerUrl === "string" && payload.webSocketDebuggerUrl.length > 0 ? { status: "ok", webSocketDebuggerUrl: payload.webSocketDebuggerUrl } : { status: "unavailable" };
  } catch {
    return { status: "unavailable" };
  }
}
function toJsonVersionUrl(endpoint) {
  const url = new URL(endpoint);
  if (url.pathname !== "/json/version") {
    url.pathname = "/json/version";
    url.search = "";
    url.hash = "";
  }
  return url;
}
async function readDevToolsActivePort(expectedPort) {
  for (const candidate of devToolsActivePortCandidates()) {
    let contents;
    try {
      contents = await readTextFile(candidate);
    } catch (error) {
      const code = error.code;
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
async function readTextFile(filePath) {
  const handle = await open(filePath, constants.O_RDONLY);
  try {
    return await handle.readFile({ encoding: "utf8" });
  } finally {
    await handle.close();
  }
}
function devToolsActivePortCandidates() {
  const home = os.homedir();
  switch (process.platform) {
    case "darwin":
      return [
        path.join(home, "Library", "Application Support", "Google", "Chrome", "DevToolsActivePort"),
        path.join(home, "Library", "Application Support", "Google", "Chrome Canary", "DevToolsActivePort"),
        path.join(home, "Library", "Application Support", "Chromium", "DevToolsActivePort"),
        path.join(home, "Library", "Application Support", "BraveSoftware", "Brave-Browser", "DevToolsActivePort")
      ];
    case "linux":
      return [
        path.join(home, ".config", "google-chrome", "DevToolsActivePort"),
        path.join(home, ".config", "chromium", "DevToolsActivePort"),
        path.join(home, ".config", "google-chrome-beta", "DevToolsActivePort"),
        path.join(home, ".config", "google-chrome-unstable", "DevToolsActivePort"),
        path.join(home, ".config", "BraveSoftware", "Brave-Browser", "DevToolsActivePort")
      ];
    case "win32":
      return [
        path.join(home, "AppData", "Local", "Google", "Chrome", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "Google", "Chrome Beta", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "Google", "Chrome SxS", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "Chromium", "User Data", "DevToolsActivePort"),
        path.join(home, "AppData", "Local", "BraveSoftware", "Brave-Browser", "User Data", "DevToolsActivePort")
      ];
    default:
      return [];
  }
}
function parseDevToolsActivePort(contents, expectedPort) {
  const lines = contents.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const port = Number.parseInt(lines[0] ?? "", 10);
  const webSocketPath = lines[1] ?? "";
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    return null;
  }
  if (expectedPort !== void 0 && port !== expectedPort) {
    return null;
  }
  if (!webSocketPath.startsWith("/devtools/browser/")) {
    return null;
  }
  return `ws://127.0.0.1:${port}${webSocketPath}`;
}
async function saveScreenshot(data, name) {
  if (data instanceof Uint8Array) {
    return await writeDevBrowserTempFile(name, data);
  }
  if (typeof data === "string") {
    return await writeDevBrowserTempFile(name, data);
  }
  throw new TypeError("saveScreenshot data must be a string or Uint8Array");
}
async function writeDevBrowserTempFile(fileName, data) {
  const destination = await resolveDevBrowserTempPath(fileName, true);
  await assertDestinationIsNotSymlink(destination);
  let handle;
  try {
    handle = await open(
      destination,
      constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC | NOFOLLOW_FLAG,
      384
    );
    await handle.writeFile(data);
  } catch (error) {
    throw normalizeSymlinkError(error, destination);
  } finally {
    await handle?.close();
  }
  return destination;
}
async function readDevBrowserTempFile(fileName) {
  const destination = await resolveDevBrowserTempPath(fileName, false);
  await assertDestinationIsNotSymlink(destination);
  let handle;
  try {
    handle = await open(destination, constants.O_RDONLY | NOFOLLOW_FLAG);
    return await handle.readFile({ encoding: "utf8" });
  } catch (error) {
    throw normalizeSymlinkError(error, destination);
  } finally {
    await handle?.close();
  }
}
async function resolveDevBrowserTempPath(fileName, createParents) {
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
function sanitizeRelativePath(fileName) {
  if (typeof fileName !== "string" || fileName.length === 0) {
    throw new TypeError("File name must be a non-empty string");
  }
  if (fileName.includes("\0") || path.posix.isAbsolute(fileName) || path.win32.isAbsolute(fileName)) {
    throw new Error("Absolute paths and null bytes are not allowed");
  }
  return fileName.replace(/\\/g, "/").split("/").map(sanitizePathSegment);
}
function sanitizePathSegment(segment) {
  if (!segment || segment === "." || segment === ".." || segment.includes("..")) {
    throw new Error("File paths must not contain empty, '.', or '..' segments");
  }
  const sanitized = segment.replace(SAFE_PATH_SEGMENT_PATTERN, "_");
  if (!sanitized || sanitized === "." || sanitized === "..") {
    throw new Error("File paths must resolve to a valid filename");
  }
  return sanitized;
}
async function assertControlledDirectory(directoryPath, label) {
  const stats = await lstat(directoryPath);
  if (stats.isSymbolicLink()) {
    throw new Error(`${label} must not be a symlink`);
  }
  if (!stats.isDirectory()) {
    throw new Error(`${label} must be a directory`);
  }
}
async function assertSafeParentDirectories(rootDir, destinationPath, createParents) {
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
async function assertDestinationIsNotSymlink(destinationPath) {
  try {
    const stats = await lstat(destinationPath);
    if (stats.isSymbolicLink()) {
      throw new Error(`Refusing to follow symlinked temp file: ${destinationPath}`);
    }
  } catch (error) {
    if (error.code !== "ENOENT") {
      throw error;
    }
  }
}
function normalizeSymlinkError(error, destinationPath) {
  return error.code === "ELOOP" ? new Error(`Refusing to follow symlinked temp file: ${destinationPath}`) : error instanceof Error ? error : new Error(String(error));
}
function isWithinDirectory(rootDir, candidatePath) {
  return candidatePath === rootDir || candidatePath.startsWith(rootDir.endsWith(path.sep) ? rootDir : `${rootDir}${path.sep}`);
}
function isHttpEndpoint(endpoint) {
  return endpoint.startsWith("http://") || endpoint.startsWith("https://");
}
function isWebSocketEndpoint(endpoint) {
  try {
    const url = new URL(endpoint);
    return url.protocol === "ws:" || url.protocol === "wss:";
  } catch {
    return false;
  }
}
function endpointPort(endpoint) {
  try {
    const url = new URL(endpoint);
    const raw = url.port || (url.protocol === "https:" ? "443" : url.protocol === "http:" ? "80" : "");
    const port = Number.parseInt(raw, 10);
    return Number.isInteger(port) && port > 0 && port <= 65535 ? port : null;
  } catch {
    return null;
  }
}
async function titleWithTimeout(page) {
  let timeoutId;
  try {
    return await Promise.race([
      page.title(),
      new Promise((resolve) => {
        timeoutId = setTimeout(() => resolve(""), PAGE_TITLE_TIMEOUT_MS);
      })
    ]);
  } finally {
    if (timeoutId !== void 0) {
      clearTimeout(timeoutId);
    }
  }
}
async function withTimeout(promise, timeoutMs, name) {
  let timeoutId;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timeoutId = setTimeout(() => {
          const error = new Error(`${name}: exceeded ${timeoutMs}ms`);
          error.name = name;
          reject(error);
        }, timeoutMs);
      })
    ]);
  } finally {
    if (timeoutId !== void 0) {
      clearTimeout(timeoutId);
    }
  }
}
function formatArgs(args) {
  return `${args.map((arg) => {
    if (typeof arg === "string") {
      return arg;
    }
    if (arg instanceof Error) {
      return arg.stack ?? arg.message;
    }
    return inspect(arg, { depth: 4, breakLength: Infinity, colors: false });
  }).join(" ")}
`;
}
function formatError(error) {
  return error instanceof Error ? error.stack ?? error.message : String(error);
}
function buildAutoConnectError() {
  const launchCommand = process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --remote-debugging-port=9222" : process.platform === "win32" ? "chrome.exe --remote-debugging-port=9222" : "google-chrome --remote-debugging-port=9222";
  return [
    "Could not auto-discover a running Chrome instance with remote debugging enabled.",
    "Enable Chrome remote debugging at chrome://inspect/#remote-debugging",
    `or launch Chrome with: ${launchCommand}`
  ].join("\n");
}
function buildManualConnectError(endpoint) {
  return [
    `Could not resolve a CDP WebSocket endpoint from ${endpoint}.`,
    "If Chrome is using built-in remote debugging, connect with the exact ws://127.0.0.1:<port>/devtools/browser/... URL from DevToolsActivePort."
  ].join("\n");
}
function sanitizePipeSegment(value) {
  const sanitized = value.replace(/[^A-Za-z0-9._-]/g, "-").replace(/^-+|-+$/g, "").toLowerCase();
  return sanitized || "user";
}
manager = new LiveCdpBrowserManager();
startServer().catch((error) => {
  process.stderr.write(`Failed to start yoetz live-cdp daemon: ${formatError(error)}
`);
  process.exit(1);
});

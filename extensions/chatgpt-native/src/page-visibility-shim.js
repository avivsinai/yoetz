// ChatGPT (since 2026-09-01) defers hydration while document.hidden is true,
// so a background automation tab stays an unhydrated skeleton forever: the
// composer pill keeps its "Thinking effort" placeholder, the picker menu
// renders static text without interactive roles, and no activation can open
// it. This shim runs in the MAIN world at document_start, only in yoetz-owned
// tabs (matched by the ?_yoetz= URL marker), and presents the page as visible
// so hydration proceeds. The tab itself is never activated; the no-activation
// contract is unchanged.
(() => {
  try {
    const define = (name, value) => {
      Object.defineProperty(Document.prototype, name, {
        get: () => value,
        configurable: true
      });
    };
    define("hidden", false);
    define("visibilityState", "visible");
    define("webkitHidden", false);
    define("webkitVisibilityState", "visible");
    const swallow = (event) => event.stopImmediatePropagation();
    window.addEventListener("visibilitychange", swallow, true);
    document.addEventListener("visibilitychange", swallow, true);
    // Chrome freezes requestAnimationFrame in hidden tabs, which stalls
    // rAF-driven UI updates (the effort slider's label text lags its
    // aria-valuenow forever, so value/label consistency checks fail). Race
    // the native rAF (which wins in visible tabs, keeping real timing) with
    // a MessageChannel-paced pump: port messages are not timer-throttled in
    // hidden tabs, so frames keep flowing at ~16ms there too.
    const nativeRaf = window.requestAnimationFrame.bind(window);
    const nativeCancelRaf = window.cancelAnimationFrame.bind(window);
    let rafSequence = 0;
    const pending = new Map();
    const channel = new MessageChannel();
    let pumping = false;
    let lastFrameAt = 0;
    channel.port1.onmessage = () => {
      if (pending.size === 0) {
        pumping = false;
        return;
      }
      const now = performance.now();
      if (now - lastFrameAt >= 16) {
        lastFrameAt = now;
        for (const [id, entry] of Array.from(pending)) {
          pending.delete(id);
          nativeCancelRaf(entry.nativeId);
          try {
            entry.callback(now);
          } catch {
            // A throwing frame callback must not stop the pump.
          }
        }
      }
      channel.port2.postMessage(0);
    };
    window.requestAnimationFrame = (callback) => {
      rafSequence += 1;
      const id = rafSequence;
      const nativeId = nativeRaf((timestamp) => {
        if (pending.delete(id)) callback(timestamp);
      });
      pending.set(id, { callback, nativeId });
      if (!pumping) {
        pumping = true;
        channel.port2.postMessage(0);
      }
      return id;
    };
    window.cancelAnimationFrame = (id) => {
      const entry = pending.get(id);
      if (entry) {
        pending.delete(id);
        nativeCancelRaf(entry.nativeId);
      }
    };
  } catch {
    // If the override is refused, leave the page untouched and let the
    // recipe fail closed as before.
  }
})();

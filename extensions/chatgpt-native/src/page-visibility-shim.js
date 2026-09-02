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
    // Captured before the override so the shim can still tell whether the
    // tab is genuinely hidden.
    const nativeVisibility = Object.getOwnPropertyDescriptor(Document.prototype, "visibilityState")?.get;
    const reallyHidden = () => {
      try {
        return nativeVisibility ? nativeVisibility.call(document) !== "visible" : false;
      } catch {
        return false;
      }
    };
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
    // Chrome never delivers IntersectionObserver entries or idle callbacks to a
    // background tab, and parts of the composer header (the Chat/Work surface
    // radiogroup) mount lazily behind them. While the tab is GENUINELY hidden
    // (read through the native descriptor captured before the override),
    // deliver one synthetic intersecting entry per observe() — dropped if the
    // target was unobserved or the observer disconnected in the meantime —
    // and back requestIdleCallback with a short timer so those gates open.
    const NativeIntersectionObserver = window.IntersectionObserver;
    if (typeof NativeIntersectionObserver === "function") {
      const observerCallbacks = new WeakMap();
      const observerTargets = new WeakMap();
      window.IntersectionObserver = class YoetzIntersectionObserver extends NativeIntersectionObserver {
        constructor(callback, init) {
          super(callback, init);
          observerCallbacks.set(this, callback);
          observerTargets.set(this, new Set());
        }
        observe(target) {
          super.observe(target);
          const targets = observerTargets.get(this);
          targets?.add(target);
          if (!reallyHidden()) return;
          setTimeout(() => {
            if (!observerTargets.get(this)?.has(target)) return;
            const rect = target?.getBoundingClientRect?.()
              ?? { x: 0, y: 0, width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0 };
            const viewport = {
              x: 0, y: 0, top: 0, left: 0,
              width: window.innerWidth, height: window.innerHeight,
              right: window.innerWidth, bottom: window.innerHeight
            };
            try {
              observerCallbacks.get(this)?.([{
                target,
                isIntersecting: true,
                intersectionRatio: 1,
                time: performance.now(),
                boundingClientRect: rect,
                intersectionRect: rect,
                rootBounds: viewport
              }], this);
            } catch {
              // A throwing observer callback must not break the shim.
            }
          }, 0);
        }
        unobserve(target) {
          observerTargets.get(this)?.delete(target);
          super.unobserve(target);
        }
        disconnect() {
          observerTargets.get(this)?.clear();
          super.disconnect();
        }
      };
    }
    if (typeof window.requestIdleCallback === "function") {
      const nativeIdle = window.requestIdleCallback.bind(window);
      const nativeCancelIdle = window.cancelIdleCallback?.bind(window);
      // Every live request is registered (value: the fallback timer, or null
      // in a visible tab) so native delivery is never suppressed; only the
      // timer is conditional on the tab being genuinely hidden.
      const idleRequests = new Map();
      window.requestIdleCallback = (callback, options) => {
        const nativeId = nativeIdle((deadline) => {
          if (!idleRequests.has(nativeId)) return;
          const timer = idleRequests.get(nativeId);
          idleRequests.delete(nativeId);
          if (timer !== null) clearTimeout(timer);
          callback(deadline);
        }, options);
        const timer = reallyHidden()
          ? setTimeout(() => {
            if (!idleRequests.delete(nativeId)) return;
            nativeCancelIdle?.(nativeId);
            // Present a normal idle slice, not a timed-out one, so callers
            // do the real work instead of their degraded path.
            callback({ didTimeout: false, timeRemaining: () => 16 });
          }, Math.min(Number(options?.timeout) || 200, 200))
          : null;
        idleRequests.set(nativeId, timer);
        return nativeId;
      };
      window.cancelIdleCallback = (id) => {
        if (idleRequests.has(id)) {
          const timer = idleRequests.get(id);
          idleRequests.delete(id);
          if (timer !== null) clearTimeout(timer);
        }
        nativeCancelIdle?.(id);
      };
    }
    // The isolated-world driver cannot see React's fiber keys, so it cannot
    // tell the server-rendered skeleton (stable but handler-less) from the
    // hydrated page. Publish hydration through DOM attributes both worlds
    // share: a presence marker set synchronously at document_start (so the
    // driver knows a flag will follow and must not fall back to node
    // stability), then data-yoetz-hydrated once the model pill carries a
    // React fiber.
    document.documentElement.setAttribute("data-yoetz-shim", "1");
    const hydrationPoll = setInterval(() => {
      try {
        const pill = document.querySelector('button.__composer-pill[aria-haspopup="menu"]');
        if (pill && Object.keys(pill).some((key) => key.startsWith("__react"))) {
          document.documentElement.setAttribute("data-yoetz-hydrated", "1");
          clearInterval(hydrationPoll);
        }
      } catch {
        // Detection must never break the page.
      }
    }, 500);
    setTimeout(() => clearInterval(hydrationPoll), 120000);
  } catch {
    // If the override is refused, leave the page untouched and let the
    // recipe fail closed as before.
  }
})();

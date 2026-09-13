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

    // Parse a CSS margin shorthand (e.g. "0px 0px 100px 0px") into px values.
    // Used by the synthetic IntersectionObserver to respect rootMargin.
    function parseRootMargin(value) {
      const parts = String(value ?? "0px").trim().split(/\s+/);
      const nums = parts.map((p) => parseFloat(p) || 0);
      // CSS shorthand: 1=val, 2=v h, 3=t h b, 4=t r b l
      let top, right, bottom, left;
      if (nums.length === 1) { top = right = bottom = left = nums[0]; }
      else if (nums.length === 2) { top = bottom = nums[0]; right = left = nums[1]; }
      else if (nums.length === 3) { top = nums[0]; right = left = nums[1]; bottom = nums[2]; }
      else { top = nums[0]; right = nums[1]; bottom = nums[2]; left = nums[3]; }
      return { top, right, bottom, left };
    }
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
      const observerDelivered = new WeakMap();
      // The lazily mounted header only needs the assist during hydration;
      // after that window, background lazy-loaders (sidebar pagination,
      // media) keep their native behavior so a hidden tab cannot page
      // itself into the account rate limit.
      const assistUntil = performance.now() + 90000;
      window.IntersectionObserver = class YoetzIntersectionObserver extends NativeIntersectionObserver {
        constructor(callback, init) {
          super(callback, init);
          observerCallbacks.set(this, callback);
          observerTargets.set(this, new Set());
          observerDelivered.set(this, new WeakSet());
          // Parse rootMargin at construction so synthetic entries respect the
          // observer's effective root rectangle (yz-5bd).
          this._yoetzParsedRootMargin = parseRootMargin(init?.rootMargin);
        }
        observe(target) {
          super.observe(target);
          observerTargets.get(this)?.add(target);
          if (!reallyHidden() || performance.now() > assistUntil) return;
          if (observerDelivered.get(this)?.has(target)) return;
          setTimeout(() => {
            if (!reallyHidden()) return;
            if (!observerTargets.get(this)?.has(target)) return;
            if (observerDelivered.get(this)?.has(target)) return;
            observerDelivered.get(this)?.add(target);
            const rect = target?.getBoundingClientRect?.()
              ?? { x: 0, y: 0, width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0 };
            // Compute the actual intersection of the target rect with the
            // observer root (or the viewport when root is null). A synthetic
            // entry must reflect real geometry: an offscreen sentinel (e.g. a
            // history pagination trigger below the fold) must NOT receive a
            // positive intersection, which could trigger extra history loads
            // in a hidden tab (yz-5bd).
            //
            // Intersection status is independent of positive intersection area:
            // a zero-height element or edge-contact target is still
            // "intersecting" per the W3C spec. We compute status from the
            // overlap interval (>= 0), not from positive area (> 0).
            const viewportRect = {
              x: 0, y: 0, top: 0, left: 0,
              width: window.innerWidth, height: window.innerHeight,
              right: window.innerWidth, bottom: window.innerHeight
            };
            // Resolve the effective root rect. root can be null (viewport),
            // an Element, or a Document. A Document root uses the viewport.
            const root = this.root ?? null;
            let rootRect;
            if (root === null) {
              rootRect = viewportRect;
            } else if (root.nodeType === 9) { // Document.DOCUMENT_NODE
              rootRect = viewportRect;
            } else {
              rootRect = root.getBoundingClientRect?.() ?? null;
            }
            // Apply rootMargin (parsed from this.rootMargin at construction).
            const margin = this._yoetzParsedRootMargin ?? { top: 0, right: 0, bottom: 0, left: 0 };
            if (rootRect && (margin.top || margin.right || margin.bottom || margin.left)) {
              rootRect = {
                ...rootRect,
                top: rootRect.top - margin.top,
                left: rootRect.left - margin.left,
                right: rootRect.right + margin.right,
                bottom: rootRect.bottom + margin.bottom
              };
            }
            let intersectionRect;
            let isIntersecting;
            if (!rootRect) {
              intersectionRect = { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
              isIntersecting = false;
            } else {
              const ix = Math.max(rect.left ?? rect.x ?? 0, rootRect.left);
              const iy = Math.max(rect.top ?? rect.y ?? 0, rootRect.top);
              const ir = Math.min(rect.right ?? (rect.x + rect.width) ?? 0, rootRect.right);
              const ib = Math.min(rect.bottom ?? (rect.y + rect.height) ?? 0, rootRect.bottom);
              // Intersection status: intervals overlap (>= 0), independent of
              // positive area. A zero-height element at a valid position is
              // still intersecting per spec.
              const xOverlap = ir - ix;
              const yOverlap = ib - iy;
              isIntersecting = xOverlap >= 0 && yOverlap >= 0;
              const cw = Math.max(0, xOverlap);
              const ch = Math.max(0, yOverlap);
              // Per spec: when not intersecting, intersectionRect is all zeros.
              intersectionRect = isIntersecting
                ? { x: ix, y: iy, top: iy, left: ix, right: ir, bottom: ib, width: cw, height: ch }
                : { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
            }
            // Compute ratio without corrupting subpixel denominators.
            const targetArea = rect.width * rect.height;
            const intersectionArea = intersectionRect.width * intersectionRect.height;
            const intersectionRatio = targetArea > 0
              ? Math.min(1, intersectionArea / targetArea)
              : (isIntersecting ? 1 : 0);
            try {
              observerCallbacks.get(this)?.call(this, [{
                target,
                isIntersecting,
                intersectionRatio,
                time: performance.now(),
                boundingClientRect: rect,
                intersectionRect,
                rootBounds: rootRect ?? null
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
            // Present a normal, shrinking idle slice (not a timed-out one) so
            // callers do the real work and their time-slicing loops end.
            const sliceStart = performance.now();
            callback({
              didTimeout: false,
              timeRemaining: () => Math.max(0, 16 - (performance.now() - sliceStart))
            });
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
        // Same shape the driver's findModelButton accepts: any composer menu
        // trigger, not only the current pill class, so class drift alone
        // cannot leave the flag unset.
        const pills = document.querySelectorAll('button.__composer-pill[aria-haspopup="menu"], form button[aria-haspopup="menu"]');
        const hydrated = Array.from(pills).some((pill) => Object.keys(pill).some((key) => key.startsWith("__react")));
        if (hydrated) {
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

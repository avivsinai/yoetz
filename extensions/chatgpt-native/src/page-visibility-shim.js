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
    // Probe tabs (?_yoetz=probe*) publish hydration breadcrumbs and run a
    // scripted picker self-test, streaming results through the tab title,
    // which is readable from outside the page without any script permission;
    // recipe tabs are unaffected.
    if (/[?&]_yoetz=probe/.test(location.search)) {
      const log = [];
      const note = (entry) => {
        log.push(entry);
        if (log.length > 8) log.shift();
      };
      setInterval(() => {
        try {
          document.title = log.join(" >> ") || "probe-boot";
        } catch {
          // Never let diagnostics break the page.
        }
      }, 1000);
      const fireClick = (el) => {
        if (!el) return false;
        for (const [type, Ctor, init] of [
          ["pointerdown", PointerEvent, { button: 0, buttons: 1, pointerId: 1, pointerType: "mouse", isPrimary: true }],
          ["mousedown", MouseEvent, { button: 0, buttons: 1 }],
          ["pointerup", PointerEvent, { button: 0, buttons: 0, pointerId: 1, pointerType: "mouse", isPrimary: true }],
          ["mouseup", MouseEvent, { button: 0, buttons: 0 }],
          ["click", MouseEvent, { button: 0, detail: 1 }]
        ]) {
          el.dispatchEvent(new Ctor(type, { bubbles: true, cancelable: true, composed: true, ...init }));
        }
        return true;
      };
      const sliderInfo = () => {
        const menu = document.querySelector('[role="menu"][data-state="open"]');
        const slider = menu?.querySelector('[role="slider"]');
        const label = (menu?.textContent || "").match(/[A-Z][A-Za-z ]{1,24}, \d+ of \d+\./);
        return `v=${slider?.getAttribute("aria-valuenow") ?? "-"} mounted=${slider ? document.contains(slider) : "-"} label=${label ? label[0] : "-"}`;
      };
      const sleepMs = (ms) => new Promise((r) => setTimeout(r, ms));
      setTimeout(async () => {
        try {
          const pill = document.querySelector(".__composer-pill");
          note(`pill=${pill ? pill.textContent.trim().slice(0, 20) : "none"}`);
          fireClick(pill);
          await sleepMs(2000);
          note(`opened:${sliderInfo()}`);
          const menu = document.querySelector('[role="menu"][data-state="open"]');
          const slider0 = menu?.querySelector('[role="slider"]');
          const toggle = [...(menu?.querySelectorAll('[role="menuitem"]') ?? [])]
            .find((x) => (x.getAttribute("aria-label") || "") === "Select model");
          note(`toggle=${toggle ? "yes" : "no"}`);
          fireClick(toggle);
          await sleepMs(2000);
          note(`expanded=${toggle?.getAttribute("aria-expanded")}:${sliderInfo()} s0mounted=${slider0 ? document.contains(slider0) : "-"}`);
          const openMenu = document.querySelector('[role="menu"][data-state="open"]');
          const rows = [...(openMenu?.querySelectorAll("*") ?? [])]
            .filter((el) => el.getAttribute("role") || el.getAttribute("data-testid") || el.getAttribute("inert") != null)
            .map((el) => {
              const bits = [el.getAttribute("role") || el.getAttribute("data-testid") || "inertdiv"];
              if (el.getAttribute("aria-checked")) bits.push(`ck=${el.getAttribute("aria-checked")}`);
              if (el.getAttribute("aria-disabled")) bits.push(`dis=${el.getAttribute("aria-disabled")}`);
              if (el.getAttribute("data-disabled") != null) bits.push("ddis");
              if (el.getAttribute("aria-describedby")) bits.push(`db=${(document.getElementById(el.getAttribute("aria-describedby"))?.textContent || "?").slice(0, 40)}`);
              if (el.getAttribute("inert") != null) bits.push("inert");
              if (el.getAttribute("aria-valuenow") != null) bits.push(`v=${el.getAttribute("aria-valuenow")}/${el.getAttribute("aria-valuemax")}`);
              if (el.getAttribute("aria-label")) bits.push(`al=${el.getAttribute("aria-label").slice(0, 14)}`);
              const own = [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent.trim()).filter(Boolean).join(" ");
              const direct = [...el.children].filter((c) => c.children.length === 0).map((c) => c.textContent.trim()).filter(Boolean).join(" ");
              const text = (own || direct).slice(0, 18);
              if (text) bits.push(text);
              return bits.join(",");
            });
          note(`DOM[${rows.join(";")}]`);
          const findPro = () => [...(document.querySelectorAll('[role="menu"][data-state="open"] [role="menuitemradio"]') ?? [])]
            .find((el) => (el.getAttribute("aria-label") || el.textContent || "").trim().toLowerCase() === "pro");
          const proChecked = () => findPro()?.getAttribute("aria-checked");
          const hover = (el) => {
            for (const [type, Ctor] of [["pointerenter", PointerEvent], ["mouseenter", MouseEvent], ["pointermove", PointerEvent], ["mousemove", MouseEvent], ["pointerover", PointerEvent], ["mouseover", MouseEvent]]) {
              el.dispatchEvent(new Ctor(type, { bubbles: true, cancelable: true, composed: true, pointerId: 1, pointerType: "mouse", isPrimary: true }));
            }
          };
          const keyOn = (el, k) => {
            el.focus?.();
            el.dispatchEvent(new KeyboardEvent("keydown", { key: k, code: k === " " ? "Space" : k, bubbles: true, cancelable: true, composed: true }));
            el.dispatchEvent(new KeyboardEvent("keyup", { key: k, code: k === " " ? "Space" : k, bubbles: true, cancelable: true, composed: true }));
          };
          let pro = findPro();
          note(`proRadio=${pro ? "yes" : "no"}`);
          if (pro) { hover(pro); fireClick(pro); }
          await sleepMs(1500);
          note(`afterHoverClick=${proChecked()}`);
          if (proChecked() !== "true") { pro = findPro(); if (pro) keyOn(pro, "Enter"); await sleepMs(1500); note(`afterEnter=${proChecked()}`); }
          if (proChecked() !== "true") { pro = findPro(); if (pro) keyOn(pro, " "); await sleepMs(1500); note(`afterSpace=${proChecked()}`); }
          const checks = [...(document.querySelectorAll('[role="menu"][data-state="open"] [role="menuitemradio"]') ?? [])]
            .map((el) => `${(el.getAttribute("aria-label") || el.textContent || "").trim().slice(0, 12)}=${el.getAttribute("aria-checked")}`);
          note(`final:[${checks.join(",")}] ${sliderInfo()}`);
          document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true, composed: true }));
          await sleepMs(2000);
          const pillAfter = document.querySelector(".__composer-pill");
          note(`closedPill=${pillAfter ? pillAfter.textContent.trim().slice(0, 24) : "none"} menuOpen=${Boolean(document.querySelector('[role="menu"][data-state="open"]'))}`);
        } catch (error) {
          note(`err=${String(error).slice(0, 60)}`);
        }
      }, 25000);
    }
  } catch {
    // If the override is refused, leave the page untouched and let the
    // recipe fail closed as before.
  }
})();

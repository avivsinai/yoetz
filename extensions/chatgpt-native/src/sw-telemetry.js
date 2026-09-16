// yz-9pf: Service-worker error + restart telemetry.
//
// A yz-4hr-style worker restart is currently undiagnosable after the fact.
// This module records, with zero behaviour change:
//   - error/unhandledrejection records (ring of the last 5) in
//     chrome.storage.session: { at_ms, kind, message, stack_head,
//     job_ids_active }
//   - one start record per worker start in chrome.storage.session:
//     { started_at_ms, reason } where reason is "startup" | "installed" |
//     "restart"
//   - a monotonically increasing start_count in chrome.storage.local
//
// No retries, no new dependency. All functions are defensive: telemetry
// must never break the worker.

const SW_LAST_ERRORS_KEY = "yoetz_sw_last_errors";
const SW_LAST_START_KEY = "yoetz_sw_last_start";
const SW_START_COUNT_KEY = "yoetz_sw_start_count";
const SW_ERROR_RING_SIZE = 5;
const STACK_HEAD_FRAMES = 3;

function storageArea(area) {
  return typeof chrome !== "undefined" && chrome?.storage?.[area] ? chrome.storage[area] : null;
}

function activeJobIdList(getActiveJobIds) {
  try {
    const ids = getActiveJobIds?.() ?? [];
    return Array.isArray(ids) ? ids.map(String).slice(0, 20) : [];
  } catch {
    return [];
  }
}

function stackHead(stack) {
  if (typeof stack !== "string" || !stack) {
    return "";
  }
  return stack.split("\n").slice(0, 1 + STACK_HEAD_FRAMES).join("\n");
}

export async function recordSwError(kind, error, getActiveJobIds) {
  try {
    const session = storageArea("session");
    if (!session) {
      return;
    }
    const existing = ((await session.get(SW_LAST_ERRORS_KEY)) ?? {})[SW_LAST_ERRORS_KEY];
    const ring = Array.isArray(existing) ? existing : [];
    ring.push({
      at_ms: Date.now(),
      kind,
      message: String(error?.message ?? error ?? "").slice(0, 500),
      stack_head: stackHead(error?.stack),
      job_ids_active: activeJobIdList(getActiveJobIds)
    });
    while (ring.length > SW_ERROR_RING_SIZE) {
      ring.shift();
    }
    await session.set({ [SW_LAST_ERRORS_KEY]: ring });
  } catch {
    // Telemetry must never break the worker.
  }
}

export async function recordSwStart(reason) {
  try {
    const session = storageArea("session");
    const local = storageArea("local");
    const started_at_ms = Date.now();
    if (session) {
      await session.set({ [SW_LAST_START_KEY]: { started_at_ms, reason } });
    }
    if (local) {
      const stored = ((await local.get(SW_START_COUNT_KEY)) ?? {})[SW_START_COUNT_KEY];
      const start_count = Number.isFinite(Number(stored)) ? Number(stored) + 1 : 1;
      await local.set({ [SW_START_COUNT_KEY]: start_count });
    }
  } catch {
    // Telemetry must never break the worker.
  }
}

export function swStartReason({ onStartupFired, onInstalledReason } = {}) {
  if (onInstalledReason === "chrome_update" || onInstalledReason === "shared_module_update") {
    return "installed";
  }
  if (onInstalledReason === "install" || onInstalledReason === "update") {
    return "installed";
  }
  if (onStartupFired) {
    return "startup";
  }
  return "restart";
}

// yz-9pf: overwrite the reason on the start record written by the module-init
// recordSwStart, WITHOUT incrementing start_count. The onInstalled/onStartup
// listeners call this instead of recordSwStart so a single worker start is
// counted exactly once. The read-modify-write races nothing that matters:
// only these listeners ever refine, the count write is untouched, and the
// CLI reads the record long after both have settled.
export async function refineSwStartReason(reason) {
  try {
    const session = storageArea("session");
    if (!session) {
      return;
    }
    const existing = ((await session.get(SW_LAST_START_KEY)) ?? {})[SW_LAST_START_KEY];
    if (!existing || typeof existing !== "object") {
      return;
    }
    await session.set({ [SW_LAST_START_KEY]: { ...existing, reason } });
  } catch {
    // Telemetry must never break the worker.
  }
}

export const SW_TELEMETRY_KEYS = {
  lastErrors: SW_LAST_ERRORS_KEY,
  lastStart: SW_LAST_START_KEY,
  startCount: SW_START_COUNT_KEY
};

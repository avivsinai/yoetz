const LABELS = {
  connected: "Connected",
  disconnected: "Disconnected",
  missing_native_host: "Missing native host",
  version_mismatch: "Version mismatch",
  manual_handoff: "Manual handoff",
  missing_extension: "Missing extension hello",
  restore_failed: "Restore failed",
  state_lost: "State lost",
  failed: "Job failed"
};

const dot = document.querySelector("#dot");
const label = document.querySelector("#label");
const detail = document.querySelector("#detail");
const reconnect = document.querySelector("#reconnect");

async function refresh() {
  try {
    const status = await chrome.runtime.sendMessage({ type: "yoetz_popup_status" });
    const value = status?.status ?? "disconnected";
    dot.className = `dot ${value}`;
    label.textContent = LABELS[value] ?? value;
    detail.textContent = status?.detail ?? "";
  } catch (error) {
    dot.className = "dot disconnected";
    label.textContent = "Waking...";
    detail.textContent = String(error?.message ?? error);
  }
}

reconnect.addEventListener("click", async () => {
  try {
    await chrome.runtime.sendMessage({ type: "yoetz_reconnect" });
  } catch {
    // Refresh below will show the current service-worker/native-host state.
  }
  await refresh();
});

refresh();

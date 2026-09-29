// Shared ChatGPT surface-proof receipt rules used by the service worker and
// contract tests. Keep in lockstep with crates/yoetz-cli/src/chatgpt_web.rs
// validate_chatgpt_final_model_selection surface_proof_kind branches.

export function validateChatgptSurfaceProofReceipt(receipt) {
  const observed = Array.isArray(receipt?.surface_observed_values)
    ? receipt.surface_observed_values
    : [];
  const labels = Array.isArray(receipt?.surface_observed_labels)
    ? receipt.surface_observed_labels
    : [];
  const chatState = receipt?.surface_chat_state;
  const workState = receipt?.surface_work_state;

  if (receipt?.surface_proof_kind === "explicit_chat_work_radios") {
    if (receipt.surface_evidence_seen !== true
        || receipt.surface_visible_toggle_count !== 2
        || chatState?.aria_checked !== "true"
        || workState?.aria_checked !== "false"
        || !observed.includes("chatgpt")
        || !observed.includes("work")) {
      return "explicit Chat/Work surface proof is incomplete";
    }
    if (receipt.surface_composer_aria !== null && receipt.surface_composer_aria !== undefined) {
      return "explicit surface proof must not claim implicit composer proof";
    }
    return null;
  }

  if (receipt?.surface_proof_kind === "explicit_composer_mode_buttons") {
    if (receipt.surface_evidence_seen !== true
        || receipt.surface_visible_toggle_count !== 2
        || chatState?.aria_pressed !== "true"
        || workState?.aria_pressed !== "false"
        || chatState?.aria_checked != null
        || workState?.aria_checked != null
        || receipt.surface_foreign_pressed !== false
        || labels.length !== 2
        || !labels.includes("Chat")
        || !labels.includes("Work")
        || observed.length !== 0) {
      return "explicit Composer mode surface proof is incomplete";
    }
    if (receipt.surface_composer_aria !== null && receipt.surface_composer_aria !== undefined) {
      return "explicit surface proof must not claim implicit composer proof";
    }
    return null;
  }

  if (receipt?.surface_proof_kind === "implicit_chat_composer_aria") {
    if (receipt.surface_evidence_seen !== false
        || receipt.surface_visible_toggle_count !== 0
        || observed.length !== 0
        || receipt.surface_composer_aria !== "Chat with ChatGPT"
        || chatState !== null
        || workState !== null) {
      return "implicit Chat composer proof is incomplete";
    }
    return null;
  }

  return `surface_proof_kind=${JSON.stringify(receipt?.surface_proof_kind)}`;
}

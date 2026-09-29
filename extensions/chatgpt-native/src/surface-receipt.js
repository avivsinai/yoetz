// Shared ChatGPT surface-proof receipt rules used by the service worker and
// contract tests. Keep in lockstep with crates/yoetz-cli/src/chatgpt_web.rs
// validate_chatgpt_final_model_selection surface_proof_kind branches.

function fmt(value) {
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function fail(parts) {
  return parts.filter(Boolean).join("; ");
}

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
    const problems = [];
    if (receipt.surface_evidence_seen !== true) {
      problems.push(`surface_evidence_seen=${fmt(receipt.surface_evidence_seen)}, expected true`);
    }
    if (receipt.surface_visible_toggle_count !== 2) {
      problems.push(`surface_visible_toggle_count=${fmt(receipt.surface_visible_toggle_count)}, expected 2`);
    }
    if (chatState?.aria_checked !== "true") {
      problems.push(`surface_chat_state.aria_checked=${fmt(chatState?.aria_checked)}, expected "true"`);
    }
    if (workState?.aria_checked !== "false") {
      problems.push(`surface_work_state.aria_checked=${fmt(workState?.aria_checked)}, expected "false"`);
    }
    if (!observed.includes("chatgpt")) {
      problems.push(`surface_observed_values missing "chatgpt": ${fmt(observed)}`);
    }
    if (!observed.includes("work")) {
      problems.push(`surface_observed_values missing "work": ${fmt(observed)}`);
    }
    if (receipt.surface_composer_aria !== null && receipt.surface_composer_aria !== undefined) {
      problems.push(`surface_composer_aria=${fmt(receipt.surface_composer_aria)}, expected null`);
    }
    return problems.length
      ? fail(["explicit Chat/Work surface proof is incomplete", ...problems])
      : null;
  }

  if (receipt?.surface_proof_kind === "explicit_composer_mode_buttons") {
    const problems = [];
    if (receipt.surface_evidence_seen !== true) {
      problems.push(`surface_evidence_seen=${fmt(receipt.surface_evidence_seen)}, expected true`);
    }
    if (receipt.surface_visible_toggle_count !== 2) {
      problems.push(`surface_visible_toggle_count=${fmt(receipt.surface_visible_toggle_count)}, expected 2`);
    }
    if (chatState?.aria_pressed !== "true") {
      problems.push(`surface_chat_state.aria_pressed=${fmt(chatState?.aria_pressed)}, expected "true"`);
    }
    if (workState?.aria_pressed !== "false") {
      problems.push(`surface_work_state.aria_pressed=${fmt(workState?.aria_pressed)}, expected "false"`);
    }
    if (chatState?.aria_checked != null) {
      problems.push(`surface_chat_state.aria_checked=${fmt(chatState?.aria_checked)}, expected null`);
    }
    if (workState?.aria_checked != null) {
      problems.push(`surface_work_state.aria_checked=${fmt(workState?.aria_checked)}, expected null`);
    }
    if (receipt.surface_foreign_pressed !== false) {
      problems.push(`surface_foreign_pressed=${fmt(receipt.surface_foreign_pressed)}, expected false`);
    }
    if (labels.length !== 2 || !labels.includes("Chat") || !labels.includes("Work")) {
      problems.push(`surface_observed_labels=${fmt(labels)}, expected ["Chat","Work"]`);
    }
    if (observed.length !== 0) {
      problems.push(`surface_observed_values=${fmt(observed)}, expected []`);
    }
    if (receipt.surface_composer_aria !== null && receipt.surface_composer_aria !== undefined) {
      problems.push(`surface_composer_aria=${fmt(receipt.surface_composer_aria)}, expected null`);
    }
    return problems.length
      ? fail(["explicit Composer mode surface proof is incomplete", ...problems])
      : null;
  }

  if (receipt?.surface_proof_kind === "implicit_chat_composer_aria") {
    const problems = [];
    if (receipt.surface_evidence_seen !== false) {
      problems.push(`surface_evidence_seen=${fmt(receipt.surface_evidence_seen)}, expected false`);
    }
    if (receipt.surface_visible_toggle_count !== 0) {
      problems.push(`surface_visible_toggle_count=${fmt(receipt.surface_visible_toggle_count)}, expected 0`);
    }
    if (observed.length !== 0) {
      problems.push(`surface_observed_values=${fmt(observed)}, expected []`);
    }
    if (receipt.surface_composer_aria !== "Chat with ChatGPT") {
      problems.push(`surface_composer_aria=${fmt(receipt.surface_composer_aria)}, expected "Chat with ChatGPT"`);
    }
    if (chatState !== null) {
      problems.push(`surface_chat_state=${fmt(chatState)}, expected null`);
    }
    if (workState !== null) {
      problems.push(`surface_work_state=${fmt(workState)}, expected null`);
    }
    return problems.length
      ? fail(["implicit Chat composer proof is incomplete", ...problems])
      : null;
  }

  return `surface_proof_kind=${fmt(receipt?.surface_proof_kind)}`;
}

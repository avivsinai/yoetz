export const PROTOCOL_VERSION = 1;
export const TRANSPORT = "chrome-extension-native";
export const EXTENSION_ID = "njdakhppfigmloihiikbjmheejfndbfa";
export const NATIVE_HOST = "com.yoetz.chatgpt_native";

export const MESSAGE_TYPES = Object.freeze([
  "hello",
  "pair_request",
  "pair_complete",
  "job_start",
  "job_progress",
  "job_file_chunk",
  "job_file_chunk_ack",
  "job_cancel",
  "job_complete",
  "job_error",
  "heartbeat",
  "reconnect",
  "inspect_run",
  "request_identity_permission"
]);

export function nowIso() {
  return new Date().toISOString();
}

export function isObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

export function makeEnvelope(type, fields = {}) {
  if (!MESSAGE_TYPES.includes(type)) {
    throw new Error(`unknown Yoetz native message type: ${type}`);
  }
  return {
    protocol_version: PROTOCOL_VERSION,
    transport: TRANSPORT,
    type,
    request_id: fields.request_id ?? `req_${cryptoRandomId()}`,
    job_id: fields.job_id,
    run_id: fields.run_id,
    workspace_id: fields.workspace_id,
    capability_token: fields.capability_token,
    payload: fields.payload ?? {},
    created_at: fields.created_at ?? nowIso()
  };
}

export function validateEnvelope(message, options = {}) {
  if (!isObject(message)) {
    return { ok: false, code: "invalid_json", message: "message must be a JSON object" };
  }
  if (message.protocol_version !== PROTOCOL_VERSION) {
    return {
      ok: false,
      code: "version_mismatch",
      message: `expected protocol_version ${PROTOCOL_VERSION}`
    };
  }
  if (message.transport !== TRANSPORT) {
    return { ok: false, code: "wrong_transport", message: `expected transport ${TRANSPORT}` };
  }
  if (!MESSAGE_TYPES.includes(message.type)) {
    return { ok: false, code: "unknown_type", message: `unknown message type ${message.type}` };
  }
  if (!message.request_id || typeof message.request_id !== "string") {
    return { ok: false, code: "missing_request_id", message: "request_id is required" };
  }
  if (options.requireCapabilityToken && !message.capability_token) {
    return { ok: false, code: "missing_capability_token", message: "capability_token is required" };
  }
  if (options.capabilityToken && message.capability_token !== options.capabilityToken) {
    return { ok: false, code: "capability_mismatch", message: "capability_token mismatch" };
  }
  if (message.type.startsWith("job_") && message.type !== "job_progress" && !message.job_id) {
    return { ok: false, code: "missing_job_id", message: "job_id is required for job messages" };
  }
  if (message.payload !== undefined && !isObject(message.payload)) {
    return { ok: false, code: "invalid_payload", message: "payload must be an object" };
  }
  return { ok: true };
}

export function progress(job, phase, detail = {}) {
  return makeEnvelope("job_progress", {
    job_id: job.job_id,
    run_id: job.run_id,
    workspace_id: job.workspace_id,
    capability_token: job.capability_token,
    payload: {
      phase,
      ...detail
    }
  });
}

export function errorEnvelope(job, code, message, detail = {}) {
  const { request_id: requestId, ...payloadDetail } = detail;
  return makeEnvelope("job_error", {
    request_id: requestId ?? job?.request_id,
    job_id: job?.job_id,
    run_id: job?.run_id,
    workspace_id: job?.workspace_id,
    capability_token: job?.capability_token,
    payload: {
      code,
      message,
      ...payloadDetail
    }
  });
}

function cryptoRandomId() {
  const bytes = new Uint8Array(12);
  const cryptoApi = globalThis.crypto;
  if (cryptoApi?.getRandomValues) {
    cryptoApi.getRandomValues(bytes);
  } else {
    for (let i = 0; i < bytes.length; i += 1) {
      bytes[i] = Math.floor(Math.random() * 256);
    }
  }
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

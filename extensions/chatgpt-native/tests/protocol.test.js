import assert from "node:assert/strict";
import test from "node:test";
import {
  EXTENSION_ID,
  MESSAGE_TYPES,
  PROTOCOL_VERSION,
  TRANSPORT,
  makeEnvelope,
  validateEnvelope
} from "../src/protocol.js";

test("protocol exports the pinned extension id", () => {
  assert.equal(EXTENSION_ID, "njdakhppfigmloihiikbjmheejfndbfa");
});

test("makeEnvelope builds a valid Yoetz protocol message", () => {
  const envelope = makeEnvelope("job_start", {
    request_id: "req_test",
    job_id: "job_test",
    run_id: "run_test",
    capability_token: "secret",
    payload: { prompt: "Review this" }
  });
  assert.equal(envelope.protocol_version, PROTOCOL_VERSION);
  assert.equal(envelope.transport, TRANSPORT);
  assert.equal(envelope.type, "job_start");
  assert.deepEqual(validateEnvelope(envelope, { capabilityToken: "secret" }), { ok: true });
});

test("all required native message types are declared", () => {
  for (const type of [
    "job_start",
    "job_file_chunk",
    "job_cancel",
    "job_progress",
    "job_complete",
    "job_error",
    "heartbeat",
    "reconnect"
  ]) {
    assert.ok(MESSAGE_TYPES.includes(type), `${type} missing`);
  }
});

test("validateEnvelope rejects malformed messages", () => {
  assert.equal(validateEnvelope(null).code, "invalid_json");
  assert.equal(validateEnvelope({ protocol_version: 2, transport: TRANSPORT, type: "hello", request_id: "r" }).code, "version_mismatch");
  assert.equal(validateEnvelope({ protocol_version: 1, transport: "other", type: "hello", request_id: "r" }).code, "wrong_transport");
  assert.equal(validateEnvelope({ protocol_version: 1, transport: TRANSPORT, type: "job_start", request_id: "r" }).code, "missing_job_id");
});

import assert from "node:assert/strict";
import test from "node:test";
import { ChunkAssembler, uint8ArrayToBase64 } from "../src/chunks.js";

function chunk(sequence, totalChunks, bytes, extra = {}) {
  return {
    job_id: "job_1",
    payload: {
      sequence,
      total_chunks: totalChunks,
      total_bytes: 12,
      bytes_base64: uint8ArrayToBase64(bytes),
      filename: "bundle.md",
      mime_type: "text/markdown",
      ...extra
    }
  };
}

test("ChunkAssembler reassembles chunks in sequence order", () => {
  const assembler = new ChunkAssembler({ maxBytes: 20 });
  assert.equal(assembler.accept(chunk(1, 2, new TextEncoder().encode("world!"))).complete, false);
  assert.equal(assembler.accept(chunk(0, 2, new TextEncoder().encode("hello "))).complete, true);
  const file = assembler.takeFile("job_1");
  assert.equal(file.filename, "bundle.md");
  assert.equal(new TextDecoder().decode(file.bytes), "hello world!");
});

test("ChunkAssembler ignores duplicate chunks without double-counting", () => {
  const assembler = new ChunkAssembler({ maxBytes: 20 });
  const first = chunk(0, 1, new TextEncoder().encode("hello world"), { total_bytes: 11 });
  assert.equal(assembler.accept(first).received_bytes, 11);
  assert.equal(assembler.accept(first).received_bytes, 11);
  assert.equal(new TextDecoder().decode(assembler.takeFile("job_1").bytes), "hello world");
});

test("ChunkAssembler rejects invalid metadata", () => {
  const assembler = new ChunkAssembler({ maxBytes: 5 });
  assert.throws(() => assembler.accept(chunk(0, 1, new Uint8Array(), { total_bytes: 6 })), /invalid total_bytes/);
  assert.throws(() => assembler.accept(chunk(2, 1, new Uint8Array(), { total_bytes: 0 })), /invalid sequence/);
});

test("ChunkAssembler rejects native messages above the extension inbound budget", () => {
  const assembler = new ChunkAssembler({ maxBytes: 2 * 1024 * 1024 });
  assert.throws(
    () => assembler.accept(chunk(0, 1, new Uint8Array(), {
      total_bytes: 1,
      bytes_base64: "x".repeat(1024 * 1024 + 1)
    })),
    /oversize_chunk/
  );
});

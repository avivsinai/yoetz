const DEFAULT_MAX_CHUNKS = 100000;
export const MAX_BUNDLE_BYTES = 10 * 1024 * 1024;
export const MAX_NATIVE_INBOUND_BYTES = 1024 * 1024;

export class ChunkAssembler {
  constructor({ maxBytes, maxChunks = DEFAULT_MAX_CHUNKS } = {}) {
    this.maxBytes = maxBytes ?? MAX_BUNDLE_BYTES;
    this.maxChunks = maxChunks;
    this.jobs = new Map();
  }

  accept(message) {
    const payload = message.payload ?? {};
    const jobId = message.job_id;
    if (!jobId) {
      throw new Error("job_file_chunk missing job_id");
    }
    const sequence = numberField(payload.sequence, "sequence");
    const totalChunks = numberField(payload.total_chunks, "total_chunks");
    const totalBytes = numberField(payload.total_bytes, "total_bytes");
    const filename = payload.filename ? stringField(payload.filename, "filename") : "yoetz-bundle.md";
    const mimeType = payload.mime_type ? stringField(payload.mime_type, "mime_type") : "text/markdown";

    if (totalChunks < 1 || totalChunks > this.maxChunks) {
      throw new Error(`invalid total_chunks ${totalChunks}`);
    }
    if (sequence < 0 || sequence >= totalChunks) {
      throw new Error(`invalid sequence ${sequence} for ${totalChunks} chunks`);
    }
    if (totalBytes < 0 || totalBytes > this.maxBytes) {
      throw new Error(`invalid total_bytes ${totalBytes}`);
    }
    const bytesBase64 = base64Field(payload.bytes_base64, "bytes_base64");
    if (bytesBase64.length > MAX_NATIVE_INBOUND_BYTES) {
      throw new Error(`oversize_chunk: bytes_base64 is ${bytesBase64.length} bytes`);
    }

    const bytes = base64ToUint8Array(bytesBase64);
    let state = this.jobs.get(jobId);
    if (!state) {
      state = {
        totalChunks,
        totalBytes,
        filename,
        mimeType,
        chunks: new Array(totalChunks),
        receivedBytes: 0,
        receivedCount: 0
      };
      this.jobs.set(jobId, state);
    }
    if (state.totalChunks !== totalChunks || state.totalBytes !== totalBytes) {
      throw new Error("chunk metadata changed for job");
    }
    if (!state.chunks[sequence]) {
      state.chunks[sequence] = bytes;
      state.receivedBytes += bytes.byteLength;
      state.receivedCount += 1;
    }
    if (state.receivedBytes > state.totalBytes) {
      throw new Error("received more bytes than declared");
    }

    const complete = state.receivedCount === state.totalChunks;
    return {
      job_id: jobId,
      sequence,
      complete,
      received_chunks: state.receivedCount,
      total_chunks: state.totalChunks,
      received_bytes: state.receivedBytes,
      total_bytes: state.totalBytes
    };
  }

  takeFile(jobId) {
    const state = this.jobs.get(jobId);
    if (!state) {
      throw new Error(`no chunk state for ${jobId}`);
    }
    if (state.receivedCount !== state.totalChunks) {
      throw new Error(`job ${jobId} is missing chunks`);
    }
    if (state.receivedBytes !== state.totalBytes) {
      throw new Error(`job ${jobId} declared ${state.totalBytes} bytes but received ${state.receivedBytes}`);
    }
    const bytes = new Uint8Array(state.totalBytes);
    let offset = 0;
    for (const chunk of state.chunks) {
      if (!chunk) {
        throw new Error(`job ${jobId} has a missing chunk`);
      }
      bytes.set(chunk, offset);
      offset += chunk.byteLength;
    }
    this.jobs.delete(jobId);
    return {
      filename: state.filename,
      mimeType: state.mimeType,
      bytes
    };
  }

  discard(jobId) {
    this.jobs.delete(jobId);
  }
}

export function base64ToUint8Array(value) {
  if (typeof Buffer !== "undefined") {
    return new Uint8Array(Buffer.from(value, "base64"));
  }
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export function uint8ArrayToBase64(bytes) {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(bytes).toString("base64");
  }
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

function numberField(value, name) {
  if (!Number.isSafeInteger(value)) {
    throw new Error(`${name} must be an integer`);
  }
  return value;
}

function stringField(value, name) {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${name} must be a non-empty string`);
  }
  return value;
}

function base64Field(value, name) {
  if (typeof value !== "string") {
    throw new Error(`${name} must be a string`);
  }
  return value;
}

// ChatGPT-only freeze-proof extraction via the same-origin conversation API.
// The worker decides when to use this fallback; this adapter owns the endpoint,
// authentication, active-lineage freshness, and answer-node semantics.
export async function fetchConversationAnswer({
  job,
  requestedConversationId,
  parseOwnedWindowName,
  assertJobOwnership,
  expectedConversationId,
  locationHref,
  commandError
}) {
  assertJobOwnership(job, parseOwnedWindowName);
  const conversationId = String(requestedConversationId ?? "").trim()
    || expectedConversationId
    || conversationIdFromUrl(locationHref);
  if (!conversationId) {
    throw backendApiError(commandError, "backend_api_unavailable", "no conversation id available for backend-api read");
  }
  const token = await fetchChatgptAccessToken();
  if (!token) {
    throw backendApiError(commandError, "backend_api_unauthorized", "no ChatGPT access token (session expired or signed out)");
  }
  const data = await requestConversationAnswer(conversationId, token, commandError);
  return resolveBackendAnswer(job, conversationId, data);
}

// GET /backend-api/conversation/<id>. An explicit 429 is the server telling us
// we are making requests too quickly: surface it as typed backend_api_throttled
// with the HTTP facts (status, Retry-After parsed to ms, endpoint category) so
// the worker can pace ALL website reads behind a shared cooldown. 401/403 keep
// their unauthorized meaning; other non-OK statuses stay generic unavailable.
async function requestConversationAnswer(conversationId, token, commandError) {
  let response;
  try {
    response = await fetch(`/backend-api/conversation/${encodeURIComponent(conversationId)}`, {
      method: "GET",
      credentials: "include",
      headers: { Authorization: `Bearer ${token}`, Accept: "application/json" }
    });
  } catch (error) {
    throw backendApiError(commandError, "backend_api_unavailable", `backend-api conversation fetch failed: ${String(error?.message ?? error)}`);
  }
  if (response.status === 429) {
    throw backendApiThrottledError(commandError, "conversation", response);
  }
  if (response.status === 401 || response.status === 403) {
    throw backendApiError(commandError, "backend_api_unauthorized", `backend-api conversation returned ${response.status}`);
  }
  if (!response.ok) {
    throw backendApiError(commandError, "backend_api_unavailable", `backend-api conversation returned ${response.status}`);
  }
  try {
    return await response.json();
  } catch (error) {
    throw backendApiError(commandError, "backend_api_unavailable", `backend-api conversation returned non-JSON: ${String(error?.message ?? error)}`);
  }
}

function backendApiError(commandError, code, message) {
  return commandError(code, message, { phase: "wait_response", side_effect_started: true });
}

// A 429 on /api/auth/session is the same website throttle (it must NOT return
// null, which the caller misreads as signed-out); surface it as typed
// backend_api_throttled so the worker paces reads and auth refreshes together.
async function fetchChatgptAccessToken() {
  let response;
  try {
    response = await fetch("/api/auth/session", {
      method: "GET",
      credentials: "include",
      headers: { Accept: "application/json" }
    });
  } catch {
    return null;
  }
  if (response.status === 429) {
    throw throttledHttpError("session", response);
  }
  if (!response.ok) {
    return null;
  }
  try {
    const session = await response.json();
    const token = session?.accessToken;
    return typeof token === "string" && token.length > 0 ? token : null;
  } catch {
    return null;
  }
}

// Parse Retry-After (seconds-form or HTTP-date-form) into milliseconds.
// Returns 0 when absent/unparsable; the worker applies its own floor.
export function retryAfterMs(response) {
  const raw = response?.headers?.get?.("retry-after");
  const value = String(raw ?? "").trim();
  if (!value) {
    return 0;
  }
  if (/^\d+$/.test(value)) {
    return Math.max(0, Number(value) * 1000);
  }
  const date = Date.parse(value);
  return Number.isFinite(date) ? Math.max(0, date - Date.now()) : 0;
}

function throttledHttpError(endpoint, response) {
  const delayMs = retryAfterMs(response);
  const error = new Error(
    `ChatGPT website returned 429 for ${endpoint}; too many requests (retry after ${delayMs > 0 ? `${Math.ceil(delayMs / 1000)}s` : "unknown delay"})`
  );
  error.code = "backend_api_throttled";
  error.http_status = 429;
  error.endpoint_category = endpoint;
  error.retry_after_ms = delayMs;
  return error;
}

function backendApiThrottledError(commandError, endpoint, response) {
  const delayMs = retryAfterMs(response);
  return commandError(
    "backend_api_throttled",
    `ChatGPT website returned 429 for the ${endpoint} route; too many requests (retry after ${delayMs > 0 ? `${Math.ceil(delayMs / 1000)}s` : "unknown delay"})`,
    {
      phase: "wait_response",
      side_effect_started: true,
      http_status: 429,
      endpoint_category: endpoint,
      retry_after_ms: delayMs
    }
  );
}

function resolveBackendAnswer(job, conversationId, data) {
  // Freshness is relative to the assistant turns that existed before this send.
  // The post-send DOM count can already include the newly-created in-progress
  // turn, and is not comparable to completed answer nodes in the backend mapping.
  const baseline = nonNegativeInt(job?.response_baseline?.assistant_count ?? 0);
  const notReady = (detail) => ({
    method: "backend_api",
    text: "",
    is_generating: true,
    conversation_id: conversationId,
    node_fresh: false,
    assistant_count: 0,
    turn_index: -1,
    has_copy_button: false,
    copy_button_count: 0,
    backend_api_detail: detail
  });
  const mapping = data && typeof data === "object" && data.mapping && typeof data.mapping === "object"
    ? data.mapping
    : null;
  if (!mapping) {
    return notReady("backend-api response had no conversation mapping");
  }
  const { answerNode, count: lineageAnswerCount } = collectLineageAnswerNodes(mapping, data.current_node);
  if (!answerNode) {
    return notReady("no completed assistant answer node on the active lineage yet (still generating / tool-only)");
  }
  const currentNode = mapping[data.current_node];
  if (currentNode !== answerNode) {
    return notReady("latest assistant answer is not the conversation current_node (later reasoning / tool work is still active)");
  }
  if (hasInProgressMessage(mapping)) {
    return notReady("conversation mapping still contains a status=in_progress message");
  }
  if (lineageAnswerCount <= baseline) {
    return notReady(`assistant answer not fresh past baseline (active-lineage ${lineageAnswerCount} <= ${baseline})`);
  }
  const text = answerTextOf(answerNode.message);
  if (!text) {
    return notReady("latest active-lineage assistant answer node had no text parts");
  }
  return {
    method: "backend_api",
    text,
    is_generating: false,
    conversation_id: conversationId,
    node_fresh: true,
    assistant_count: lineageAnswerCount,
    turn_index: Math.max(0, lineageAnswerCount - 1),
    node_id: String(answerNode.id ?? answerNode.message?.id ?? ""),
    has_copy_button: false,
    copy_button_count: 0
  };
}

function isAssistantAnswerNode(message) {
  if (!message || typeof message !== "object" || message.author?.role !== "assistant") {
    return false;
  }
  const content = message.content;
  return Boolean(
    content
    && content.content_type === "text"
    && (message.recipient ?? "all") === "all"
    && message.end_turn === true
    && answerTextOf(message).length > 0
  );
}

function answerTextOf(message) {
  const parts = message?.content?.parts;
  return Array.isArray(parts)
    ? parts.filter((part) => typeof part === "string").join("").trim()
    : "";
}

function collectLineageAnswerNodes(mapping, currentNodeId) {
  let id = currentNodeId;
  let guard = 0;
  let answerNode = null;
  let count = 0;
  while (id && guard < 2000) {
    guard += 1;
    const node = mapping[id];
    if (!node) break;
    if (isAssistantAnswerNode(node.message)) {
      answerNode ??= node;
      count += 1;
    }
    id = node.parent;
  }
  return { answerNode, count };
}

function hasInProgressMessage(mapping) {
  return Object.values(mapping).some((node) =>
    String(node?.message?.status ?? "").toLowerCase() === "in_progress"
  );
}

function nonNegativeInt(value) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? Math.floor(number) : 0;
}

function conversationIdFromUrl(value) {
  try {
    const match = new URL(String(value ?? "")).pathname.match(/^\/c\/([^/?#]+)$/);
    return match ? decodeURIComponent(match[1]) : null;
  } catch {
    return null;
  }
}

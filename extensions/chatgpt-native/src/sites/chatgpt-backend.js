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
  if (response.status === 401 || response.status === 403) {
    throw backendApiError(commandError, "backend_api_unauthorized", `backend-api conversation returned ${response.status}`);
  }
  if (!response.ok) {
    throw backendApiError(commandError, "backend_api_unavailable", `backend-api conversation returned ${response.status}`);
  }
  let data;
  try {
    data = await response.json();
  } catch (error) {
    throw backendApiError(commandError, "backend_api_unavailable", `backend-api conversation returned non-JSON: ${String(error?.message ?? error)}`);
  }
  return resolveBackendAnswer(job, conversationId, data);
}

function backendApiError(commandError, code, message) {
  return commandError(code, message, { phase: "wait_response", side_effect_started: true });
}

async function fetchChatgptAccessToken() {
  try {
    const response = await fetch("/api/auth/session", {
      method: "GET",
      credentials: "include",
      headers: { Accept: "application/json" }
    });
    if (!response.ok) {
      return null;
    }
    const session = await response.json();
    const token = session?.accessToken;
    return typeof token === "string" && token.length > 0 ? token : null;
  } catch {
    return null;
  }
}

function resolveBackendAnswer(job, conversationId, data) {
  const baseline = nonNegativeInt(job?.submitted_assistant_count ?? job?.response_baseline?.assistant_count ?? 0);
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

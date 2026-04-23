// A2A JSON-RPC envelope builder. Single source of truth for the request shape
// used by the load harness against POST /agents/{package}/{instance}/a2a.
//
// The endpoint returns a JSON array of JSON-RPC response chunks (see
// crates/baml-rt-api/src/handlers.rs post_a2a -> Json<Vec<Value>>). It is NOT
// an SSE stream; the /a2a/sse paths found elsewhere in the repo are stale.

export function buildSendStreamBody({ text, messageId, correlationId, contextId }) {
  const message = {
    messageId,
    role: "user",
    parts: [{ kind: "text", text }],
  };
  if (contextId) {
    message.contextId = contextId;
  }
  return {
    jsonrpc: "2.0",
    id: correlationId,
    method: "message.sendStream",
    params: { message },
  };
}

// Correlation/message IDs must match the server-side parser in
// crates/baml-rt-core/src/ids.rs:133 which does `splitn(2, '-')` and
// requires both segments to be base-10 u64. Shape: `corr-<millis>-<counter>`.
// Any extra segments, non-digit characters, or base-36 encoding will be
// rejected with HTTP 400 "Invalid correlation_id".
let counter = 0;
export function nextIds() {
  counter += 1;
  const stamp = Date.now();
  return {
    messageId: `m-${stamp}-${counter}`,
    correlationId: `corr-${stamp}-${counter}`,
  };
}

export function buildEndpointUrl(ingressBase, pkg, instance) {
  const base = ingressBase.replace(/\/+$/, "");
  return `${base}/agents/${encodeURIComponent(pkg)}/${encodeURIComponent(instance)}/a2a`;
}

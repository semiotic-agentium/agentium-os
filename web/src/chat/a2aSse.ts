import type { JSONRPCResponse } from "../types/a2a";

/** Extract `data:` payloads from one SSE event block (between blank-line separators). */
function sseEventDataPayload(block: string): string | null {
  const lines = block.split("\n");
  const parts: string[] = [];
  for (const line of lines) {
    const t = line.trimEnd();
    if (t.startsWith("data:")) {
      parts.push(t.slice(5).trimStart());
    }
  }
  if (parts.length === 0) return null;
  return parts.join("\n");
}

function normalizeSseText(text: string): string {
  return text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

function parseA2aSseJsonRpcEventBlock(block: string): JSONRPCResponse | null {
  const trimmed = block.trim();
  if (!trimmed) return null;
  const payload = sseEventDataPayload(trimmed);
  if (!payload?.trim()) return null;
  return JSON.parse(payload) as JSONRPCResponse;
}

/**
 * Incrementally parse a `text/event-stream` response body and emit JSON-RPC
 * objects as soon as each SSE event block arrives.
 */
export async function readA2aSseJsonRpcStream(
  body: ReadableStream<Uint8Array>,
  onEvent: (event: JSONRPCResponse) => void,
): Promise<number> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let eventCount = 0;

  try {
    while (true) {
      const { done, value } = await reader.read();
      buffer += decoder.decode(value ?? new Uint8Array(), { stream: !done });
      buffer = normalizeSseText(buffer);

      const blocks = buffer.split("\n\n");
      buffer = blocks.pop() ?? "";

      for (const rawEvent of blocks) {
        const parsed = parseA2aSseJsonRpcEventBlock(rawEvent);
        if (!parsed) continue;
        onEvent(parsed);
        eventCount += 1;
      }

      if (done) break;
    }

    const trailing = parseA2aSseJsonRpcEventBlock(buffer);
    if (trailing) {
      onEvent(trailing);
      eventCount += 1;
    }

    return eventCount;
  } finally {
    reader.releaseLock();
  }
}

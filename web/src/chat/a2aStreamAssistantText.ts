/**
 * Extract assistant-visible text from A2A stream chunks (POST /a2a SSE JSON-RPC `result.chunk`).
 * Centralizes wire shapes so the Primary pane stays in sync with runners that emit text on
 * alternate paths (task.status.message, nested statusUpdate, task.history tail, structured parts).
 */
import type { A2aMessage, ChunkPayload, Part, StatusUpdatePayload } from "../types/a2a";

function partToVisibleString(p: Part): string | undefined {
  if (typeof p.text === "string" && p.text.trim().length > 0) {
    return p.text;
  }
  const raw = (p as { raw?: unknown }).raw;
  if (typeof raw === "string" && raw.trim().length > 0) {
    return raw;
  }
  const data = p.data;
  if (data === undefined || data === null) {
    return undefined;
  }
  const mt = (p.media_type ?? p.mediaType ?? "").toLowerCase();
  if (typeof data === "string" && data.trim().length > 0) {
    return data;
  }
  if (mt.includes("json") || (mt === "" && typeof data === "object")) {
    try {
      return JSON.stringify(data);
    } catch {
      return String(data);
    }
  }
  if (mt.includes("text/plain") || mt.includes("text/markdown")) {
    if (typeof data === "string") return data;
  }
  return undefined;
}

/** Concatenate non-empty part payloads (text, raw, or renderable data parts). */
export function extractWireMessageText(
  message: Partial<A2aMessage> | null | undefined,
): string | undefined {
  const parts = message?.parts;
  if (!parts?.length) return undefined;
  const texts: string[] = [];
  for (const p of parts) {
    const s = partToVisibleString(p);
    if (s) texts.push(s);
  }
  if (texts.length === 0) return undefined;
  return texts.join("\n\n");
}

function extractFromTaskHistory(task: ChunkPayload["task"]): string | undefined {
  const hist = task?.history;
  if (!Array.isArray(hist) || hist.length === 0) return undefined;
  for (let i = hist.length - 1; i >= 0; i--) {
    const row = hist[i];
    if (row && typeof row === "object" && "parts" in row) {
      const t = extractWireMessageText(row as A2aMessage);
      if (t?.trim()) return t;
    }
  }
  return undefined;
}

/** Relay/async payloads sometimes nest prose under `chunk.chunk`. */
function extractFromRelayInnerChunk(chunk: ChunkPayload): string | undefined {
  const inner = chunk.chunk;
  if (!inner || typeof inner !== "object") return undefined;
  const o = inner as Record<string, unknown>;
  if (o.message && typeof o.message === "object") {
    const t = extractWireMessageText(o.message as A2aMessage);
    if (t?.trim()) return t;
  }
  if (Array.isArray(o.parts)) {
    const t = extractWireMessageText({
      messageId: "",
      role: "agent",
      parts: o.parts as Part[],
    });
    if (t?.trim()) return t;
  }
  return undefined;
}

function extractFromStatusUpdatePayload(su: StatusUpdatePayload | undefined): string | undefined {
  if (!su) return undefined;
  const flat = extractWireMessageText(su.status?.message);
  if (flat?.trim()) return flat;
  const inner = su.statusUpdate ?? su.status_update;
  const nested = inner as
    | { message?: A2aMessage; status?: { message?: A2aMessage } }
    | undefined;
  return (
    extractWireMessageText(nested?.status?.message) ?? extractWireMessageText(nested?.message)
  );
}

/**
 * Best-effort assistant transcript text for one stream chunk (all common runner paths).
 */
export function collectChunkAssistantPlainText(chunk: ChunkPayload): string | undefined {
  const candidates = [
    extractWireMessageText(chunk.message),
    extractWireMessageText(chunk.task?.status?.message),
    extractFromStatusUpdatePayload(chunk.statusUpdate),
    extractFromTaskHistory(chunk.task),
    extractFromRelayInnerChunk(chunk),
  ];
  for (const c of candidates) {
    if (c?.trim()) return c;
  }
  return undefined;
}

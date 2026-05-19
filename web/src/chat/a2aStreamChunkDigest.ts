import type { ChunkPayload, StreamChunkResult } from "../types/a2a";
import { collectChunkAssistantPlainText } from "./a2aStreamAssistantText";

/** Task state string from a stream chunk (mirrors useA2aClient.getStateFromChunk). */
export function getTaskStateFromChunk(chunk: ChunkPayload | undefined): string | undefined {
  if (!chunk) return undefined;
  const t = chunk.task?.status?.state;
  if (t) return t;
  const su = chunk.statusUpdate;
  const flat = su?.status?.state;
  if (flat) return flat;
  const inner = su?.statusUpdate ?? su?.status_update;
  return (inner as { status?: { state?: string } } | undefined)?.status?.state;
}

/** Compact summary for localhost debugging (Chat vs Observe desync investigation). */
export function digestA2aProcessEvent(
  chunk: ChunkPayload | undefined,
  result: Pick<StreamChunkResult, "final" | "toolStreamChunk"> | undefined,
): {
  hasState: boolean;
  state: string | null;
  textLen: number;
  toolish: boolean;
  final: boolean;
  toolStreamChunk: boolean;
} {
  const state = getTaskStateFromChunk(chunk);
  const text = collectChunkAssistantPlainText(chunk ?? {});
  const toolish =
    !!(chunk as { toolName?: string } | undefined)?.toolName ||
    !!(chunk as { task?: { toolName?: string } } | undefined)?.task?.toolName ||
    !!(chunk as { events?: unknown[] } | undefined)?.events?.length ||
    !!(chunk as { task?: { events?: unknown[] } } | undefined)?.task?.events?.length ||
    !!(chunk as { completion?: unknown } | undefined)?.completion ||
    !!(chunk as { task?: { completion?: unknown } } | undefined)?.task?.completion;
  return {
    hasState: !!state,
    state: state ?? null,
    textLen: text?.length ?? 0,
    toolish,
    final: result?.final ?? false,
    toolStreamChunk: result?.toolStreamChunk ?? false,
  };
}

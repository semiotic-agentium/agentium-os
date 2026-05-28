/** True when text looks like a Mermaid sequence diagram from the runner export API. */
export function looksLikeMermaidDiagram(text: string): boolean {
  return /^\s*sequenceDiagram\b/m.test(text.trim());
}

type DebounceSlot = {
  timer: ReturnType<typeof setTimeout>;
  promise: Promise<string>;
  resolve: (text: string) => void;
};

const inflight = new Map<string, Promise<string>>();
const debounceByContext = new Map<string, DebounceSlot>();

const MERMAID_DEBOUNCE_MS = 400;

async function fetchContextMermaidDiagramInner(contextId: string): Promise<string> {
  try {
    const res = await fetch(`/contexts/${contextId}/mermaid`);
    if (!res.ok) {
      return "";
    }
    const text = await res.text();
    return looksLikeMermaidDiagram(text) ? text : "";
  } catch {
    return "";
  }
}

function clearDebounce(contextId: string): void {
  const slot = debounceByContext.get(contextId);
  if (!slot) return;
  clearTimeout(slot.timer);
  debounceByContext.delete(contextId);
}

/**
 * Fetch canonical context mermaid from the runner. Returns empty string on 404, invalid body, or error.
 * Concurrent callers for the same context share one in-flight request.
 */
export async function fetchContextMermaidDiagram(
  contextId: string,
  options?: { force?: boolean },
): Promise<string> {
  const trimmed = contextId.trim();
  if (!trimmed) return "";

  if (options?.force) {
    clearDebounce(trimmed);
  }

  const existing = inflight.get(trimmed);
  if (existing) return existing;

  const request = fetchContextMermaidDiagramInner(trimmed).finally(() => {
    inflight.delete(trimmed);
  });
  inflight.set(trimmed, request);
  return request;
}

/** Drop debounce timers (e.g. when switching context). */
export function invalidateContextMermaidSchedule(contextId?: string): void {
  if (contextId) {
    clearDebounce(contextId.trim());
    return;
  }
  for (const id of debounceByContext.keys()) {
    clearDebounce(id);
  }
}

/**
 * Debounced mermaid fetch for high-frequency provenance bumps (SSE deltas, trace refresh).
 * Multiple calls within {@link MERMAID_DEBOUNCE_MS} coalesce to one network request.
 */
export function scheduleContextMermaidDiagram(contextId: string): Promise<string> {
  const trimmed = contextId.trim();
  if (!trimmed) return Promise.resolve("");

  let slot = debounceByContext.get(trimmed);
  if (!slot) {
    let resolve!: (text: string) => void;
    const promise = new Promise<string>((r) => {
      resolve = r;
    });
    slot = {
      timer: setTimeout(() => {}, 0),
      promise,
      resolve,
    };
    debounceByContext.set(trimmed, slot);
  }

  clearTimeout(slot.timer);
  slot.timer = setTimeout(() => {
    debounceByContext.delete(trimmed);
    void fetchContextMermaidDiagram(trimmed).then(slot!.resolve);
  }, MERMAID_DEBOUNCE_MS);

  return slot.promise;
}

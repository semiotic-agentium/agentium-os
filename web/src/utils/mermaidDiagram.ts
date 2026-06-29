// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** True when text looks like a Mermaid sequence diagram from the runner export API. */
export function looksLikeMermaidDiagram(text: string): boolean {
  return /^\s*sequenceDiagram\b/m.test(text.trim());
}

type DebounceSlot = {
  timer: ReturnType<typeof setTimeout>;
  promise: Promise<string>;
  resolve: (text: string) => void;
};

type MermaidFetchOptions = {
  /** Include derived A2A child contexts via /mermaid/full. */
  full?: boolean;
};

const inflight = new Map<string, Promise<string>>();
const debounceByContext = new Map<string, DebounceSlot>();

const MERMAID_DEBOUNCE_MS = 400;

function mermaidCacheKey(contextId: string, options?: MermaidFetchOptions): string {
  return options?.full ? `${contextId}::full` : contextId;
}

function mermaidUrl(contextId: string, options?: MermaidFetchOptions): string {
  const encoded = encodeURIComponent(contextId);
  return options?.full ? `/contexts/${encoded}/mermaid/full` : `/contexts/${encoded}/mermaid`;
}

async function fetchContextMermaidDiagramInner(
  contextId: string,
  options?: MermaidFetchOptions,
): Promise<string> {
  try {
    const res = await fetch(mermaidUrl(contextId, options));
    if (!res.ok) {
      return "";
    }
    const text = await res.text();
    return looksLikeMermaidDiagram(text) ? text : "";
  } catch {
    return "";
  }
}

function clearDebounceKey(key: string): void {
  const slot = debounceByContext.get(key);
  if (!slot) return;
  clearTimeout(slot.timer);
  debounceByContext.delete(key);
}

function clearDebounce(contextId: string): void {
  clearDebounceKey(mermaidCacheKey(contextId));
  clearDebounceKey(mermaidCacheKey(contextId, { full: true }));
}

/**
 * Fetch canonical context mermaid from the runner. Returns empty string on 404, invalid body, or error.
 * Concurrent callers for the same context share one in-flight request.
 */
export async function fetchContextMermaidDiagram(
  contextId: string,
  options?: MermaidFetchOptions & { force?: boolean },
): Promise<string> {
  const trimmed = contextId.trim();
  if (!trimmed) return "";
  const key = mermaidCacheKey(trimmed, options);

  if (options?.force) {
    clearDebounce(trimmed);
  }

  const existing = inflight.get(key);
  if (existing) return existing;

  const request = fetchContextMermaidDiagramInner(trimmed, options).finally(() => {
    inflight.delete(key);
  });
  inflight.set(key, request);
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
export function scheduleContextMermaidDiagram(
  contextId: string,
  options?: MermaidFetchOptions,
): Promise<string> {
  const trimmed = contextId.trim();
  if (!trimmed) return Promise.resolve("");
  const key = mermaidCacheKey(trimmed, options);

  let slot = debounceByContext.get(key);
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
    debounceByContext.set(key, slot);
  }

  clearTimeout(slot.timer);
  slot.timer = setTimeout(() => {
    debounceByContext.delete(key);
    void fetchContextMermaidDiagram(trimmed, options).then(slot!.resolve);
  }, MERMAID_DEBOUNCE_MS);

  return slot.promise;
}

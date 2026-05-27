import { ref, watch, type ComputedRef, type Ref } from "vue";
import type { ObservationBundle } from "../types/provenance";

export type { ObservationBundle };

function buildObserveUrl(
  contextId: string,
  params: {
    taskId?: string;
    agentPackage?: string;
    agentId?: string;
    includeDrift?: boolean;
    stream?: boolean;
  },
): string {
  const search = new URLSearchParams();
  if (params.taskId) search.set("taskId", params.taskId);
  if (params.agentPackage) search.set("agentPackage", params.agentPackage);
  if (params.agentId) search.set("agentId", params.agentId);
  if (params.includeDrift) search.set("includeDrift", "true");
  const suffix = search.toString();
  const base = params.stream
    ? `/contexts/${encodeURIComponent(contextId)}/observe/stream`
    : `/contexts/${encodeURIComponent(contextId)}/observe`;
  return suffix ? `${base}?${suffix}` : base;
}

export function useContextObserve(options: {
  contextId: Ref<string | undefined> | ComputedRef<string | undefined>;
  taskId?: Ref<string | undefined> | ComputedRef<string | undefined>;
  agentPackage?: Ref<string | undefined> | ComputedRef<string | undefined>;
  agentId?: Ref<string | undefined> | ComputedRef<string | undefined>;
  includeDrift?: Ref<boolean> | ComputedRef<boolean>;
  active: Ref<boolean> | ComputedRef<boolean>;
}) {
  const bundle = ref<ObservationBundle | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);
  let abort: AbortController | null = null;
  let eventSource: EventSource | null = null;

  function closeStream() {
    eventSource?.close();
    eventSource = null;
    abort?.abort();
    abort = null;
  }

  function applyBundle(raw: ObservationBundle) {
    bundle.value = {
      contextId: raw.contextId ?? "",
      version: raw.version ?? "",
      planning: raw.planning ?? null,
      llmOps: raw.llmOps ?? null,
      toolOps: raw.toolOps ?? null,
    };
  }

  async function fetchSnapshot() {
    const contextId = options.contextId.value?.trim();
    if (!contextId) {
      bundle.value = null;
      return;
    }
    closeStream();
    loading.value = true;
    error.value = null;
    abort = new AbortController();
    try {
      const url = buildObserveUrl(contextId, {
        taskId: options.taskId?.value,
        agentPackage: options.agentPackage?.value,
        agentId: options.agentId?.value,
        includeDrift: options.includeDrift?.value,
        stream: false,
      });
      const res = await fetch(url, { signal: abort.signal });
      if (!res.ok) throw new Error(`observe failed: ${res.status}`);
      applyBundle((await res.json()) as ObservationBundle);
    } catch (e) {
      if (e instanceof DOMException && e.name === "AbortError") return;
      error.value = (e as Error).message;
    } finally {
      loading.value = false;
    }
  }

  function connectStream() {
    const contextId = options.contextId.value?.trim();
    if (!contextId) return;
    closeStream();
    loading.value = true;
    error.value = null;
    const url = buildObserveUrl(contextId, {
      taskId: options.taskId?.value,
      agentPackage: options.agentPackage?.value,
      agentId: options.agentId?.value,
      includeDrift: options.includeDrift?.value,
      stream: true,
    });
    eventSource = new EventSource(url);
    eventSource.addEventListener("snapshot", (ev) => {
      loading.value = false;
      try {
        applyBundle(JSON.parse((ev as MessageEvent).data) as ObservationBundle);
      } catch (e) {
        error.value = (e as Error).message;
      }
    });
    eventSource.addEventListener("bundle", (ev) => {
      try {
        applyBundle(JSON.parse((ev as MessageEvent).data) as ObservationBundle);
      } catch (e) {
        error.value = (e as Error).message;
      }
    });
    eventSource.onerror = () => {
      if (!bundle.value) {
        error.value = "Observation stream disconnected";
      }
      loading.value = false;
    };
  }

  function refresh() {
    if (options.active.value) {
      connectStream();
    } else {
      void fetchSnapshot();
    }
  }

  watch(
    [
      options.contextId,
      () => options.taskId?.value,
      () => options.agentPackage?.value,
      () => options.agentId?.value,
      options.active,
    ],
    () => {
      refresh();
    },
    { immediate: true },
  );

  return { bundle, loading, error, refresh, closeStream };
}

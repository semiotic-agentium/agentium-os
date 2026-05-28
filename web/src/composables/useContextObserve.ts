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

function observeScopeKey(options: {
  contextId?: string;
  taskId?: string;
  agentPackage?: string;
  agentId?: string;
  includeDrift?: boolean;
  active: boolean;
}): string {
  return JSON.stringify({
    contextId: options.contextId?.trim() ?? "",
    taskId: options.taskId?.trim() ?? "",
    agentPackage: options.agentPackage?.trim() ?? "",
    agentId: options.agentId?.trim() ?? "",
    includeDrift: options.includeDrift ?? false,
    active: options.active,
  });
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
  let lastScopeKey = "";
  let fetchInFlight: Promise<void> | null = null;
  let fetchInFlightUrl = "";
  let streamUrl = "";

  function closeStream() {
    eventSource?.close();
    eventSource = null;
    streamUrl = "";
    abort?.abort();
    abort = null;
    fetchInFlight = null;
    fetchInFlightUrl = "";
  }

  function currentSnapshotUrl(contextId: string): string {
    return buildObserveUrl(contextId, {
      taskId: options.taskId?.value,
      agentPackage: options.agentPackage?.value,
      agentId: options.agentId?.value,
      includeDrift: options.includeDrift?.value,
      stream: false,
    });
  }

  function currentStreamUrl(contextId: string): string {
    return buildObserveUrl(contextId, {
      taskId: options.taskId?.value,
      agentPackage: options.agentPackage?.value,
      agentId: options.agentId?.value,
      includeDrift: options.includeDrift?.value,
      stream: true,
    });
  }

  async function fetchSnapshot(): Promise<void> {
    const contextId = options.contextId.value?.trim();
    if (!contextId) {
      closeStream();
      bundle.value = null;
      return;
    }

    const url = currentSnapshotUrl(contextId);
    if (fetchInFlightUrl === url && fetchInFlight) {
      return fetchInFlight;
    }

    if (eventSource) {
      eventSource.close();
      eventSource = null;
      streamUrl = "";
    }
    if (fetchInFlightUrl !== url) {
      abort?.abort();
    }

    const controller = new AbortController();
    abort = controller;
    fetchInFlightUrl = url;
    fetchInFlight = (async () => {
      loading.value = true;
      error.value = null;
      try {
        const res = await fetch(url, { signal: controller.signal });
        if (!res.ok) throw new Error(`observe failed: ${res.status}`);
        applyBundle((await res.json()) as ObservationBundle);
      } catch (e) {
        if (e instanceof DOMException && e.name === "AbortError") return;
        error.value = (e as Error).message;
      } finally {
        if (fetchInFlightUrl === url) {
          fetchInFlight = null;
          fetchInFlightUrl = "";
        }
        if (abort === controller) {
          abort = null;
        }
        loading.value = false;
      }
    })();

    return fetchInFlight;
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

  function connectStream() {
    const contextId = options.contextId.value?.trim();
    if (!contextId) return;

    const url = currentStreamUrl(contextId);
    if (streamUrl === url && eventSource) return;

    if (fetchInFlightUrl !== url) {
      abort?.abort();
    }
    fetchInFlight = null;
    fetchInFlightUrl = "";
    eventSource?.close();

    loading.value = true;
    error.value = null;
    const stream = new EventSource(url);
    eventSource = stream;
    streamUrl = url;
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

  function refresh(force = false) {
    const scopeKey = observeScopeKey({
      contextId: options.contextId.value,
      taskId: options.taskId?.value,
      agentPackage: options.agentPackage?.value,
      agentId: options.agentId?.value,
      includeDrift: options.includeDrift?.value,
      active: options.active.value,
    });
    if (!force && scopeKey === lastScopeKey) {
      return;
    }
    if (force && scopeKey === lastScopeKey) {
      const contextId = options.contextId.value?.trim();
      if (!contextId) return;
      if (options.active.value) {
        if (streamUrl === currentStreamUrl(contextId) && eventSource) return;
      } else if (fetchInFlightUrl === currentSnapshotUrl(contextId) && fetchInFlight) {
        return;
      }
    }
    lastScopeKey = scopeKey;

    const contextId = options.contextId.value?.trim();
    if (!contextId) {
      closeStream();
      bundle.value = null;
      return;
    }

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
      () => options.includeDrift?.value,
      options.active,
    ],
    () => {
      refresh();
    },
    { immediate: true },
  );

  return {
    bundle,
    loading,
    error,
    refresh: () => refresh(true),
    closeStream,
  };
}

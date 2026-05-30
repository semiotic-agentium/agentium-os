// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { computed, onUnmounted, ref, type ComputedRef, type Ref } from "vue";
import type {
  ProvenanceGroupHotspot,
  ProvenanceQueryParams,
  ProvenanceQueryResponse,
  ProvenanceRowBase,
  ProvenanceResource,
} from "../types/provenance";

export interface ProvenanceQueryState {
  loading: boolean;
  error: string | null;
  response: ProvenanceQueryResponse | null;
  lastUpdatedAt: number | null;
  params: ProvenanceQueryParams;
  previousCursors: string[];
}

interface AutoRefreshOptions {
  activeRef?: Ref<boolean>;
  activeIntervalMs?: number;
  idleIntervalMs?: number;
}

export interface ProvenanceQueryController {
  readonly resource: Ref<ProvenanceResource>;
  readonly state: Ref<ProvenanceQueryState>;
  readonly hasNextPage: ComputedRef<boolean>;
  readonly hasPreviousPage: ComputedRef<boolean>;
  run: (override?: Partial<ProvenanceQueryParams>) => Promise<void>;
  refresh: () => Promise<void>;
  nextPage: () => Promise<void>;
  previousPage: () => Promise<void>;
  setParams: (next: Partial<ProvenanceQueryParams>) => void;
  setResource: (resource: ProvenanceResource) => void;
  clear: () => void;
  startAutoRefresh: (options?: AutoRefreshOptions) => void;
  stopAutoRefresh: () => void;
}

function rowIdentity(row: ProvenanceRowBase, idx: number): string {
  const activityId = row.activity_id;
  if (typeof activityId === "string" && activityId.length > 0) return activityId;
  const messageId = row.message_id;
  if (typeof messageId === "string" && messageId.length > 0) {
    const kind = typeof row.activity_kind === "string" ? row.activity_kind : "row";
    return `${kind}:${messageId}:${idx}`;
  }
  return `row:${idx}`;
}

function groupIdentity(group: ProvenanceGroupHotspot, idx: number): string {
  const key = group.groupKey;
  if (typeof key === "string" && key.length > 0) return key;
  return `group:${idx}`;
}

function mergeResponse(
  previous: ProvenanceQueryResponse | null,
  incoming: ProvenanceQueryResponse,
): ProvenanceQueryResponse {
  if (!previous) return incoming;
  const prevRowsById = new Map<string, ProvenanceRowBase>();
  previous.rows.forEach((row, idx) => {
    prevRowsById.set(rowIdentity(row, idx), row);
  });
  const mergedRows = incoming.rows.map((row, idx) => {
    const id = rowIdentity(row, idx);
    const prev = prevRowsById.get(id);
    if (!prev) return row;
    return JSON.stringify(prev) === JSON.stringify(row) ? prev : row;
  });

  const prevGroupsById = new Map<string, ProvenanceGroupHotspot>();
  previous.hotspotGroups.forEach((group, idx) => {
    prevGroupsById.set(groupIdentity(group, idx), group);
  });
  const mergedGroups = incoming.hotspotGroups.map((group, idx) => {
    const id = groupIdentity(group, idx);
    const prev = prevGroupsById.get(id);
    if (!prev) return group;
    return JSON.stringify(prev) === JSON.stringify(group) ? prev : group;
  });

  return {
    ...incoming,
    rows: mergedRows,
    hotspotGroups: mergedGroups,
  };
}

function toSearchParams(params: ProvenanceQueryParams): URLSearchParams {
  const out = new URLSearchParams();
  const entries: Array<[string, string | number | undefined]> = [
    ["contextId", params.contextId],
    ["taskId", params.taskId],
    ["agentId", params.agentId],
    ["provider", params.provider],
    ["model", params.model],
    ["toolName", params.toolName],
    ["bamlPrompt", params.bamlPrompt],
    ["fromTimestampMs", params.fromTimestampMs],
    ["toTimestampMs", params.toTimestampMs],
    ["sortBy", params.sortBy],
    ["sortDir", params.sortDir],
    ["pageSize", params.pageSize],
    ["cursor", params.cursor],
    ["topK", params.topK],
    ["outcome", params.outcome],
    ["responseProfile", params.responseProfile],
  ];

  for (const [key, value] of entries) {
    if (value === undefined || value === null || value === "") continue;
    out.set(key, String(value));
  }
  if (params.groupBy && params.groupBy.length > 0) {
    out.set("groupBy", params.groupBy.join(","));
  }
  return out;
}

function defaultState(initialParams?: ProvenanceQueryParams): ProvenanceQueryState {
  return {
    loading: false,
    error: null,
    response: null,
    lastUpdatedAt: null,
    params: {
      pageSize: 25,
      sortBy: "timestamp_ms",
      sortDir: "desc",
      outcome: "both",
      ...initialParams,
    },
    previousCursors: [],
  };
}

export function useProvenanceOps() {
  function createQuery(
    initialResource: ProvenanceResource,
    initialParams?: ProvenanceQueryParams,
  ): ProvenanceQueryController {
    const resource = ref<ProvenanceResource>(initialResource);
    const state = ref<ProvenanceQueryState>(defaultState(initialParams));

    let abortController: AbortController | null = null;
    let refreshTimer: ReturnType<typeof setTimeout> | null = null;
    let refreshTick = 0;
    let refreshOptions: AutoRefreshOptions | null = null;

    const hasNextPage = computed(
      () => !!state.value.response?.nextCursor && state.value.response.rows.length > 0,
    );
    const hasPreviousPage = computed(() => state.value.previousCursors.length > 0);

    function clearRefreshTimer() {
      if (refreshTimer) {
        clearTimeout(refreshTimer);
        refreshTimer = null;
      }
    }

    function stopAutoRefresh() {
      clearRefreshTimer();
      refreshOptions = null;
    }

    async function executeQuery(params: ProvenanceQueryParams) {
      abortController?.abort();
      abortController = new AbortController();

      state.value.loading = true;
      state.value.error = null;
      try {
        const search = toSearchParams(params);
        const path = `/provenance/${resource.value.replace("_", "-")}`;
        const url = search.toString() ? `${path}?${search.toString()}` : path;
        const response = await fetch(url, { signal: abortController.signal });
        if (!response.ok) {
          let detail = `${response.status}`;
          try {
            const body = (await response.json()) as { detail?: string };
            if (body?.detail) detail = body.detail;
          } catch {
            // ignore non-json body
          }
          throw new Error(`Provenance request failed: ${detail}`);
        }

        const payload = (await response.json()) as ProvenanceQueryResponse;
        state.value.response = mergeResponse(state.value.response, payload);
        state.value.lastUpdatedAt = Date.now();
      } catch (error) {
        if ((error as Error).name !== "AbortError") {
          state.value.error = (error as Error).message;
        }
      } finally {
        state.value.loading = false;
      }
    }

    async function run(override?: Partial<ProvenanceQueryParams>) {
      const merged = {
        ...state.value.params,
        ...override,
      };
      state.value.params = merged;
      await executeQuery(merged);
    }

    async function refresh() {
      await executeQuery(state.value.params);
    }

    async function nextPage() {
      const next = state.value.response?.nextCursor;
      if (!next) return;
      const currentCursor = state.value.params.cursor;
      state.value.previousCursors.push(currentCursor ?? "");
      await run({ cursor: next });
    }

    async function previousPage() {
      if (state.value.previousCursors.length === 0) return;
      const prev = state.value.previousCursors.pop();
      await run({ cursor: prev || undefined });
    }

    function setParams(next: Partial<ProvenanceQueryParams>) {
      state.value.params = { ...state.value.params, ...next };
    }

    function setResource(nextResource: ProvenanceResource) {
      if (resource.value === nextResource) return;
      resource.value = nextResource;
      state.value.previousCursors = [];
      state.value.params = {
        ...state.value.params,
        cursor: undefined,
      };
    }

    function clear() {
      stopAutoRefresh();
      abortController?.abort();
      state.value = defaultState(state.value.params);
    }

    function scheduleRefreshLoop() {
      clearRefreshTimer();
      if (!refreshOptions) return;
      refreshTick += 1;
      const tick = refreshTick;
      const active = refreshOptions.activeRef?.value ?? false;
      const delay = active
        ? (refreshOptions.activeIntervalMs ?? 1500)
        : (refreshOptions.idleIntervalMs ?? 6000);
      refreshTimer = setTimeout(async () => {
        if (!refreshOptions || tick !== refreshTick) return;
        await refresh();
        scheduleRefreshLoop();
      }, delay);
    }

    function startAutoRefresh(options?: AutoRefreshOptions) {
      refreshOptions = options ?? {};
      scheduleRefreshLoop();
    }

    onUnmounted(() => {
      stopAutoRefresh();
      abortController?.abort();
    });

    return {
      resource,
      state,
      hasNextPage,
      hasPreviousPage,
      run,
      refresh,
      nextPage,
      previousPage,
      setParams,
      setResource,
      clear,
      startAutoRefresh,
      stopAutoRefresh,
    };
  }

  return {
    createQuery,
  };
}

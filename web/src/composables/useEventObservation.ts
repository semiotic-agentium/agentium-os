// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { computed, onUnmounted, ref } from "vue";
import { resolveInstanceUrl } from "./instanceApi";
import type { ChatMessage, ConversationHistoryPage, HistoryHydrateState } from "../types/a2a";
import type { ConversationHistoryIngressMode } from "../chat/conversationHistorySync";
import { fetchMergedConversationHistoryPage } from "../chat/fetchConversationHistoryPage";
import {
  applyConversationHistoryIngress,
  type ConversationHistoryIngressDeps,
} from "../chat/conversationHistorySync";
import {
  invalidateContextMermaidSchedule,
  scheduleContextMermaidDiagram,
} from "../utils/mermaidDiagram";
import {
  mergeEventConsoleTranscript,
  transcriptHasIngressUserRows,
} from "../events/dispatchObserve";
import {
  observationScopeKey,
  observationScopeQueryParams,
  type ObservationScope,
} from "./useObservationScope";
import { useTraceRefreshGeneration, bumpTraceRefreshOnHistoryIngress } from "./useTraceRefreshGeneration";

const HISTORY_PAGE_SIZE = 50;
const HISTORY_FETCH_TIMEOUT_MS = 12_000;
/** One-shot transcript retry after ack-only dispatch (no interval polling). */
const PRESERVE_TRANSCRIPT_RETRY_MS = 2_000;
/** Coalesce SSE delta notifications into one authoritative GET reconcile. */
const DELTA_RECONCILE_MS = 80;

export interface LoadContextOptions {
  /** When set, do not clear existing messages until a non-empty transcript arrives. */
  preserveMessagesUntilTranscript?: boolean;
  /** Restrict transcript rows to this agent package (matches GET /contexts?agentPackage=). */
  agentPackage?: string | null;
  /** How to apply GET/SSE conversation-history payloads (restore vs live publish). */
  ingressMode?: ConversationHistoryIngressMode;
}

export type TraceObserveState = "idle" | "loading" | "waiting" | "ready" | "empty" | "error";

function mapHydrateState(state: HistoryHydrateState): TraceObserveState {
  if (state === "skipped") return "waiting";
  return state;
}

/** Load provenance-backed transcript for an event run context (no A2A chat). */
export function useEventObservation() {
  const messages = ref<ChatMessage[]>([]);
  const localOverlay = ref<ChatMessage[]>([]);
  const contextId = ref<string | null>(null);
  const taskId = ref<string | null>(null);
  const provenanceDiagram = ref("");
  const hydrateState = ref<TraceObserveState>("idle");

  let historyStream: EventSource | null = null;
  let historyStreamKey = "";
  let loadContextInFlight: Promise<void> | null = null;
  let loadContextInFlightKey = "";
  let preserveRetryTimer: ReturnType<typeof setTimeout> | null = null;
  let deltaReconcileTimer: ReturnType<typeof setTimeout> | null = null;
  let reconcileSeq = 0;
  let diagramFetchSeq = 0;
  let observeCtx = "";
  /** Skip redundant full reloads when context/task/agent unchanged. */
  let loadedObserveKey = "";
  let activeIngressMode: ConversationHistoryIngressMode = "evented";

  const transcriptMessages = computed(() =>
    mergeEventConsoleTranscript(messages.value, localOverlay.value),
  );

  const traceRefresh = useTraceRefreshGeneration({
    onBump: () => {
      if (observeCtx) {
        void fetchDiagram(observeCtx);
      }
    },
  });

  function ingressDeps(): ConversationHistoryIngressDeps {
    return {
      messages,
      getHistoryVersion: traceRefresh.getHistoryVersion,
      setHistoryVersion: traceRefresh.setHistoryVersion,
      setHydrateState: (state: HistoryHydrateState) => {
        hydrateState.value = mapHydrateState(state);
      },
      setSelectedContextId: () => {},
      setTaskId: (id: string | null) => {
        taskId.value = id;
      },
      replaceLlmFromPage: () => {},
      extendLlmFromPage: () => {},
    };
  }

  function pruneLocalOverlay(): void {
    if (!transcriptHasIngressUserRows(messages.value)) return;
    localOverlay.value = localOverlay.value.filter(
      (m) => !(m.role === "user" && m.speakerKind === "ingress"),
    );
  }

  function applyFullConversationPage(
    page: ConversationHistoryPage,
    options?: { respectDuplicateVersion?: boolean; ingressMode?: ConversationHistoryIngressMode },
  ): void {
    const effect = applyConversationHistoryIngress(ingressDeps(), {
      kind: "full",
      mode: options?.ingressMode ?? "evented",
      page,
      respectDuplicateVersion: options?.respectDuplicateVersion ?? false,
      syncTaskIdFromPageBeforeDefer: false,
    });
    bumpTraceRefreshOnHistoryIngress(traceRefresh, effect);
    pruneLocalOverlay();
    if (messages.value.length > 0) {
      hydrateState.value = "ready";
      stopPreserveRetry();
    }
  }

  function closeHistoryStream(): void {
    if (historyStream) {
      historyStream.close();
      historyStream = null;
    }
    historyStreamKey = "";
  }

  function stopPreserveRetry(): void {
    if (preserveRetryTimer !== null) {
      clearTimeout(preserveRetryTimer);
      preserveRetryTimer = null;
    }
  }

  function stopDeltaReconcile(): void {
    if (deltaReconcileTimer !== null) {
      clearTimeout(deltaReconcileTimer);
      deltaReconcileTimer = null;
    }
  }

  function bumpTraceRefresh(): void {
    traceRefresh.bumpTraceRefresh();
  }

  function streamQueryParams(scope: ObservationScope): URLSearchParams {
    const params = observationScopeQueryParams(scope);
    params.set("limit", String(HISTORY_PAGE_SIZE));
    params.set("profile", "full");
    return params;
  }

  function scheduleDeltaReconcile(scope: ObservationScope): void {
    stopDeltaReconcile();
    deltaReconcileTimer = setTimeout(() => {
      deltaReconcileTimer = null;
      void reconcileFullTranscript(scope);
    }, DELTA_RECONCILE_MS);
  }

  async function reconcileFullTranscript(scope: ObservationScope): Promise<void> {
    const seq = ++reconcileSeq;
    const page = await fetchMergedConversationHistoryPage(scope);
    if (!page || seq !== reconcileSeq) return;
    applyFullConversationPage(page, { respectDuplicateVersion: false, ingressMode: activeIngressMode });
  }

  function ensureHistoryStream(scope: ObservationScope): void {
    const key = observationScopeKey(scope);
    if (historyStreamKey === key && historyStream) return;

    closeHistoryStream();
    const params = streamQueryParams(scope);
    const encodedContextId = encodeURIComponent(scope.contextId);
    const url = `/contexts/${encodedContextId}/conversation-history/stream?${params.toString()}`;
    const stream = new EventSource(resolveInstanceUrl(url));
    // Register before listeners so concurrent ensureHistoryStream calls close this stream.
    historyStream = stream;
    historyStreamKey = key;

    stream.addEventListener("snapshot", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        applyFullConversationPage(page, {
          respectDuplicateVersion: true,
          ingressMode: activeIngressMode,
        });
      } catch {
        // ignore malformed payloads
      }
    });
    stream.addEventListener("delta", () => {
      scheduleDeltaReconcile(scope);
    });
    stream.addEventListener("done", () => {
      bumpTraceRefresh();
      if (historyStream === stream) {
        stream.close();
        historyStream = null;
        historyStreamKey = "";
      }
    });
    stream.onerror = () => {
      if (historyStream === stream) {
        stream.close();
        historyStream = null;
        historyStreamKey = "";
      }
    };
  }

  function schedulePreserveTranscriptRetry(
    ctx: string,
    task?: string | null,
    agentPackage?: string | null,
  ): void {
    stopPreserveRetry();
    preserveRetryTimer = setTimeout(() => {
      preserveRetryTimer = null;
      void refreshHistoryPage(observeScope(ctx, task, agentPackage), { bumpTrace: true }).then(() => {
        if (messages.value.length > 0) {
          hydrateState.value = "ready";
        } else if (hydrateState.value === "waiting") {
          hydrateState.value = "empty";
        }
      });
    }, PRESERVE_TRANSCRIPT_RETRY_MS);
  }

  async function refreshHistoryPage(
    scope: ObservationScope,
    options?: { bumpTrace?: boolean },
  ): Promise<void> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), HISTORY_FETCH_TIMEOUT_MS);
    try {
      const page = await fetchMergedConversationHistoryPage(scope, {
        signal: controller.signal,
        pageSize: HISTORY_PAGE_SIZE,
      });
      if (!page) return;
      applyFullConversationPage(page, {
        respectDuplicateVersion: false,
        ingressMode: activeIngressMode,
      });
      if (options?.bumpTrace) {
        bumpTraceRefresh();
      }
    } catch {
      // timeout or network — observation may stay empty; operator summary still shown
    } finally {
      clearTimeout(timer);
    }
  }

  async function fetchDiagram(ctx: string): Promise<void> {
    const seq = ++diagramFetchSeq;
    const text = await scheduleContextMermaidDiagram(ctx, { full: true });
    if (seq !== diagramFetchSeq) return;
    provenanceDiagram.value = text;
  }

  function observeScope(
    ctx: string,
    task?: string | null,
    agentPackage?: string | null,
  ): ObservationScope {
    return { contextId: ctx, taskId: task ?? null, agentPackage: agentPackage ?? null };
  }

  function observeKey(
    ctx: string,
    task?: string | null,
    agentPackage?: string | null,
  ): string {
    return observationScopeKey(observeScope(ctx, task, agentPackage));
  }

  function setLocalOverlay(rows: ChatMessage[]): void {
    localOverlay.value = rows;
  }

  async function loadContext(
    ctx: string,
    task?: string | null,
    options?: LoadContextOptions,
  ): Promise<void> {
    const preserve = options?.preserveMessagesUntilTranscript ?? false;
    const agentPackage = options?.agentPackage ?? null;
    const ingressMode = options?.ingressMode ?? "evented";
    const key = observeKey(ctx, task, agentPackage);
    const inFlightKey = `${key}:${preserve ? "preserve" : "full"}:${ingressMode}`;
    if (loadContextInFlightKey === inFlightKey && loadContextInFlight) {
      return loadContextInFlight;
    }

    loadContextInFlightKey = inFlightKey;
    loadContextInFlight = runLoadContext(ctx, task, agentPackage, preserve, ingressMode, key);
    try {
      await loadContextInFlight;
    } finally {
      if (loadContextInFlightKey === inFlightKey) {
        loadContextInFlight = null;
        loadContextInFlightKey = "";
      }
    }
  }

  async function runLoadContext(
    ctx: string,
    task: string | null | undefined,
    agentPackage: string | null,
    preserve: boolean,
    ingressMode: ConversationHistoryIngressMode,
    key: string,
  ): Promise<void> {
    const scope = observeScope(ctx, task, agentPackage);
    const previousObserveKey = loadedObserveKey;
    const sameObserveTarget = key === loadedObserveKey;

    observeCtx = ctx;
    contextId.value = ctx;
    taskId.value = task ?? null;

    if (sameObserveTarget && !preserve) {
      if (messages.value.length > 0) {
        bumpTraceRefresh();
        return;
      }
    }

    if (preserve && sameObserveTarget) {
      try {
        await refreshHistoryPage(scope);
        if (messages.value.length > 0) {
          hydrateState.value = "ready";
          stopPreserveRetry();
        } else {
          hydrateState.value = "waiting";
          schedulePreserveTranscriptRetry(ctx, task, agentPackage);
        }
        ensureHistoryStream(scope);
        bumpTraceRefresh();
        return;
      } catch {
        hydrateState.value = "error";
        return;
      }
    }

    loadedObserveKey = key;
    activeIngressMode = ingressMode;
    traceRefresh.resetHistoryVersion();
    stopDeltaReconcile();
    if (!preserve) {
      provenanceDiagram.value = "";
      invalidateContextMermaidSchedule(ctx);
      diagramFetchSeq += 1;
      messages.value = [];
      localOverlay.value = [];
      hydrateState.value = "loading";
    }
    stopPreserveRetry();
    if (previousObserveKey !== key) {
      closeHistoryStream();
    }
    try {
      await refreshHistoryPage(scope);
      if (messages.value.length > 0) {
        hydrateState.value = "ready";
        ensureHistoryStream(scope);
        bumpTraceRefresh();
        return;
      }
      if (preserve) {
        hydrateState.value = "waiting";
        schedulePreserveTranscriptRetry(ctx, task, agentPackage);
      } else {
        hydrateState.value = "empty";
      }
      ensureHistoryStream(scope);
      bumpTraceRefresh();
    } catch {
      hydrateState.value = "error";
    }
  }

  function setDemoMessages(demo: ChatMessage[]): void {
    messages.value = demo;
    if (demo.length > 0) {
      hydrateState.value = "ready";
      stopPreserveRetry();
    }
  }

  function clear(): void {
    stopPreserveRetry();
    stopDeltaReconcile();
    closeHistoryStream();
    observeCtx = "";
    loadedObserveKey = "";
    traceRefresh.resetHistoryVersion();
    messages.value = [];
    localOverlay.value = [];
    contextId.value = null;
    taskId.value = null;
    provenanceDiagram.value = "";
    diagramFetchSeq += 1;
    hydrateState.value = "idle";
  }

  onUnmounted(() => {
    clear();
  });

  return {
    messages,
    transcriptMessages,
    localOverlay,
    contextId,
    taskId,
    provenanceDiagram,
    traceRefreshGeneration: traceRefresh.traceRefreshGeneration,
    hydrateState,
    loadContext,
    setLocalOverlay,
    setDemoMessages,
    clear,
    bumpTraceRefresh,
  };
}

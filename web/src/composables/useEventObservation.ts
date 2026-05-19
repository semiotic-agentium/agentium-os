import { onUnmounted, ref } from "vue";
import type { ChatMessage, ConversationHistoryPage } from "../types/a2a";
import {
  applyConversationHistoryDelta,
  applyConversationHistoryPage,
  ConversationHistoryDeltaApplyMode,
} from "../chat/conversationHistoryHydration";
import { fetchContextMermaidDiagram } from "../utils/mermaidDiagram";

const HISTORY_PAGE_SIZE = 50;
const HISTORY_FETCH_TIMEOUT_MS = 12_000;
const TRACE_REFRESH_DEBOUNCE_MS = 300;
/** One-shot transcript retry after ack-only dispatch (no interval polling). */
const PRESERVE_TRANSCRIPT_RETRY_MS = 2_000;

export interface LoadContextOptions {
  /** When set, do not clear existing messages until a non-empty transcript arrives. */
  preserveMessagesUntilTranscript?: boolean;
  /** Restrict transcript rows to this agent package (matches GET /contexts?agentPackage=). */
  agentPackage?: string | null;
}

export type TraceObserveState = "idle" | "loading" | "waiting" | "ready" | "empty" | "error";

/** Load provenance-backed transcript for an event run context (no A2A chat). */
export function useEventObservation() {
  const messages = ref<ChatMessage[]>([]);
  const contextId = ref<string | null>(null);
  const taskId = ref<string | null>(null);
  const provenanceDiagram = ref("");
  const traceRefreshGeneration = ref(0);
  const hydrateState = ref<TraceObserveState>("idle");

  let historyStream: EventSource | null = null;
  let historyStreamKey = "";
  let traceRefreshDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  let preserveRetryTimer: ReturnType<typeof setTimeout> | null = null;
  let diagramFetchSeq = 0;
  let observeCtx = "";
  let observeTask: string | null = null;
  let observeAgentPackage: string | null = null;

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

  function bumpTraceRefresh(): void {
    traceRefreshGeneration.value += 1;
  }

  function scheduleTraceRefreshBump(): void {
    if (!observeCtx) return;
    if (traceRefreshDebounceTimer !== null) {
      clearTimeout(traceRefreshDebounceTimer);
    }
    traceRefreshDebounceTimer = setTimeout(() => {
      traceRefreshDebounceTimer = null;
      bumpTraceRefresh();
      void fetchDiagram(observeCtx);
    }, TRACE_REFRESH_DEBOUNCE_MS);
  }

  function historyQueryParams(
    task?: string | null,
    agentPackage?: string | null,
  ): URLSearchParams {
    const params = new URLSearchParams();
    params.set("limit", String(HISTORY_PAGE_SIZE));
    if (task) params.set("taskId", task);
    if (agentPackage) params.set("agentPackage", agentPackage);
    return params;
  }

  function ensureHistoryStream(
    ctx: string,
    task?: string | null,
    agentPackage?: string | null,
  ): void {
    const key = `${ctx}:${task ?? ""}:${agentPackage ?? ""}`;
    if (historyStreamKey === key && historyStream) return;

    closeHistoryStream();
    const params = historyQueryParams(task, agentPackage);
    const url = `/contexts/${ctx}/conversation-history/stream?${params.toString()}`;
    const stream = new EventSource(url);

    stream.addEventListener("snapshot", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        applyConversationHistoryPage(messages, page);
        if (messages.value.length > 0) {
          hydrateState.value = "ready";
          stopPreserveRetry();
        }
        scheduleTraceRefreshBump();
      } catch {
        // ignore malformed payloads
      }
    });
    stream.addEventListener("delta", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        applyConversationHistoryDelta(
          messages,
          page,
          ConversationHistoryDeltaApplyMode.Full,
        );
        if (messages.value.length > 0) {
          hydrateState.value = "ready";
          stopPreserveRetry();
        }
        scheduleTraceRefreshBump();
      } catch {
        // ignore malformed payloads
      }
    });
    stream.addEventListener("done", () => {
      scheduleTraceRefreshBump();
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

    historyStream = stream;
    historyStreamKey = key;
  }

  function schedulePreserveTranscriptRetry(
    ctx: string,
    task?: string | null,
    agentPackage?: string | null,
  ): void {
    stopPreserveRetry();
    preserveRetryTimer = setTimeout(() => {
      preserveRetryTimer = null;
      void refreshHistoryPage(ctx, task, agentPackage, { bumpTrace: true }).then(() => {
        if (messages.value.length > 0) {
          hydrateState.value = "ready";
        } else if (hydrateState.value === "waiting") {
          hydrateState.value = "empty";
        }
      });
    }, PRESERVE_TRANSCRIPT_RETRY_MS);
  }

  async function refreshHistoryPage(
    ctx: string,
    task?: string | null,
    agentPackage?: string | null,
    options?: { bumpTrace?: boolean },
  ): Promise<void> {
    const params = historyQueryParams(task, agentPackage);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), HISTORY_FETCH_TIMEOUT_MS);
    try {
      const res = await fetch(`/contexts/${ctx}/conversation-history?${params.toString()}`, {
        signal: controller.signal,
      });
      if (!res.ok) return;
      const page = (await res.json()) as ConversationHistoryPage;
      applyConversationHistoryPage(messages, page);
      await fetchDiagram(ctx);
      if (options?.bumpTrace) {
        scheduleTraceRefreshBump();
      }
    } catch {
      // timeout or network — observation may stay empty; operator summary still shown
    } finally {
      clearTimeout(timer);
    }
  }

  async function fetchDiagram(ctx: string): Promise<void> {
    const seq = ++diagramFetchSeq;
    const text = await fetchContextMermaidDiagram(ctx);
    if (seq !== diagramFetchSeq) return;
    provenanceDiagram.value = text;
  }

  async function loadContext(
    ctx: string,
    task?: string | null,
    options?: LoadContextOptions,
  ): Promise<void> {
    const preserve = options?.preserveMessagesUntilTranscript ?? false;
    const agentPackage = options?.agentPackage ?? null;
    observeCtx = ctx;
    observeTask = task ?? null;
    observeAgentPackage = agentPackage;
    contextId.value = ctx;
    taskId.value = task ?? null;
    provenanceDiagram.value = "";
    diagramFetchSeq += 1;
    if (!preserve) {
      messages.value = [];
    }
    hydrateState.value = "loading";
    stopPreserveRetry();
    closeHistoryStream();
    if (traceRefreshDebounceTimer !== null) {
      clearTimeout(traceRefreshDebounceTimer);
      traceRefreshDebounceTimer = null;
    }
    try {
      await refreshHistoryPage(ctx, task, agentPackage);
      if (messages.value.length > 0) {
        hydrateState.value = "ready";
        ensureHistoryStream(ctx, task, agentPackage);
        scheduleTraceRefreshBump();
        return;
      }
      if (preserve) {
        hydrateState.value = "waiting";
        schedulePreserveTranscriptRetry(ctx, task, agentPackage);
      } else {
        hydrateState.value = "empty";
      }
      ensureHistoryStream(ctx, task, agentPackage);
      scheduleTraceRefreshBump();
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
    closeHistoryStream();
    if (traceRefreshDebounceTimer !== null) {
      clearTimeout(traceRefreshDebounceTimer);
      traceRefreshDebounceTimer = null;
    }
    observeCtx = "";
    observeTask = null;
    observeAgentPackage = null;
    messages.value = [];
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
    contextId,
    taskId,
    provenanceDiagram,
    traceRefreshGeneration,
    hydrateState,
    loadContext,
    setDemoMessages,
    clear,
    bumpTraceRefresh,
  };
}

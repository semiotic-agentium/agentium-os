import { ref } from "vue";
import type { ConversationHistoryIngressEffect } from "../chat/conversationHistorySync";

export interface UseTraceRefreshGenerationOptions {
  /** When provided, bump is skipped unless this returns true (unless `force` is true). */
  when?: () => boolean;
  /** Invoked after each successful bump (e.g. refetch context mermaid diagram). */
  onBump?: () => void;
}

/**
 * Monotonic generation counter for ProvenancePane / planning refresh.
 * Pair with {@link bumpTraceRefreshOnHistoryIngress} on conversation-history SSE.
 */
export function useTraceRefreshGeneration(options: UseTraceRefreshGenerationOptions = {}) {
  const traceRefreshGeneration = ref(0);
  let lastHistoryVersion = "";

  function bumpTraceRefresh(force = false): void {
    if (!force && options.when && !options.when()) return;
    traceRefreshGeneration.value += 1;
    options.onBump?.();
  }

  function resetHistoryVersion(): void {
    lastHistoryVersion = "";
  }

  function getHistoryVersion(): string {
    return lastHistoryVersion;
  }

  function setHistoryVersion(version: string): void {
    lastHistoryVersion = version;
  }

  /** Skip SSE snapshot apply when version unchanged and transcript already hydrated. */
  function isRedundantSnapshot(version: string, transcriptLoaded: boolean): boolean {
    return version === lastHistoryVersion && transcriptLoaded;
  }

  /** Bump only when `version` advances; updates tracked fingerprint. */
  function bumpOnHistoryVersion(version: string): void {
    if (version === lastHistoryVersion) return;
    lastHistoryVersion = version;
    bumpTraceRefresh();
  }

  return {
    traceRefreshGeneration,
    bumpTraceRefresh,
    resetHistoryVersion,
    getHistoryVersion,
    setHistoryVersion,
    isRedundantSnapshot,
    bumpOnHistoryVersion,
  };
}

/** Bump observe panes when conversation-history ingress applied new provenance rows. */
export function bumpTraceRefreshOnHistoryIngress(
  trace: Pick<ReturnType<typeof useTraceRefreshGeneration>, "bumpTraceRefresh">,
  effect: ConversationHistoryIngressEffect,
): void {
  if (effect.kind === "applied_full" || effect.kind === "applied_delta") {
    trace.bumpTraceRefresh();
  }
}

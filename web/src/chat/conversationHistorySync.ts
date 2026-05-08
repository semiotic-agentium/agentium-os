/**
 * Single ingress policy for provenance-backed transcript rows (GET merged pages + SSE snapshot/delta).
 *
 * - **full**: HTTP paginated merge or SSE `snapshot` → [`applyConversationHistoryPage`].
 * - **delta**: SSE `delta` only → [`applyConversationHistoryDelta`] (incremental `items`; do not run lag heuristic meant for merged pages).
 *
 * **Modes**
 * - `explicit_restore`: context picker / URL restore — defer sets `skipped` + optional retry.
 * - `background`: post-send quiet hydrate — defer leaves hydrate state unchanged.
 * - `evented`: live SSE — defer leaves hydrate state unchanged; success sets `ready`.
 *
 * A2A streaming mutations stay in [`useA2aClient`]; this module only owns conversation-history API payloads.
 */
import type { Ref } from "vue";
import type { ChatMessage, ConversationHistoryPage, HistoryHydrateState } from "../types/a2a";
import {
  applyConversationHistoryDelta,
  applyConversationHistoryPage,
  liveAgentBubbleBlocksHistoryReplace,
  shouldDeferProvenanceHistoryRebuild,
} from "./conversationHistoryHydration";

export type ConversationHistoryIngressKind = "full" | "delta";

/** Maps UI scenarios that toggle [`historyHydrateState`] differently on defer/success. */
export type ConversationHistoryIngressMode = "explicit_restore" | "background" | "evented";

export type ConversationHistoryIngressDeferReason =
  | "streaming_or_input_required"
  | "provenance_lags_live";

export type ConversationHistoryIngressEffect =
  | { kind: "noop_duplicate_version" }
  | { kind: "deferred"; reason: ConversationHistoryIngressDeferReason }
  | { kind: "applied_full" }
  | { kind: "applied_delta" };

export interface ConversationHistoryIngressDeps {
  messages: Ref<ChatMessage[]>;
  getHistoryVersion: () => string;
  setHistoryVersion: (v: string) => void;
  setHydrateState: (s: HistoryHydrateState) => void;
  setSelectedContextId: (id: string) => void;
  /** Full-page replace only; omitted wire keeps prior task id on delta apply. */
  setTaskId?: (id: string | null) => void;
  replaceLlmFromPage: (page: ConversationHistoryPage) => void;
  extendLlmFromPage: (page: ConversationHistoryPage) => void;
  /** Used when deferring merged GET hydrate (not SSE). */
  scheduleHydrateRetry?: () => void;
}

function deferReasonFromFull(messages: ChatMessage[]) {
  if (liveAgentBubbleBlocksHistoryReplace(messages)) {
    return "streaming_or_input_required" as const;
  }
  return "provenance_lags_live" as const;
}

function applyDeferSideEffects(
  deps: ConversationHistoryIngressDeps,
  mode: ConversationHistoryIngressMode,
  allowRetry: boolean,
): void {
  if (mode === "explicit_restore") {
    deps.setHydrateState("skipped");
  }
  if (allowRetry && deps.scheduleHydrateRetry) {
    deps.scheduleHydrateRetry();
  }
}

/**
 * Apply one conversation-history payload from HTTP or SSE.
 */
export function applyConversationHistoryIngress(
  deps: ConversationHistoryIngressDeps,
  input: {
    kind: ConversationHistoryIngressKind;
    mode: ConversationHistoryIngressMode;
    page: ConversationHistoryPage;
    allowRetry?: boolean;
    /**
     * When true (default), identical [`ConversationHistoryPage.version`] becomes a no-op (SSE dedupe).
     * Paginated GET hydrate must pass `false` so each merged fetch still runs policy + optional refresh.
     */
    respectDuplicateVersion?: boolean;
    /**
     * GET hydrate syncs server [`taskId`] before defer checks even when the transcript replace is skipped.
     * SSE snapshot leaves task wiring to A2A chunks unless callers opt in.
     */
    syncTaskIdFromPageBeforeDefer?: boolean;
  },
): ConversationHistoryIngressEffect {
  const { kind, mode, page } = input;
  const allowRetry = input.allowRetry !== false;
  const respectDuplicateVersion = input.respectDuplicateVersion !== false;
  const msgs = deps.messages.value;

  if (respectDuplicateVersion && page.version === deps.getHistoryVersion()) {
    return { kind: "noop_duplicate_version" };
  }

  if (kind === "full") {
    if (input.syncTaskIdFromPageBeforeDefer) {
      deps.setTaskId?.(page.taskId ?? null);
    }
    if (shouldDeferProvenanceHistoryRebuild(msgs, page)) {
      applyDeferSideEffects(deps, mode, allowRetry && mode !== "evented");
      return {
        kind: "deferred",
        reason: deferReasonFromFull(msgs),
      };
    }
    applyConversationHistoryPage(deps.messages, page);
    deps.replaceLlmFromPage(page);
    deps.setHistoryVersion(page.version);
    deps.setSelectedContextId(page.contextId);
    deps.setHydrateState("ready");
    return { kind: "applied_full" };
  }

  // Delta: never treat partial `items` as a full transcript for lag detection (snapshot-only heuristic).
  if (liveAgentBubbleBlocksHistoryReplace(msgs)) {
    applyDeferSideEffects(deps, mode, allowRetry && mode !== "evented");
    return { kind: "deferred", reason: "streaming_or_input_required" };
  }
  applyConversationHistoryDelta(deps.messages, page);
  deps.extendLlmFromPage(page);
  deps.setHistoryVersion(page.version);
  deps.setSelectedContextId(page.contextId);
  deps.setHydrateState("ready");
  return { kind: "applied_delta" };
}

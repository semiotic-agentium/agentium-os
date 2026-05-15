/**
 * Transcript contract: the Primary pane renders **conversation-history** (GET hydrate + SSE
 * snapshot/delta). A2A stream text is progressive decoration only — never treat it as canonical
 * transcript content or merge it as a competing source of truth once hydration runs.
 */
import type { Ref } from "vue";
import type {
  ChatMessage,
  ConversationHistoryItem,
  ConversationHistoryPage,
} from "../types/a2a";
import { normalizeEpochMs } from "./chatTime";
import { appendExecutionErrorCard } from "./executionErrorCard";
import { completionFromStatus, statusFromFsmPhase } from "./provenanceFsm";
import { appendProvenanceSessionStepToMessage } from "./provenanceSessionStep";
import { ensureContentBlocks, pushTextBlock } from "./chatMessageBlocks";
import { deriveToolStatus, getOrCreateToolBlockForAppend } from "./toolBlocks";
import {
  pushSystemNoticeEvent,
  pushTerminalResultEvent,
  stableJsonSignature,
} from "./toolNotificationEvents";
import { stripLegacyStructuredPlaceholderLines } from "./legacyStructuredPlaceholders";
import { isSyntheticInputRequiredPrompt } from "./inputRequiredUi";
import { isWorkflowStatusText, shouldSuppressAgentTranscriptText } from "./workflowUiFilters";

/** Best-effort: relay / delegation user rows often carry distinctive activity anchors. */
export function inferUserSpeakerKind(item: ConversationHistoryItem): "relay" | "human" {
  const a = item.activityAnchor.toLowerCase();
  if (
    a.includes("relay") ||
    a.includes("delegat") ||
    a.includes("sub_agent") ||
    a.includes("subagent")
  ) {
    return "relay";
  }
  return "human";
}

/** Non-user message entries with `type: message` and visible text (assistant output in provenance). */
export function conversationHistoryHasAssistantMessageText(
  page: ConversationHistoryPage,
): boolean {
  return page.items.some(
    (item) =>
      item.role.toLowerCase() !== "user" &&
      item.content.type === "message" &&
      typeof item.content.text === "string" &&
      item.content.text.trim().length > 0,
  );
}

/**
 * Live stream already rendered agent content that full-page hydration would wipe if provenance lags
 * (GET /conversation-history can briefly return only the user turn after the stream ends).
 */
export function chatMessagesHaveStreamedAgentBody(messages: ChatMessage[]): boolean {
  return messages.some((m) => {
    if (m.role !== "agent") return false;
    // Empty placeholder bubble (text + blocks still blank) — provenance snapshot must not wipe it
    // before INPUT_REQUIRED or first tokens arrive; otherwise stream chunks target a missing row.
    if (m.isStreaming) return true;
    if (m.text?.trim()) return true;
    const blocks = m.contentBlocks;
    if (!blocks?.length) return false;
    return blocks.some((b) => {
      if (b.type === "text") return (b.text ?? "").trim().length > 0;
      return b.type === "tool";
    });
  });
}

function agentMessageHasRenderableBody(message: ChatMessage): boolean {
  if (message.role !== "agent") return false;
  if (message.text?.trim()) return true;
  const blocks = message.contentBlocks;
  if (!blocks?.length) return false;
  return blocks.some((b) => {
    if (b.type === "text") return (b.text ?? "").trim().length > 0;
    return true;
  });
}

/**
 * Explicit restore should not let an empty live placeholder veto a real persisted transcript page.
 * Preserve only visible streamed content or genuine INPUT_REQUIRED suspension.
 */
export function explicitRestoreMayIgnoreEmptyLivePlaceholder(
  messages: ChatMessage[],
  page: ConversationHistoryPage,
): boolean {
  if (!Array.isArray(page.items) || page.items.length === 0) return false;
  const hasLocalUserTurn = messages.some((m) => m.role === "user" && (m.text ?? "").trim().length > 0);
  if (!hasLocalUserTurn) return false;
  if (messages.some((m) => m.role === "agent" && m.awaitingInput === true)) return false;
  const hasStreamingAgent = messages.some((m) => m.role === "agent" && m.isStreaming === true);
  if (!hasStreamingAgent) return false;
  return !messages.some(agentMessageHasRenderableBody);
}

/** Live assistant row is mid-stream or suspended for INPUT_REQUIRED — never replace from provenance. */
export function liveAgentBubbleBlocksHistoryReplace(messages: ChatMessage[]): boolean {
  return messages.some(
    (m) => m.role === "agent" && (m.awaitingInput === true || m.isStreaming === true),
  );
}

/** Provenance page has not caught up with assistant text already on the wire. */
export function provenanceSnapshotLagsLiveChat(
  messages: ChatMessage[],
  page: ConversationHistoryPage,
): boolean {
  return (
    chatMessagesHaveStreamedAgentBody(messages) &&
    !conversationHistoryHasAssistantMessageText(page)
  );
}

/**
 * Live sends use `user-msg-*` ids (see `useA2aClient`); persisted rows hydrate as `prov-user-*`.
 * A full provenance replace must not run while GET still omits a client user turn — otherwise the
 * UI wipes recent history even when an older assistant `message` row is already present (weak lag test).
 */
export function provenancePageOmitsPersistedClientUserTurns(
  messages: ChatMessage[],
  page: ConversationHistoryPage,
): boolean {
  const clientUsers = messages.filter(
    (m) => m.role === "user" && typeof m.id === "string" && m.id.startsWith("user-msg-"),
  );
  if (clientUsers.length === 0) return false;
  const pageUserTexts = new Set(
    page.items.flatMap((i) => {
      if (i.role.toLowerCase() !== "user") return [];
      const c = i.content;
      if (c.type !== "message") return [];
      const t = String(c.text ?? "").trim();
      return t.length > 0 ? [t] : [];
    }),
  );
  return clientUsers.some((m) => !pageUserTexts.has(String(m.text ?? "").trim()));
}

/** SSE snapshot/delta: skip rebuild/merge when the in-memory transcript must win. */
export function shouldDeferProvenanceHistoryRebuild(
  messages: ChatMessage[],
  page?: ConversationHistoryPage,
): boolean {
  if (liveAgentBubbleBlocksHistoryReplace(messages)) return true;
  if (page !== undefined && provenanceSnapshotLagsLiveChat(messages, page)) return true;
  if (page !== undefined && provenancePageOmitsPersistedClientUserTurns(messages, page)) return true;
  return false;
}

/** Apply page-level INPUT_REQUIRED hints to the last agent bubble (snapshot/delta from runner). */
export function syncResumeHintsFromPage(
  messages: Ref<ChatMessage[]>,
  page: ConversationHistoryPage,
): void {
  for (const m of messages.value) {
    if (m.role === "agent") {
      m.awaitingInput = false;
      m.inputRequiredPrompt = undefined;
    }
  }
  if (!page.awaitingInput) return;
  for (let i = messages.value.length - 1; i >= 0; i--) {
    const m = messages.value[i]!;
    if (m.role === "agent") {
      m.awaitingInput = true;
      const p = page.inputRequiredPrompt;
      const trimmed = typeof p === "string" ? p.trim() : "";
      m.inputRequiredPrompt =
        trimmed.length > 0 && !isSyntheticInputRequiredPrompt(trimmed) ? trimmed : undefined;
      break;
    }
  }
}

export function applyConversationHistoryPage(
  messages: Ref<ChatMessage[]>,
  page: ConversationHistoryPage,
): void {
  const traceTranscript = (...args: unknown[]) => {
    if (typeof window !== "undefined" && window.location.hostname === "localhost") {
      console.debug(
        "[transcript]",
        ...args.map((arg) => (typeof arg === "string" ? arg : JSON.stringify(arg))),
      );
    }
  };
  const sorted = [...page.items].sort((a, b) => a.timestampMs - b.timestampMs);
  const rebuilt: ChatMessage[] = [];
  let turnOrdinal = 0;
  let activeAgentMsg: ChatMessage | null = null;
  let sendDonePayloadSignaturesByTool = new Map<string, Set<string>>();

  const ensureAgentMsg = (item: ConversationHistoryItem): ChatMessage => {
    if (activeAgentMsg) return activeAgentMsg;
    const msg: ChatMessage = {
      id: `prov-agent-${turnOrdinal}-${item.activityAnchor}`,
      role: "agent",
      text: "",
      timestamp: new Date(normalizeEpochMs(item.timestampMs)),
      contentBlocks: [],
    };
    rebuilt.push(msg);
    activeAgentMsg = msg;
    return msg;
  };

  for (const item of sorted) {
    const isUser = item.role.toLowerCase() === "user";
    const ts = new Date(normalizeEpochMs(item.timestampMs));
    const content = item.content;

    if (isUser) {
      turnOrdinal += 1;
      activeAgentMsg = null;
      sendDonePayloadSignaturesByTool = new Map<string, Set<string>>();
      const text = content.type === "message" ? content.text : "";
      const sk = inferUserSpeakerKind(item);
      rebuilt.push({
        id: `prov-user-${item.activityAnchor}`,
        role: "user",
        text,
        timestamp: ts,
        ...(sk === "relay" ? { speakerKind: "relay" as const } : {}),
      });
      continue;
    }

    if (
      content.type === "session_step" &&
      content.op.kind === "send_done" &&
      content.send_done_replay_payload !== undefined
    ) {
      const signature = stableJsonSignature(content.send_done_replay_payload);
      const toolName = content.tool_name;
      const seen = sendDonePayloadSignaturesByTool.get(toolName) ?? new Set<string>();
      if (seen.has(signature)) {
        continue;
      }
      seen.add(signature);
      sendDonePayloadSignaturesByTool.set(toolName, seen);
    }

    if (content.type === "message" && isWorkflowStatusText(content.text ?? "")) {
      continue;
    }

    const msg = ensureAgentMsg(item);
    ensureContentBlocks(msg);

    switch (content.type) {
      case "message": {
        if (content.text && appendExecutionErrorCard(msg, content.text)) {
          break;
        }
        if (content.text) {
          const cleaned = stripLegacyStructuredPlaceholderLines(content.text);
          const t = cleaned.trim();
          if (
            t.length > 0 &&
            !shouldSuppressAgentTranscriptText(t) &&
            !isSyntheticInputRequiredPrompt(t)
          ) {
            pushTextBlock(msg, cleaned);
          }
        }
        if (Array.isArray(content.citations) && content.citations.length > 0) {
          const prev = Array.isArray(msg.metadata?.citations)
            ? (msg.metadata!.citations as unknown[]).filter((x): x is string => typeof x === "string")
            : [];
          const merged = [...new Set([...prev, ...content.citations])];
          msg.metadata = { ...(msg.metadata ?? {}), citations: merged };
        }
        break;
      }
      case "tool_call": {
        const status = statusFromFsmPhase(content.fsm_phase);
        const completion = completionFromStatus(status);
        const block = getOrCreateToolBlockForAppend(msg, content.tool_name, "start");
        block.events.push({
          kind: "assistant_tool_use",
          name: content.tool_name,
          input: content.args,
        });
        pushSystemNoticeEvent(block, `FSM phase: ${content.fsm_phase}`, `FSM phase: ${content.fsm_phase}`);
        if (completion) block.completion = completion;
        block.status = deriveToolStatus(block);
        break;
      }
      case "tool_result": {
        const status = statusFromFsmPhase(content.fsm_phase);
        const completion = completionFromStatus(status) ?? "DONE";
        const block = getOrCreateToolBlockForAppend(msg, content.tool_name, "end");
        pushSystemNoticeEvent(block, `FSM phase: ${content.fsm_phase}`, `FSM phase: ${content.fsm_phase}`);
        pushTerminalResultEvent(
          block,
          "success",
          typeof content.outcome === "string" ? content.outcome : JSON.stringify(content.outcome),
        );
        block.completion = completion;
        block.status = deriveToolStatus(block);
        break;
      }
      case "session_step": {
        appendProvenanceSessionStepToMessage(msg, content);
        break;
      }
    }
  }

  messages.value = rebuilt;
  traceTranscript("applyPage.commit", {
    contextId: page.contextId,
    version: page.version,
    items: sorted.length,
    rebuilt: rebuilt.length,
    sample: rebuilt.slice(0, 4).map((m) => ({
      id: m.id,
      role: m.role,
      text: (m.text ?? "").slice(0, 60),
    })),
  });
  syncResumeHintsFromPage(messages, page);
}

export function applyConversationHistoryDelta(
  messages: Ref<ChatMessage[]>,
  page: ConversationHistoryPage,
): void {
  if (Array.isArray(page.items) && page.items.length > 0) {
    const sorted = [...page.items].sort((a, b) => a.timestampMs - b.timestampMs);
    for (const item of sorted) {
      const isUser = item.role.toLowerCase() === "user";
      const ts = new Date(normalizeEpochMs(item.timestampMs));
      const content = item.content;
      if (isUser) {
        const text = content.type === "message" ? content.text : "";
        const sk = inferUserSpeakerKind(item);
        messages.value.push({
          id: `prov-user-${item.activityAnchor}`,
          role: "user",
          text,
          timestamp: ts,
          ...(sk === "relay" ? { speakerKind: "relay" as const } : {}),
        });
        continue;
      }

      if (content.type === "message" && isWorkflowStatusText(content.text ?? "")) {
        continue;
      }

      let msg = messages.value[messages.value.length - 1];
      if (!msg || msg.role !== "agent") {
        msg = {
          id: `prov-agent-${item.activityAnchor}`,
          role: "agent",
          text: "",
          timestamp: ts,
          contentBlocks: [],
        };
        messages.value.push(msg);
      }
      ensureContentBlocks(msg);

      switch (content.type) {
        case "message":
          if (content.text && !appendExecutionErrorCard(msg, content.text)) {
            const cleaned = stripLegacyStructuredPlaceholderLines(content.text);
            const t = cleaned.trim();
            if (
              t.length > 0 &&
              !shouldSuppressAgentTranscriptText(t) &&
              !isSyntheticInputRequiredPrompt(t)
            ) {
              pushTextBlock(msg, cleaned);
            }
          }
          break;
        case "tool_call": {
          const status = statusFromFsmPhase(content.fsm_phase);
          const completion = completionFromStatus(status);
          const block = getOrCreateToolBlockForAppend(msg, content.tool_name, "start");
          block.events.push({ kind: "assistant_tool_use", name: content.tool_name, input: content.args });
          pushSystemNoticeEvent(block, `FSM phase: ${content.fsm_phase}`, `FSM phase: ${content.fsm_phase}`);
          if (completion) block.completion = completion;
          block.status = deriveToolStatus(block);
          break;
        }
        case "tool_result": {
          const status = statusFromFsmPhase(content.fsm_phase);
          const completion = completionFromStatus(status) ?? "DONE";
          const block = getOrCreateToolBlockForAppend(msg, content.tool_name, "end");
          pushSystemNoticeEvent(block, `FSM phase: ${content.fsm_phase}`, `FSM phase: ${content.fsm_phase}`);
          pushTerminalResultEvent(
            block,
            "success",
            typeof content.outcome === "string" ? content.outcome : JSON.stringify(content.outcome),
          );
          block.completion = completion;
          block.status = deriveToolStatus(block);
          break;
        }
        case "session_step": {
          appendProvenanceSessionStepToMessage(msg, content);
          break;
        }
      }
    }
  }
  syncResumeHintsFromPage(messages, page);
}

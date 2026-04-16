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
import { isWorkflowStatusText } from "./workflowUiFilters";

export function applyConversationHistoryPage(
  messages: Ref<ChatMessage[]>,
  page: ConversationHistoryPage,
): void {
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
      rebuilt.push({
        id: `prov-user-${item.activityAnchor}`,
        role: "user",
        text,
        timestamp: ts,
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
          pushTextBlock(msg, content.text);
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
}

export function applyConversationHistoryDelta(
  messages: Ref<ChatMessage[]>,
  page: ConversationHistoryPage,
): void {
  if (!Array.isArray(page.items) || page.items.length === 0) return;
  const sorted = [...page.items].sort((a, b) => a.timestampMs - b.timestampMs);
  for (const item of sorted) {
    const isUser = item.role.toLowerCase() === "user";
    const ts = new Date(normalizeEpochMs(item.timestampMs));
    const content = item.content;
    if (isUser) {
      const text = content.type === "message" ? content.text : "";
      messages.value.push({
        id: `prov-user-${item.activityAnchor}`,
        role: "user",
        text,
        timestamp: ts,
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
          pushTextBlock(msg, content.text);
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

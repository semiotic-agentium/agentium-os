import type { ChatMessage, SessionStepOp } from "../types/a2a";
import { appendInterAgentA2aSendDoneArtifacts } from "./internalA2aComms";
import { deriveToolStatus, getOrCreateToolBlockForAppend, type ToolAppendMode } from "./toolBlocks";
import {
  pushReadReplayEvent,
  pushSystemNoticeEvent,
  pushSessionStepDetailEvent,
} from "./toolNotificationEvents";
import { summarizeSessionStepContent } from "./sessionStepContent";

export type ProvenanceSessionStepContent = {
  type: "session_step";
  tool_name: string;
  op: SessionStepOp;
  send_done_replay_payload?: unknown;
  read_replay_lines?: string[];
};

export function appendProvenanceSessionStepToMessage(
  msg: ChatMessage,
  content: ProvenanceSessionStepContent,
): void {
  const stepKind = content.op.kind;
  const done = stepKind === "send_done" || stepKind === "finish";
  const completion = done ? "DONE" : undefined;
  const mode: ToolAppendMode = stepKind === "open" ? "start" : done ? "end" : "progress";
  const block = getOrCreateToolBlockForAppend(msg, content.tool_name, mode);
  pushSystemNoticeEvent(block, `Session step: ${stepKind}`, `Session step: ${stepKind}`);
  const stepDetail = summarizeSessionStepContent(content.tool_name, content.op);
  if (stepDetail) {
    pushSessionStepDetailEvent(block, stepDetail);
  }
  appendInterAgentA2aSendDoneArtifacts(
    block,
    content.tool_name,
    stepKind,
    content.op,
    content.send_done_replay_payload,
  );
  if (Array.isArray(content.read_replay_lines) && content.read_replay_lines.length > 0) {
    pushReadReplayEvent(block, content.read_replay_lines.join("\n"));
  }
  if (completion) block.completion = completion;
  block.status = deriveToolStatus(block);
}

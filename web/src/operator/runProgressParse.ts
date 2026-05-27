/** Parse dispatch-unit and subscriber progress from agent ack detail strings. */

import type { EventPublishAcceptance } from "../types/events";
import type { ChatMessage, ToolNotificationBlock } from "../types/a2a";
import { buildFsmSteps } from "../chat/toolCardDisplay";

export interface UnitProgress {
  done: number;
  total: number;
}

const UNIT_PROGRESS_RE = /(\d+)\/(\d+)\s+unit\(s\)/i;

export function parseUnitProgress(detail: string): UnitProgress | null {
  const match = detail.match(UNIT_PROGRESS_RE);
  if (!match) return null;
  const done = Number.parseInt(match[1]!, 10);
  const total = Number.parseInt(match[2]!, 10);
  if (!Number.isFinite(done) || !Number.isFinite(total) || total <= 0) return null;
  return { done, total };
}

/** Minimum done/total ratio across subscriber acceptances (most incomplete wins). */
export function worstUnitProgress(
  acceptances: EventPublishAcceptance[] | undefined,
): (UnitProgress & { agentLabel: string }) | null {
  if (!acceptances?.length) return null;
  let worst: (UnitProgress & { agentLabel: string; ratio: number }) | null = null;
  for (const a of acceptances) {
    const parsed = parseUnitProgress(a.detail);
    if (!parsed) continue;
    const ratio = parsed.done / parsed.total;
    const agentLabel = `${a.agent_package}/${a.agent_instance_id}`;
    if (!worst || ratio < worst.ratio) {
      worst = { ...parsed, agentLabel, ratio };
    }
  }
  if (!worst) return null;
  const { agentLabel, done, total } = worst;
  return { agentLabel, done, total };
}

function toolBlockHasOpenSession(block: ToolNotificationBlock): boolean {
  if (block.completion === "DONE" || block.completion === "INTERRUPTED") return false;
  if (block.status === "Running" || block.completion === "INPUT_REQUIRED") return true;
  const steps = buildFsmSteps(block.events, block.status);
  const lastKey = steps[steps.length - 1]?.key;
  return lastKey === "open";
}

/** True when the transcript shows an in-flight host tool session (open FSM, not finished). */
export function hasOpenToolSession(messages: ChatMessage[]): boolean {
  for (let i = messages.length - 1; i >= 0; i--) {
    const msg = messages[i];
    if (!msg || msg.role !== "agent") continue;
    for (const block of msg.contentBlocks ?? []) {
      if (block.type === "tool" && toolBlockHasOpenSession(block)) return true;
    }
  }
  return false;
}

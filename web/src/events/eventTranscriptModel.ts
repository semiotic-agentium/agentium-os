/** Event Console transcript row model — presentation layer over ChatMessage[]. */

import type { ChatMessage, ContentBlock, OperationalContentBlock } from "../types/a2a";
import type { EventDispatchPhase, EventPublishResponse } from "../types/events";
import type { TraceObserveState } from "../composables/useEventObservation";
import { deriveEventRunStatus } from "../operator/runStatus";
import { isIngressWireBody } from "./ingressWireBody";
import { formatPublishAcceptanceSummary } from "./publishOutcome";

export type EventTranscriptRowKind =
  | "milestone"
  | "ingress_wire"
  | "operational"
  | "agent_turn"
  | "skeleton";

export interface EventRunMeta {
  dispatchPhase: EventDispatchPhase;
  hydrateState: TraceObserveState;
  lastPublishOutcome: EventPublishResponse | null;
  publishError: string | null;
  waitingForIngress: boolean;
  hasPublishedRun: boolean;
}

export interface EventTranscriptMilestoneRow {
  kind: "milestone";
  key: string;
  label: string;
  summary: string;
  detail?: string;
  severity: "info" | "success" | "warning" | "error";
}

export interface EventTranscriptIngressRow {
  kind: "ingress_wire";
  key: string;
  message: ChatMessage;
  pending: boolean;
}

export interface EventTranscriptOperationalRow {
  kind: "operational";
  key: string;
  block: OperationalContentBlock;
  message: ChatMessage;
}

export interface EventTranscriptAgentRow {
  kind: "agent_turn";
  key: string;
  message: ChatMessage;
}

export interface EventTranscriptSkeletonRow {
  kind: "skeleton";
  key: string;
  variant: "ingress" | "agent" | "tool";
}

export type EventTranscriptRow =
  | EventTranscriptMilestoneRow
  | EventTranscriptIngressRow
  | EventTranscriptOperationalRow
  | EventTranscriptAgentRow
  | EventTranscriptSkeletonRow;

/** Stable merge key for ingress wire rows (local + provenance). */
export const EVENT_TRANSCRIPT_INGRESS_KEY = "row:ingress-wire";

/** Stable key for publish milestone in timeline. */
export const EVENT_TRANSCRIPT_PUBLISH_MILESTONE_KEY = "row:milestone:publish";

export function rowKeyForMessage(message: ChatMessage): string {
  if (isIngressWireMessage(message)) {
    return EVENT_TRANSCRIPT_INGRESS_KEY;
  }
  if (message.id.includes("publish-trace-outcome") || message.id.includes("milestone-publish")) {
    return EVENT_TRANSCRIPT_PUBLISH_MILESTONE_KEY;
  }
  return message.id || `row:${message.role}:${message.timestamp.getTime()}`;
}

export function isIngressWireMessage(message: ChatMessage): boolean {
  if (message.role !== "user") return false;
  if (message.speakerKind === "ingress") return true;
  return isIngressWireBody(message.text ?? "");
}

function isHostOperationalMessage(message: ChatMessage): boolean {
  if (message.role !== "agent") return false;
  if (message.speakerKind === "host" || message.speakerKind === "system") {
    return Boolean(message.contentBlocks?.some((b) => b.type === "operational"));
  }
  return false;
}

function isLocalPublishTraceMessage(message: ChatMessage): boolean {
  return (
    message.id.includes("operator-publish-trace") ||
    message.id.includes("publish-trace-outcome") ||
    message.id.includes("publish-trace-outbound")
  );
}

function operationalBlocks(message: ChatMessage): OperationalContentBlock[] {
  return (message.contentBlocks ?? []).filter(
    (b): b is OperationalContentBlock => b.type === "operational",
  );
}

export function transcriptPhaseLabel(
  dispatchPhase: EventDispatchPhase,
  hydrateState: TraceObserveState,
): string {
  return deriveEventRunStatus({
    dispatchPhase,
    hydrateState,
    lastPublishOutcome: null,
    publishError: null,
    waitingForIngress: false,
    transcriptMessages: [],
    contextId: null,
  }).label;
}

function buildPublishMilestone(meta: EventRunMeta): EventTranscriptMilestoneRow | null {
  if (meta.publishError) {
    return {
      kind: "milestone",
      key: EVENT_TRANSCRIPT_PUBLISH_MILESTONE_KEY,
      label: "Publish",
      summary: meta.publishError,
      severity: "error",
    };
  }
  const o = meta.lastPublishOutcome;
  if (!o) return null;
  const summary = formatPublishAcceptanceSummary(o);
  const hasFailures = o.failures.length > 0 || o.subscribers_accepted < o.subscribers_matched;
  const detail =
    o.failures.length > 0
      ? o.failures
          .map((f) => `${f.agent_package}/${f.agent_instance_id}: ${f.detail}`)
          .join("\n")
      : undefined;
  return {
    kind: "milestone",
    key: EVENT_TRANSCRIPT_PUBLISH_MILESTONE_KEY,
    label: "Publish",
    summary,
    detail,
    severity: hasFailures ? "error" : o.subscribers_accepted > 0 ? "success" : "warning",
  };
}

function provenanceHasPublishFailures(messages: ChatMessage[]): boolean {
  return messages.some((m) =>
    operationalBlocks(m).some(
      (b) => b.kind === "dispatch_rejected" || b.kind === "dispatch_transport_error",
    ),
  );
}

export function normalizeEventTranscriptRows(
  messages: ChatMessage[],
  meta: EventRunMeta,
  options?: { includeSkeletons?: boolean },
): EventTranscriptRow[] {
  const rows: EventTranscriptRow[] = [];
  const milestone = buildPublishMilestone(meta);
  const showMilestone =
    milestone &&
    (meta.publishError ||
      (milestone.severity === "error" && !provenanceHasPublishFailures(messages))) &&
    (meta.lastPublishOutcome || meta.publishError);
  if (showMilestone && milestone) {
    rows.push(milestone);
  }

  for (const message of messages) {
    if (isLocalPublishTraceMessage(message)) {
      continue;
    }
    if (isIngressWireMessage(message)) {
      rows.push({
        kind: "ingress_wire",
        key: EVENT_TRANSCRIPT_INGRESS_KEY,
        message,
        pending: false,
      });
      continue;
    }
    if (isHostOperationalMessage(message)) {
      for (const block of operationalBlocks(message)) {
        rows.push({
          kind: "operational",
          key: `${message.id}:${block.kind}:${block.summary}`,
          block,
          message,
        });
      }
      continue;
    }
    rows.push({
      kind: "agent_turn",
      key: message.id,
      message,
    });
  }

  if (options?.includeSkeletons) {
    const hasIngress = rows.some((r) => r.kind === "ingress_wire");
    if (!hasIngress && (meta.waitingForIngress || meta.hasPublishedRun)) {
      rows.push({ kind: "skeleton", key: "skeleton:ingress", variant: "ingress" });
    }
    if (meta.hasPublishedRun && (meta.hydrateState === "loading" || meta.hydrateState === "waiting")) {
      if (!rows.some((r) => r.kind === "agent_turn")) {
        rows.push({ kind: "skeleton", key: "skeleton:agent", variant: "agent" });
        rows.push({ kind: "skeleton", key: "skeleton:tool", variant: "tool" });
      }
    }
  }

  return sortEventTranscriptRows(rows);
}

function eventTranscriptRowTimestamp(row: EventTranscriptRow): number {
  if (row.kind === "ingress_wire" || row.kind === "agent_turn" || row.kind === "operational") {
    return row.message.timestamp.getTime();
  }
  return 0;
}

/** Transcript rows follow provenance timestamp order. */
export function sortEventTranscriptRows(rows: EventTranscriptRow[]): EventTranscriptRow[] {
  return [...rows].sort((a, b) => {
    const ta = eventTranscriptRowTimestamp(a);
    const tb = eventTranscriptRowTimestamp(b);
    if (ta !== tb) return ta - tb;
    return a.key.localeCompare(b.key);
  });
}

export function sortMessagesChronologically(messages: ChatMessage[]): ChatMessage[] {
  return [...messages].sort((a, b) => {
    const ta = a.timestamp.getTime();
    const tb = b.timestamp.getTime();
    if (ta !== tb) return ta - tb;
    return a.id.localeCompare(b.id);
  });
}

/** Dedupe by stable row key; provenance replaces local for ingress. */
export function mergeMessagesByRowKey(
  provenance: ChatMessage[],
  local: ChatMessage[],
): ChatMessage[] {
  const byKey = new Map<string, ChatMessage>();

  for (const message of local) {
    byKey.set(rowKeyForMessage(message), message);
  }

  for (const message of provenance) {
    const key = rowKeyForMessage(message);
    if (key === EVENT_TRANSCRIPT_INGRESS_KEY) {
      byKey.set(key, message);
      continue;
    }
    if (!byKey.has(key)) {
      byKey.set(key, message);
    } else if (key === message.id) {
      byKey.set(key, message);
    }
  }

  return sortMessagesChronologically(Array.from(byKey.values()));
}

export function operationalBadgeLabel(block: OperationalContentBlock): string {
  if (block.kind.startsWith("dispatch_")) return "Host";
  if (block.kind === "llm_call_failed" || block.kind === "prompt_rejected") return "Error";
  return "System";
}

export function operationalSeverityClass(block: OperationalContentBlock): string {
  if (block.severity === "error") return "operational-card--error";
  if (block.severity === "warning") return "operational-card--warning";
  return "operational-card--info";
}

/** Event timeline lane surface (left accent + chip), not chat operational-card. */
export function operationalLaneClass(block: OperationalContentBlock): string {
  if (block.severity === "error") return "event-lane-card--error";
  if (block.severity === "warning") return "event-lane-card--warning";
  if (block.kind.startsWith("dispatch_")) return "event-lane-card--host";
  if (block.kind === "source_poll_recorded") return "event-lane-card--system";
  return "event-lane-card--system";
}

export function operationalRailDotClass(block: OperationalContentBlock): string {
  if (block.severity === "error") return "event-transcript-rail-dot--error";
  if (block.severity === "warning") return "event-transcript-rail-dot--warning";
  if (block.kind.startsWith("dispatch_")) return "event-transcript-rail-dot--host";
  return "event-transcript-rail-dot--system";
}

export function operationalChipClass(block: OperationalContentBlock): string {
  if (block.severity === "error") return "event-lane-chip--error";
  if (block.kind.startsWith("dispatch_")) return "event-lane-chip--host";
  return "event-lane-chip--system";
}

export function isToolBlock(block: ContentBlock): block is import("../types/a2a").ToolNotificationBlock {
  return block.type === "tool";
}

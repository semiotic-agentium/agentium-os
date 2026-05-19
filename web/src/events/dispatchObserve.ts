/** Scope resolution and operator dispatch summary helpers for Event Console observation. */

import type { ChatMessage } from "../types/a2a";
import type {
  AgentDeliverableMessageShape,
  AgentDispatchAck,
  DerivedDispatchEnvelope,
  EventDispatchScope,
} from "../types/events";

export interface DispatchedScope {
  contextId: string;
  taskId?: string | null;
  messageId?: string | null;
  /** Set when scope comes from a dispatch ack so observation does not follow another agent. */
  agentPackage?: string | null;
  agentInstanceId?: string | null;
}

export interface ObservationAgentRef {
  agentPackage: string;
  agentInstanceId: string;
}

export function scopeFromRecord(
  req: Record<string, unknown> | undefined | null,
): DispatchedScope | null {
  if (!req) return null;
  const contextId = typeof req.context_id === "string" ? req.context_id : null;
  if (!contextId) return null;
  return {
    contextId,
    taskId: typeof req.task_id === "string" ? req.task_id : null,
    messageId: typeof req.message_id === "string" ? req.message_id : null,
  };
}

export function scopeFromAck(ack: AgentDispatchAck | null | undefined): DispatchedScope | null {
  if (!ack?.context_id) return null;
  return {
    contextId: ack.context_id,
    taskId: ack.task_id ?? null,
    messageId: ack.message_id ?? null,
  };
}

export function scopeFromContextId(contextId: string | null | undefined): DispatchedScope | null {
  if (!contextId) return null;
  return { contextId, taskId: null, messageId: null };
}

export function scopeFromDraftScope(
  scope: EventDispatchScope,
): DispatchedScope | null {
  if (scope.kind === "new_context") {
    return null;
  }
  return {
    contextId: scope.context_id,
    taskId: scope.kind === "existing_task" ? scope.task_id : null,
  };
}

export function dispatchedScopeMatchesAgent(
  scope: DispatchedScope | null | undefined,
  agent: ObservationAgentRef | null | undefined,
): boolean {
  if (!scope) {
    return false;
  }
  if (!scope.agentPackage || !scope.agentInstanceId) {
    return true;
  }
  if (!agent?.agentPackage || !agent?.agentInstanceId) {
    return false;
  }
  return (
    scope.agentPackage === agent.agentPackage &&
    scope.agentInstanceId === agent.agentInstanceId
  );
}

/**
 * Resolve which provenance context to observe for the current Event Console session.
 *
 * In compose mode, toolbar context pickers update the draft scope but must not reuse a
 * prior dispatch scope from another agent. History mode may browse any listed context.
 */
export function resolveObservationScope(input: {
  lastDispatchedScope: DispatchedScope | null;
  draftScope: EventDispatchScope;
  selectedContextId: string | null;
  previewRequest: Record<string, unknown> | undefined | null;
  currentAgent: ObservationAgentRef | null;
  mode: "compose" | "history";
}): DispatchedScope | null {
  const fromDispatch = dispatchedScopeMatchesAgent(
    input.lastDispatchedScope,
    input.currentAgent,
  )
    ? input.lastDispatchedScope
    : null;
  const fromDraft = scopeFromDraftScope(input.draftScope);
  const fromPreview = scopeFromRecord(input.previewRequest);
  const fromPicker =
    input.mode === "history"
      ? scopeFromContextId(input.selectedContextId)
      : null;

  // Explicit draft scope (continue context/task) wins over a prior dispatch ack so
  // operators can inspect another context without clearing the last ack.
  return fromDraft ?? fromDispatch ?? fromPreview ?? fromPicker;
}

export function buildOperatorDispatchTraceMessages(input: {
  agentPackage: string;
  agentInstanceId: string;
  messageShape: AgentDeliverableMessageShape | undefined;
  envelope: DerivedDispatchEnvelope | null;
  sampleLabel?: string;
  ack: AgentDispatchAck | null;
  dispatchError: string | null;
}): ChatMessage[] {
  const now = new Date();
  const shapeLabel = input.messageShape?.display_name ?? "Event dispatch";
  const routing = input.envelope?.routingKey ?? "—";
  const messageType = input.envelope?.messageType ?? "—";
  const sample = input.sampleLabel ? `\nSample: ${input.sampleLabel}` : "";
  const outbound = [
    `Operator dispatch`,
    `Agent: ${input.agentPackage}/${input.agentInstanceId}`,
    `Message type: ${shapeLabel} (${messageType})`,
    `Routing: ${routing}${sample}`,
  ].join("\n");

  const rows: ChatMessage[] = [
    {
      id: "operator-dispatch-trace-outbound",
      role: "user",
      text: outbound,
      timestamp: now,
    },
  ];

  if (input.dispatchError) {
    rows.push({
      id: "operator-dispatch-trace-error",
      role: "agent",
      text: `Dispatch failed:\n${input.dispatchError}`,
      timestamp: now,
    });
    return rows;
  }

  if (input.ack) {
    const status = input.ack.accepted ? "Accepted" : "Rejected";
    const detail = input.ack.detail ? `\n${input.ack.detail}` : "";
    rows.push({
      id: "operator-dispatch-trace-ack",
      role: "agent",
      text: `${status}${detail}`.trim(),
      timestamp: now,
    });
  }

  return rows;
}

/** @deprecated Use buildOperatorDispatchTraceMessages */
export const buildConsoleTraceFallbackMessages = buildOperatorDispatchTraceMessages;

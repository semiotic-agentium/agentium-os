/** Scope resolution and operator publish summary helpers for Event Console observation. */

import type { ChatMessage } from "../types/a2a";
import type {
  AgentDeliverableMessageShape,
  DerivedDispatchEnvelope,
  EventDispatchScope,
  EventPublishResponse,
} from "../types/events";

export interface PublishedScope {
  contextId: string;
  taskId?: string | null;
  messageId?: string | null;
  /** Set when scope comes from a publish run so observation does not follow another agent. */
  agentPackage?: string | null;
  agentInstanceId?: string | null;
}

/** @deprecated Use PublishedScope */
export type DispatchedScope = PublishedScope;

export interface ObservationAgentRef {
  agentPackage: string;
  agentInstanceId: string;
}

export function scopeFromRecord(
  req: Record<string, unknown> | undefined | null,
): PublishedScope | null {
  if (!req) return null;
  const contextId = typeof req.context_id === "string" ? req.context_id : null;
  if (!contextId) return null;
  return {
    contextId,
    taskId: typeof req.task_id === "string" ? req.task_id : null,
    messageId: typeof req.message_id === "string" ? req.message_id : null,
  };
}

export function scopeFromContextId(contextId: string | null | undefined): PublishedScope | null {
  if (!contextId) return null;
  return { contextId, taskId: null, messageId: null };
}

export function scopeFromDraftScope(scope: EventDispatchScope): PublishedScope | null {
  if (scope.kind === "new_context") {
    return null;
  }
  return {
    contextId: scope.context_id,
    taskId: scope.kind === "existing_task" ? scope.task_id : null,
  };
}

export function publishedScopeMatchesAgent(
  scope: PublishedScope | null | undefined,
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

/** @deprecated Use publishedScopeMatchesAgent */
export const dispatchedScopeMatchesAgent = publishedScopeMatchesAgent;

/**
 * Resolve which provenance context to observe for the current Event Console session.
 *
 * In compose mode, toolbar context pickers update the draft scope but must not reuse a
 * prior publish scope from another agent. History mode may browse any listed context.
 */
export function resolveObservationScope(input: {
  lastPublishedScope: PublishedScope | null;
  draftScope: EventDispatchScope;
  selectedContextId: string | null;
  previewProducedEvent: Record<string, unknown> | undefined | null;
  currentAgent: ObservationAgentRef | null;
  mode: "compose" | "history";
}): PublishedScope | null {
  const fromPublish = publishedScopeMatchesAgent(
    input.lastPublishedScope,
    input.currentAgent,
  )
    ? input.lastPublishedScope
    : null;
  const fromDraft = scopeFromDraftScope(input.draftScope);
  const fromPreview = scopeFromRecord(input.previewProducedEvent);
  const fromPicker =
    input.mode === "history" ? scopeFromContextId(input.selectedContextId) : null;

  return fromDraft ?? fromPublish ?? fromPreview ?? fromPicker;
}

export function buildOperatorPublishTraceMessages(input: {
  agentPackage: string;
  agentInstanceId: string;
  messageShape: AgentDeliverableMessageShape | undefined;
  envelope: DerivedDispatchEnvelope | null;
  sampleLabel?: string;
  outcome: EventPublishResponse | null;
  publishError: string | null;
}): ChatMessage[] {
  const now = new Date();
  const shapeLabel = input.messageShape?.display_name ?? "Event publish";
  const routing = input.envelope?.routingKey ?? "—";
  const messageType = input.envelope?.messageType ?? "—";
  const sample = input.sampleLabel ? `\nSample: ${input.sampleLabel}` : "";
  const outbound = [
    "Operator publish (host ingress)",
    `Compose agent: ${input.agentPackage}/${input.agentInstanceId}`,
    `Message type: ${shapeLabel} (${messageType})`,
    `Routing: ${routing}${sample}`,
  ].join("\n");

  const rows: ChatMessage[] = [
    {
      id: "operator-publish-trace-outbound",
      role: "user",
      text: outbound,
      timestamp: now,
    },
  ];

  if (input.publishError) {
    rows.push({
      id: "operator-publish-trace-error",
      role: "agent",
      text: `Publish failed:\n${input.publishError}`,
      timestamp: now,
    });
    return rows;
  }

  if (input.outcome) {
    const o = input.outcome;
    const failureLines =
      o.failures.length > 0
        ? `\nFailures:\n${o.failures
            .map((f) => `- ${f.agent_package}/${f.agent_instance_id}: ${f.detail}`)
            .join("\n")}`
        : "";
    rows.push({
      id: "operator-publish-trace-outcome",
      role: "agent",
      text: `Published ${o.subscribers_accepted}/${o.subscribers_matched} subscriber(s)${failureLines}`.trim(),
      timestamp: now,
    });
  }

  return rows;
}

/** @deprecated Use buildOperatorPublishTraceMessages */
export const buildOperatorDispatchTraceMessages = buildOperatorPublishTraceMessages;

/** @deprecated Use buildOperatorPublishTraceMessages */
export const buildConsoleTraceFallbackMessages = buildOperatorPublishTraceMessages;

/** True when transcript includes host-written ingress poll/unit user rows. */
export function transcriptHasIngressUserRows(messages: ChatMessage[]): boolean {
  return messages.some(
    (m) =>
      m.role === "user" &&
      (m.speakerKind === "ingress" ||
        m.id.includes("ingress-poll-user") ||
        m.id.includes("ingress-unit-user")),
  );
}

/** @deprecated Use transcriptHasIngressUserRows */
export const transcriptHasHostIngress = transcriptHasIngressUserRows;

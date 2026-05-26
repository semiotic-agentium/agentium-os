/** Scope resolution and operator publish summary helpers for Event Console observation. */

import type { ChatMessage } from "../types/a2a";
import type {
  AgentDeliverableMessageShape,
  DerivedDispatchEnvelope,
  EventDispatchScope,
  EventPublishResponse,
  ObservationSource,
  PreviewProducedEvent,
} from "../types/events";
import { INGRESS_WIRE_BODY_DELIMITER } from "./ingressWireBody";

export type { ObservationSource };

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
 * Observation is decoupled from compose `draftScope`: the event-run picker sets
 * `observationSource: "picker"` without mutating the publish draft.
 */
export function resolveObservationScope(input: {
  lastPublishedScope: PublishedScope | null;
  draftScope: EventDispatchScope;
  observedContextId: string | null;
  previewProducedEvent: PreviewProducedEvent | null;
  currentAgent: ObservationAgentRef | null;
  observationSource: ObservationSource;
}): PublishedScope | null {
  const fromPicker = scopeFromContextId(input.observedContextId);
  const fromPublish = publishedScopeMatchesAgent(
    input.lastPublishedScope,
    input.currentAgent,
  )
    ? input.lastPublishedScope
    : null;
  const fromDraft = scopeFromDraftScope(input.draftScope);
  const fromPreview = scopeFromRecord(
    input.previewProducedEvent as Record<string, unknown> | null,
  );

  if (input.observationSource === "picker") {
    if (fromPicker) return fromPicker;
    return fromPublish ?? fromDraft ?? fromPreview;
  }
  if (input.observationSource === "publish") {
    if (fromPublish) return fromPublish;
    return fromDraft ?? fromPreview ?? fromPicker;
  }
  return fromDraft ?? fromPreview ?? fromPublish ?? null;
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
    const acceptanceLines =
      (o.acceptances?.length ?? 0) > 0
        ? `\n${o.acceptances!
            .map((a) => `- ${a.agent_package}/${a.agent_instance_id}: ${a.detail}`)
            .join("\n")}`
        : "";
    const failureLines =
      o.failures.length > 0
        ? `\nFailures:\n${o.failures
            .map((f) => `- ${f.agent_package}/${f.agent_instance_id}: ${f.detail}`)
            .join("\n")}`
        : "";
    rows.push({
      id: "operator-publish-trace-outcome",
      role: "agent",
      text: `Published ${o.subscribers_accepted}/${o.subscribers_matched} subscriber(s)${acceptanceLines}${failureLines}`.trim(),
      timestamp: now,
    });
  }

  return rows;
}

/** @deprecated Use buildOperatorPublishTraceMessages */
export const buildOperatorDispatchTraceMessages = buildOperatorPublishTraceMessages;

/** @deprecated Use buildOperatorPublishTraceMessages */
export const buildConsoleTraceFallbackMessages = buildOperatorPublishTraceMessages;

/** First dispatch-unit task id for a context (multi-unit: first match). */
export async function resolveDispatchUnitTaskId(
  contextId: string,
): Promise<string | null> {
  const res = await fetch(`/contexts/${encodeURIComponent(contextId)}/planning`);
  if (!res.ok) return null;
  const data = (await res.json()) as { allTaskIds?: string[] };
  const ids = data.allTaskIds ?? [];
  const unit = ids.find((id) => id.startsWith("dispatch-unit-"));
  if (unit) return unit;
  return ids.length === 1 ? (ids[0] ?? null) : null;
}

function recordsFromPreviewBatch(preview: unknown): unknown[] {
  if (!preview || typeof preview !== "object" || Array.isArray(preview)) {
    return [];
  }
  const messages = (preview as { messages?: unknown }).messages;
  if (!Array.isArray(messages) || messages.length === 0) {
    return [];
  }
  const batch = messages[0];
  if (!batch || typeof batch !== "object" || Array.isArray(batch)) {
    return [];
  }
  const records = (batch as { records?: unknown }).records;
  return Array.isArray(records) ? records : [];
}

/** Local ingress wire row matching host `format_source_records_wire_body`. */
export function buildIngressWireUserMessage(records: unknown[]): ChatMessage {
  const jsonText = JSON.stringify({ records }, null, 2);
  return {
    id: "event-console-local-ingress-wire",
    role: "user",
    speakerKind: "ingress",
    text: `${INGRESS_WIRE_BODY_DELIMITER}\n${jsonText}`,
    timestamp: new Date(),
  };
}

/**
 * Optimistic transcript rows after publish until provenance conversation-history hydrates.
 */
export function buildEventConsoleLocalTranscript(input: {
  previewProducedEvent: unknown;
  outcome: EventPublishResponse | null;
  publishError: string | null;
  agentPackage: string;
  agentInstanceId: string;
  messageShape: AgentDeliverableMessageShape | undefined;
  envelope: DerivedDispatchEnvelope | null;
  sampleLabel?: string;
}): ChatMessage[] {
  const rows: ChatMessage[] = [];
  const records = recordsFromPreviewBatch(input.previewProducedEvent);
  if (records.length > 0) {
    rows.push(buildIngressWireUserMessage(records));
  }

  const summaryRows = buildOperatorPublishTraceMessages({
    agentPackage: input.agentPackage,
    agentInstanceId: input.agentInstanceId,
    messageShape: input.messageShape,
    envelope: input.envelope,
    sampleLabel: input.sampleLabel,
    outcome: input.outcome,
    publishError: input.publishError,
  });
  for (const row of summaryRows) {
    if (row.role === "user" && records.length > 0) {
      continue;
    }
    rows.push({
      ...row,
      id: `event-console-local-${row.id}`,
    });
  }
  return rows;
}

export function localTranscriptMatchesScope(
  scope: PublishedScope | null,
  contextId: string | null,
): boolean {
  if (!scope || !contextId) return false;
  return scope.contextId === contextId;
}

/** Merge provenance transcript with optimistic publish rows until host ingress lands. */
export function mergeEventConsoleTranscript(
  provenance: ChatMessage[],
  local: ChatMessage[],
): ChatMessage[] {
  if (local.length === 0) return provenance;

  const hasIngress = transcriptHasIngressUserRows(provenance);
  let retainedLocal = local;
  if (hasIngress) {
    retainedLocal = local.filter(
      (m) => !(m.role === "user" && m.speakerKind === "ingress"),
    );
  }

  const outcomeRow = retainedLocal.find((m) => m.id.includes("publish-trace-outcome"));
  if (outcomeRow && provenanceHasPublishOutcomeEquivalent(provenance, outcomeRow)) {
    retainedLocal = retainedLocal.filter((m) => m !== outcomeRow);
  }

  if (retainedLocal.length === 0) return provenance;
  if (provenance.length === 0) return retainedLocal;

  const localIds = new Set(retainedLocal.map((m) => m.id));
  const tail = provenance.filter((m) => !localIds.has(m.id));
  return [...retainedLocal, ...tail];
}

function provenanceHasPublishOutcomeEquivalent(
  provenance: ChatMessage[],
  localOutcome: ChatMessage,
): boolean {
  const failureLines = parsePublishFailureLines(localOutcome.text ?? "");
  if (failureLines.length === 0) return false;
  const operational = provenance.flatMap((m) =>
    (m.contentBlocks ?? []).filter(
      (b): b is import("../types/a2a").OperationalContentBlock =>
        b.type === "operational" &&
        (b.kind === "dispatch_rejected" || b.kind === "dispatch_transport_error"),
    ),
  );
  if (operational.length === 0) return false;
  return failureLines.every((failure) =>
    operational.some(
      (op) =>
        op.agentPackage === failure.agentPackage &&
        op.agentInstanceId === failure.agentInstanceId &&
        (op.detail?.includes(failure.detail) ?? op.summary.includes(failure.detail)),
    ),
  );
}

function parsePublishFailureLines(text: string): Array<{
  agentPackage: string;
  agentInstanceId: string;
  detail: string;
}> {
  const lines = text.split("\n");
  const failures: Array<{ agentPackage: string; agentInstanceId: string; detail: string }> = [];
  let inFailures = false;
  for (const line of lines) {
    if (line.trim() === "Failures:") {
      inFailures = true;
      continue;
    }
    if (!inFailures) continue;
    const match = line.match(/^- ([^/]+)\/([^:]+):\s*(.+)$/);
    if (!match) continue;
    failures.push({
      agentPackage: match[1]!,
      agentInstanceId: match[2]!,
      detail: match[3]!.trim(),
    });
  }
  return failures;
}

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

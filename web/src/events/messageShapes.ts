/** Message-shape registry helpers for the Event Console. */

import type { EventSubscriptionInfo } from "../types/a2a";
import type {
  AgentDeliverableMessageShape,
  DerivedDispatchEnvelope,
  EventConsoleSelection,
  MessageShapeSample,
} from "../types/events";

function cloneJson<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

export function subscriptionMatchesShape(
  sub: EventSubscriptionInfo,
  shape: AgentDeliverableMessageShape,
): boolean {
  const versions = sub.schema_versions ?? [];
  if (versions.length > 0 && !versions.includes(shape.wire_schema_version)) {
    return false;
  }
  const kinds = sub.source_kinds ?? [];
  if (kinds.length > 0 && !kinds.includes(shape.source_kind)) {
    return false;
  }
  return true;
}

export function messageShapesForAgent(
  shapes: AgentDeliverableMessageShape[],
  subscriptions: EventSubscriptionInfo[],
): AgentDeliverableMessageShape[] {
  if (subscriptions.length === 0) return [];
  const matched = new Map<string, AgentDeliverableMessageShape>();
  for (const sub of subscriptions) {
    for (const shape of shapes) {
      if (subscriptionMatchesShape(sub, shape)) {
        matched.set(shape.message_shape_id, shape);
      }
    }
  }
  return [...matched.values()].sort((a, b) => a.display_name.localeCompare(b.display_name));
}

/** Message types deliverable through one subscription row (plan: agent → subscription → message type). */
export function messageShapesForSubscription(
  shapes: AgentDeliverableMessageShape[],
  subscriptions: EventSubscriptionInfo[],
  subscriptionIndex: number,
): AgentDeliverableMessageShape[] {
  const sub = subscriptions[subscriptionIndex];
  if (!sub) return [];
  return shapes
    .filter((shape) => subscriptionMatchesShape(sub, shape))
    .sort((a, b) => a.display_name.localeCompare(b.display_name));
}

export function firstDeliverableShape(
  shapes: AgentDeliverableMessageShape[],
  subscriptions: EventSubscriptionInfo[],
  subscriptionIndex = 0,
): AgentDeliverableMessageShape | undefined {
  return messageShapesForSubscription(shapes, subscriptions, subscriptionIndex)[0];
}

export function findMessageShape(
  shapes: AgentDeliverableMessageShape[],
  messageShapeId: string,
): AgentDeliverableMessageShape | undefined {
  return shapes.find((s) => s.message_shape_id === messageShapeId);
}

export function findSample(
  shape: AgentDeliverableMessageShape,
  sampleId?: string,
): MessageShapeSample | undefined {
  if (!sampleId) return shape.samples[0];
  return shape.samples.find((s) => s.sample_id === sampleId) ?? shape.samples[0];
}

export function deriveDispatchEnvelope(
  shape: AgentDeliverableMessageShape,
  sample?: MessageShapeSample,
): DerivedDispatchEnvelope {
  return {
    routingKey: shape.delivery_defaults.routing_key,
    messageType: shape.wire_schema_version,
    sourceKind: shape.source_kind,
    sourceKey: sample?.source_key ?? "",
  };
}

export function autofillPayload(
  shape: AgentDeliverableMessageShape,
  envelope: DerivedDispatchEnvelope,
  payload: Record<string, unknown>,
): Record<string, unknown> {
  const next = { ...payload };
  const schema = shape.payload_schema as {
    properties?: Record<string, { const?: unknown; properties?: Record<string, unknown> }>;
  };
  const rootSchemaVersion = schema.properties?.schema_version?.const;
  if (rootSchemaVersion !== undefined) {
    next.schema_version = rootSchemaVersion;
  }
  const source = next.source;
  if (source && typeof source === "object" && !Array.isArray(source)) {
    const src = { ...(source as Record<string, unknown>) };
    const sourceSchema = schema.properties?.source;
    if (sourceSchema?.properties) {
      if (
        "source_kind" in sourceSchema.properties &&
        (src.source_kind === "" || src.source_kind === undefined)
      ) {
        src.source_kind = envelope.sourceKind;
      }
      if ("source" in sourceSchema.properties && (src.source === "" || src.source === undefined)) {
        src.source = envelope.sourceKind;
      }
      if (
        "source_key" in sourceSchema.properties &&
        envelope.sourceKey &&
        (src.source_key === "" || src.source_key === undefined)
      ) {
        src.source_key = envelope.sourceKey;
      }
    }
    next.source = src;
  }
  return next;
}

export function payloadFromShapeSelection(
  shape: AgentDeliverableMessageShape,
  sampleId?: string,
): { payload: Record<string, unknown>; sample?: MessageShapeSample; envelope: DerivedDispatchEnvelope } {
  const sample = findSample(shape, sampleId);
  const envelope = deriveDispatchEnvelope(shape, sample);
  const base = cloneJson(sample?.payload ?? {}) as Record<string, unknown>;
  const payload = autofillPayload(shape, envelope, base);
  return { payload, sample, envelope };
}

export function resolveShapeFromHistory(
  shapes: AgentDeliverableMessageShape[],
  schemaVersion: string,
  sourceKind?: string,
): AgentDeliverableMessageShape | undefined {
  return shapes.find(
    (s) =>
      s.wire_schema_version === schemaVersion &&
      (sourceKind === undefined || s.source_kind === sourceKind),
  );
}

export function selectionFromFlow(
  summary: {
    target: { agent_package: string; agent_instance_id: string };
    message_shape_id?: string;
    schema_version: string;
    source_kind?: string;
  },
  shapes: AgentDeliverableMessageShape[],
  subscriptions: EventSubscriptionInfo[],
): EventConsoleSelection | null {
  const shape =
    (summary.message_shape_id
      ? findMessageShape(shapes, summary.message_shape_id)
      : undefined) ??
    resolveShapeFromHistory(shapes, summary.schema_version, summary.source_kind);
  if (!shape) return null;
  const subIdx = subscriptions.findIndex((sub) => subscriptionMatchesShape(sub, shape));
  return {
    agentPackage: summary.target.agent_package,
    agentInstanceId: summary.target.agent_instance_id,
    subscriptionIndex: subIdx >= 0 ? subIdx : 0,
    messageShapeId: shape.message_shape_id,
    sampleId: shape.samples[0]?.sample_id,
  };
}

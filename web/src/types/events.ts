/** Event Console API types (mirror baml-rt-api event_console DTOs). */

export type EventDispatchScope =
  | { kind: "new_context" }
  | { kind: "existing_context"; context_id: string }
  | { kind: "existing_task"; context_id: string; task_id: string };

export type EventDispatchScopeKind = EventDispatchScope["kind"];

/** One message object in an event publish batch (schema-driven JSON). */
export type DraftPayloadRecord = Record<string, unknown>;

/** How transcript/provenance observation scope is chosen (decoupled from compose draft). */
export type ObservationSource = "picker" | "publish" | "draft";

export interface EventObservationState {
  contextId: string | null;
  source: ObservationSource;
  /** Dispatch-unit task id resolved for provenance/episode scoping (picker deep-links). */
  taskId?: string | null;
}

export interface ResolvedObservationIds {
  contextId: string | null;
  taskId: string | null;
}

export interface EventConsoleRoute {
  agentPackage: string | null;
  agentInstance: string | null;
  contextId: string | null;
}

/** Subset of validate `preview_produced_event` used for scope resolution. */
export interface PreviewProducedEvent {
  context_id?: string;
  task_id?: string;
  message_id?: string;
}

export interface MessageShapeFieldGroup {
  title: string;
  json_pointers: string[];
}

export interface MessageShapeUiHints {
  field_labels?: Record<string, string>;
  field_descriptions?: Record<string, string>;
  field_groups?: MessageShapeFieldGroup[];
  primary_record_array_pointer?: string;
}

export interface MessageShapeSample {
  sample_id: string;
  label: string;
  source_key?: string;
  payload: unknown;
}

export interface MessageShapeDeliveryDefaults {
  routing_key: string;
}

export interface AgentDeliverableMessageShape {
  message_shape_id: string;
  display_name: string;
  description: string;
  origin: string;
  payload_name: string;
  wire_schema_version: string;
  source_kind: string;
  payload_schema: unknown;
  samples: MessageShapeSample[];
  delivery_defaults: MessageShapeDeliveryDefaults;
  ui_hints?: MessageShapeUiHints;
}

export interface EventConsoleSelection {
  agentPackage: string;
  agentInstanceId: string;
  subscriptionIndex: number;
  messageShapeId: string;
  sampleId?: string;
}

export interface DerivedDispatchEnvelope {
  routingKey: string;
  messageType: string;
  sourceKind: string;
  sourceKey: string;
}

export interface EventPayloadDraft {
  agent_package: string;
  agent_instance_id: string;
  messages: DraftPayloadRecord[];
  scope: EventDispatchScope;
  message_id: string;
  metadata: Record<string, unknown>;
}

export interface EventValidationIssue {
  code: string;
  message: string;
  json_pointer?: string;
}

export interface EventValidationReport {
  valid: boolean;
  matched_subscription: boolean;
  errors: EventValidationIssue[];
  warnings: EventValidationIssue[];
  preview_produced_event?: unknown;
}

export interface EventPublishFailure {
  agent_package: string;
  agent_instance_id: string;
  detail: string;
}

export interface EventPublishAcceptance {
  agent_package: string;
  agent_instance_id: string;
  detail: string;
}

export interface EventPublishResponse {
  subscribers_matched: number;
  subscribers_accepted: number;
  acceptances?: EventPublishAcceptance[];
  failures: EventPublishFailure[];
  context_id?: string;
}

export type EventDispatchPhase =
  | "idle"
  | "validating"
  | "publishing"
  | "recording"
  | "live"
  | "empty"
  | "failed";

/** Event Console API types (mirror baml-rt-api event_console DTOs). */

export type EventDispatchScope =
  | { kind: "new_context" }
  | { kind: "existing_context"; context_id: string }
  | { kind: "existing_task"; context_id: string; task_id: string };

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
  messages: unknown[];
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
  preview_request?: unknown;
}

export interface AgentDispatchAck {
  accepted: boolean;
  detail?: string;
  context_id?: string;
  task_id?: string;
  message_id?: string;
}

export type EventDispatchPhase =
  | "idle"
  | "validating"
  | "dispatching"
  | "recording"
  | "live"
  | "empty"
  | "failed";

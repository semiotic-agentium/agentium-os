import { mintTemporalId } from "../utils/temporalId";
import type { AgentDispatchRequestBody, EventSample } from "./sampleCatalog";

/** Origin tag stamped onto every Event Console dispatch's metadata. */
export const EVENT_CONSOLE_ORIGIN = "operator-eval-console";

export const SCOPE_NEW_CONTEXT = "new_context" as const;
export const SCOPE_EXISTING_CONTEXT = "existing_context" as const;

/** Scope an operator can select for a dispatched event. */
export type EventDispatchScope =
  | { kind: typeof SCOPE_NEW_CONTEXT }
  | { kind: typeof SCOPE_EXISTING_CONTEXT; contextId: string };

/** Sentinel substituted into the preview body for fields the host mints at dispatch time. */
export const PREVIEW_MINTED_AT_DISPATCH = "<minted at dispatch>";

export function mintContextId(now?: number): string {
  return mintTemporalId("ctx", now);
}

export function mintMessageId(now?: number): string {
  return mintTemporalId("evt", now);
}

export interface BuildDispatchRequestInput {
  sample: EventSample;
  /** messages parsed from the JSON editor — overrides sample.messages when supplied. */
  messages: unknown[];
  scope: EventDispatchScope;
  /** Optional operator-supplied note merged into metadata for traceability. */
  note?: string;
}

/**
 * Build the exact JSON body the Event Console will POST to /dispatch.
 *
 * Mints context_id + message_id under new-context scope so the resulting
 * provenance is locatable from the Event Console after dispatch ack.
 */
export function buildDispatchRequest(
  input: BuildDispatchRequestInput,
  now: number = Date.now(),
): AgentDispatchRequestBody {
  const { sample, messages, scope, note } = input;

  const metadata: Record<string, unknown> = {
    origin: EVENT_CONSOLE_ORIGIN,
    sample_id: sample.id,
    source_kind: sample.sourceKind,
    dispatched_at: new Date(now).toISOString(),
    ...(sample.extraMetadata ?? {}),
  };
  if (note && note.trim()) {
    metadata.operator_note = note.trim();
  }

  const body: AgentDispatchRequestBody = {
    routing_key: sample.routingKey,
    message_type: sample.messageType,
    messages,
    metadata,
  };

  if (scope.kind === SCOPE_NEW_CONTEXT) {
    body.context_id = mintContextId(now);
    body.message_id = mintMessageId(now);
  } else {
    body.context_id = scope.contextId;
    body.message_id = mintMessageId(now);
  }

  return body;
}

/**
 * Variant of {@link buildDispatchRequest} for the preview pane.
 *
 * Substitutes sentinel placeholders for host-minted fields (context_id /
 * message_id under new-context scope, dispatched_at timestamp) so the preview
 * is stable as the operator types — and so the displayed ids cannot diverge
 * from what `buildDispatchRequest` actually sends at dispatch time.
 */
export function buildDispatchRequestPreview(
  input: BuildDispatchRequestInput,
): AgentDispatchRequestBody {
  const { sample, messages, scope, note } = input;

  const metadata: Record<string, unknown> = {
    origin: EVENT_CONSOLE_ORIGIN,
    sample_id: sample.id,
    source_kind: sample.sourceKind,
    dispatched_at: PREVIEW_MINTED_AT_DISPATCH,
    ...(sample.extraMetadata ?? {}),
  };
  if (note && note.trim()) {
    metadata.operator_note = note.trim();
  }

  const body: AgentDispatchRequestBody = {
    routing_key: sample.routingKey,
    message_type: sample.messageType,
    messages,
    metadata,
    message_id: PREVIEW_MINTED_AT_DISPATCH,
  };

  body.context_id =
    scope.kind === SCOPE_EXISTING_CONTEXT ? scope.contextId : PREVIEW_MINTED_AT_DISPATCH;

  return body;
}

/** Stable pretty-print for the preview pane. */
export function previewJson(value: unknown): string {
  return JSON.stringify(value, null, 2);
}

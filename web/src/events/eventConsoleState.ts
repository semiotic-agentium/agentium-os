/**
 * Event Console working-state types and pure helpers (scope, observation, route).
 */

import type { ConversationHistoryOption } from "../types/a2a";
import type {
  EventDispatchScope,
  EventDispatchScopeKind,
  EventObservationState,
  EventPayloadDraft,
  EventValidationReport,
  PreviewProducedEvent,
  ResolvedObservationIds,
} from "../types/events";
import {
  resolveObservationScope,
  scopeFromRecord,
  type ObservationAgentRef,
  type PublishedScope,
} from "./dispatchObserve";

export const EVENT_DISPATCH_SCOPE_KINDS = [
  "new_context",
  "existing_context",
  "existing_task",
] as const satisfies readonly EventDispatchScopeKind[];

export function isEventDispatchScopeKind(value: string): value is EventDispatchScopeKind {
  return (EVENT_DISPATCH_SCOPE_KINDS as readonly string[]).includes(value);
}

export function createInitialObservation(): EventObservationState {
  return { contextId: null, source: "draft" };
}

export function parsePreviewProducedEvent(
  value: unknown,
): PreviewProducedEvent | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  return value as PreviewProducedEvent;
}

export function observationAgentRefFromDraft(
  draft: Pick<EventPayloadDraft, "agent_package" | "agent_instance_id">,
): ObservationAgentRef | null {
  if (!draft.agent_package || !draft.agent_instance_id) {
    return null;
  }
  return {
    agentPackage: draft.agent_package,
    agentInstanceId: draft.agent_instance_id,
  };
}

export interface ObservationScopeResolveInput {
  lastPublishedScope: PublishedScope | null;
  draftScope: EventDispatchScope;
  observation: EventObservationState;
  previewProducedEvent: PreviewProducedEvent | null;
  currentAgent: ObservationAgentRef | null;
}

export function buildObservationScopeResolveInput(input: {
  lastPublishedScope: PublishedScope | null;
  draft: EventPayloadDraft;
  observation: EventObservationState;
  validation: EventValidationReport | null;
}): ObservationScopeResolveInput {
  return {
    lastPublishedScope: input.lastPublishedScope,
    draftScope: input.draft.scope,
    observation: input.observation,
    previewProducedEvent: parsePreviewProducedEvent(
      input.validation?.preview_produced_event,
    ),
    currentAgent: observationAgentRefFromDraft(input.draft),
  };
}

export function resolveObservedScopeIds(
  input: ObservationScopeResolveInput,
): ResolvedObservationIds {
  const resolved = resolveObservationScope({
    lastPublishedScope: input.lastPublishedScope,
    draftScope: input.draftScope,
    observedContextId: input.observation.contextId,
    previewProducedEvent: input.previewProducedEvent,
    currentAgent: input.currentAgent,
    observationSource: input.observation.source,
  });
  if (!resolved) {
    return { contextId: null, taskId: null };
  }
  return {
    contextId: resolved.contextId,
    taskId: input.observation.taskId ?? resolved.taskId ?? null,
  };
}

export function scopeContextIdFromDraft(scope: EventDispatchScope): string {
  return scope.kind === "new_context" ? "" : scope.context_id;
}

export function publishTargetsNewSession(
  draftScope: EventDispatchScope,
  observedContextId: string | null,
): boolean {
  if (observedContextId === null) return false;
  if (draftScope.kind === "new_context") return true;
  const draftContextId = scopeContextIdFromDraft(draftScope);
  return draftContextId.length > 0 && draftContextId !== observedContextId;
}

export function scopeTaskIdFromDraft(scope: EventDispatchScope): string {
  return scope.kind === "existing_task" ? scope.task_id : "";
}

export function changeDraftScopeKind(
  current: EventDispatchScope,
  kind: EventDispatchScopeKind,
): EventDispatchScope {
  if (kind === "new_context") {
    return { kind: "new_context" };
  }
  if (kind === "existing_context") {
    const contextId =
      current.kind === "existing_context" || current.kind === "existing_task"
        ? current.context_id
        : "";
    return { kind: "existing_context", context_id: contextId };
  }
  const contextId = current.kind !== "new_context" ? current.context_id : "";
  const taskId = current.kind === "existing_task" ? current.task_id : "";
  return { kind: "existing_task", context_id: contextId, task_id: taskId };
}

export function updateDraftScopeContextId(
  scope: EventDispatchScope,
  contextId: string,
): EventDispatchScope {
  if (scope.kind === "existing_task") {
    return { kind: "existing_task", context_id: contextId, task_id: scope.task_id };
  }
  return { kind: "existing_context", context_id: contextId };
}

export function updateDraftScopeTaskId(
  scope: EventDispatchScope,
  taskId: string,
): EventDispatchScope {
  if (scope.kind === "existing_task") {
    return { kind: "existing_task", context_id: scope.context_id, task_id: taskId };
  }
  return scope;
}

export function publishedScopeFromPreview(
  preview: PreviewProducedEvent,
  agentPackage: string,
  agentInstanceId: string,
): PublishedScope | null {
  const base = scopeFromRecord(preview as Record<string, unknown>);
  if (!base) return null;
  return { ...base, agentPackage, agentInstanceId };
}

export function pickerOptionFromContextId(
  contextId: string,
): ConversationHistoryOption {
  return { contextId, latestTimestampMs: 0, preview: "" };
}

export function shouldOfferApplyObservedScope(
  observedContextId: string | null,
  draftScope: EventDispatchScope,
): boolean {
  if (!observedContextId || draftScope.kind !== "new_context") {
    return false;
  }
  return true;
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { computed, toValue, type MaybeRefOrGetter } from "vue";

/** Mirrors backend `ObservationScope` — one scope for transcript, ops, and planning. */
export interface ObservationScope {
  contextId: string;
  taskId?: string | null;
  agentPackage?: string | null;
  afterEventOrder?: number | null;
}

export function observationScopeKey(scope: ObservationScope): string {
  return `${scope.contextId}:${scope.taskId ?? ""}:${scope.agentPackage ?? ""}:${scope.afterEventOrder ?? ""}`;
}

/** Build query params shared by conversation-history and provenance ops routes. */
export function observationScopeQueryParams(scope: ObservationScope): URLSearchParams {
  const params = new URLSearchParams();
  if (scope.taskId) params.set("taskId", scope.taskId);
  if (scope.agentPackage) params.set("agentPackage", scope.agentPackage);
  if (scope.afterEventOrder != null) {
    params.set("afterEventOrder", String(scope.afterEventOrder));
  }
  return params;
}

function scopeField(value: string | null | undefined): string | null {
  return value ?? null;
}

/** Reactive observation scope from refs, getters, or plain values. */
export function useObservationScope(
  contextId: MaybeRefOrGetter<string | null | undefined>,
  taskId?: MaybeRefOrGetter<string | null | undefined>,
  agentPackage?: MaybeRefOrGetter<string | null | undefined>,
  afterEventOrder?: MaybeRefOrGetter<number | null | undefined>,
) {
  return computed((): ObservationScope | null => {
    const ctx = toValue(contextId);
    if (!ctx) return null;
    return {
      contextId: ctx,
      taskId: taskId ? scopeField(toValue(taskId)) : null,
      agentPackage: agentPackage ? scopeField(toValue(agentPackage)) : null,
      afterEventOrder: afterEventOrder ? toValue(afterEventOrder) ?? null : null,
    };
  });
}

/** Map observation scope to provenance ops query params (plus optional agent id). */
export function provenanceQueryFromScope(
  scope: ObservationScope | null,
  agentId?: string,
): Pick<
  import("../types/provenance").ProvenanceQueryParams,
  "contextId" | "taskId" | "agentId" | "agentPackage"
> {
  return {
    contextId: scope?.contextId,
    taskId: scope?.taskId ?? undefined,
    agentId,
    agentPackage: scope?.agentPackage ?? undefined,
  };
}

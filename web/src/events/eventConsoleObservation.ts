// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Stable watch key for context + task observation scope. */
export function buildObserveScopeWatchKey(
  contextId: string | null | undefined,
  taskId: string | null | undefined,
  agentPackage?: string | null | undefined,
): string {
  return `${contextId ?? ""}:${taskId ?? ""}:${agentPackage ?? ""}`;
}

/** Dedupe key for in-flight observation refresh (includes preserve mode). */
export function buildObservationRefreshLoadKey(
  contextId: string,
  taskId: string | null | undefined,
  preserve: boolean,
): string {
  const resolvedTask = taskId?.trim() ?? "";
  return `${contextId}:${resolvedTask}:${preserve ? "preserve" : "full"}`;
}

/** Scope key without preserve suffix — used to skip redundant full reloads. */
export function buildObservationScopeKey(
  contextId: string,
  taskId: string | null | undefined,
  agentPackage?: string | null | undefined,
): string {
  const resolvedTask = taskId?.trim() ?? "";
  const pkg = agentPackage?.trim() ?? "";
  return `${contextId}:${resolvedTask}:${pkg}`;
}

/** Whether a scope transition should preserve the local transcript overlay. */
export function shouldPreserveTranscriptOnScopeChange(
  prevScopeKey: string | undefined,
  nextScopeKey: string,
  publishActive: boolean,
): boolean {
  if (!publishActive) return false;
  if (!prevScopeKey || prevScopeKey === ":") return false;
  const prevContext = prevScopeKey.split(":")[0] ?? "";
  return prevContext.length > 0 && nextScopeKey.startsWith(`${prevContext}:`);
}

export function shouldSkipObservationRefresh(
  scopeKey: string,
  lastScopeKey: string,
  transcriptLength: number,
  preserve: boolean,
): boolean {
  return scopeKey === lastScopeKey && transcriptLength > 0 && !preserve;
}

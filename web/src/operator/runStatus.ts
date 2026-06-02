// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/** Unified operator run status for Chat and Event Console. */

import type {
  ChatMessage,
  HistoryHydrateState,
  WorkflowPhaseName,
  WorkflowProgressState,
} from "../types/a2a";
import type { EventDispatchPhase, EventPublishResponse } from "../types/events";
import type { TraceObserveState } from "../composables/useEventObservation";
import { isEventDispatchInFlight } from "../events/dispatchPhases";
import {
  formatPublishAcceptanceSummary,
  publishHadNoEffectiveWork,
} from "../events/publishOutcome";
import { hasOpenToolSession, worstUnitProgress } from "./runProgressParse";

export type OperatorRunPhase =
  | "idle"
  | "preparing"
  | "publishing"
  | "recording"
  | "executing"
  | "waiting"
  | "complete"
  | "partial"
  | "failed";

export type OperatorRunSeverity = "neutral" | "progress" | "success" | "warning" | "error";

export interface OperatorRunStatusStep {
  key: string;
  label: string;
  state: "done" | "active" | "pending";
}

export interface OperatorRunStatus {
  phase: OperatorRunPhase;
  label: string;
  detail?: string;
  severity: OperatorRunSeverity;
  active: boolean;
  steps?: OperatorRunStatusStep[];
  progress?: { done: number; total: number; noun: string };
}

export const IDLE_RUN_STATUS: OperatorRunStatus = {
  phase: "idle",
  label: "Ready",
  severity: "neutral",
  active: false,
};

export interface DeriveEventRunStatusInput {
  dispatchPhase: EventDispatchPhase;
  hydrateState: TraceObserveState;
  lastPublishOutcome: EventPublishResponse | null;
  publishError: string | null;
  waitingForIngress: boolean;
  transcriptMessages: ChatMessage[];
  contextId: string | null;
  /** Deep-link / picker observe — do not treat stale tool rows as live execution. */
  observeOnly?: boolean;
}

export interface DeriveChatRunStatusInput {
  isLoading: boolean;
  awaitingInput: boolean;
  hydrateState: HistoryHydrateState;
  workflowProgress: WorkflowProgressState;
  messages: ChatMessage[];
  contextId: string | null | undefined;
}

function hasSubscriberFailure(outcome: EventPublishResponse | null): boolean {
  if (!outcome) return false;
  return outcome.failures.length > 0 || outcome.subscribers_accepted < outcome.subscribers_matched;
}

function isHydrateBusy(hydrateState: TraceObserveState | HistoryHydrateState): boolean {
  return hydrateState === "loading" || hydrateState === "waiting";
}

function executionIncomplete(
  unitProgress: ReturnType<typeof worstUnitProgress>,
  messages: ChatMessage[],
): boolean {
  if (hasOpenToolSession(messages)) return true;
  if (unitProgress && unitProgress.done < unitProgress.total) return true;
  return false;
}

export function deriveEventRunStatus(input: DeriveEventRunStatusInput): OperatorRunStatus {
  const {
    dispatchPhase,
    hydrateState,
    lastPublishOutcome,
    publishError,
    waitingForIngress,
    transcriptMessages,
    contextId,
    observeOnly = false,
  } = input;

  if (publishError) {
    return {
      phase: "failed",
      label: "Publish failed",
      detail: publishError,
      severity: "error",
      active: false,
    };
  }

  if (dispatchPhase === "validating") {
    return {
      phase: "preparing",
      label: "Validating draft…",
      severity: "progress",
      active: true,
    };
  }

  if (dispatchPhase === "publishing") {
    return {
      phase: "publishing",
      label: "Publishing to subscribers…",
      severity: "progress",
      active: true,
    };
  }

  if (dispatchPhase === "failed" || hasSubscriberFailure(lastPublishOutcome)) {
    const o = lastPublishOutcome;
    const detail = o
      ? `${o.subscribers_accepted} of ${o.subscribers_matched} subscriber(s) accepted`
      : undefined;
    return {
      phase: "failed",
      label: "Publish failed",
      detail,
      severity: "error",
      active: false,
    };
  }

  if (waitingForIngress) {
    return {
      phase: "recording",
      label: "Waiting for ingress",
      detail: "Host ingress has not appeared in the transcript yet.",
      severity: "progress",
      active: true,
    };
  }

  if (
    dispatchPhase === "recording" ||
    isHydrateBusy(hydrateState) ||
    isEventDispatchInFlight(dispatchPhase)
  ) {
    return {
      phase: "recording",
      label: "Recording provenance…",
      severity: "progress",
      active: true,
    };
  }

  const unitProgress = worstUnitProgress(lastPublishOutcome?.acceptances);
  if (
    !observeOnly &&
    executionIncomplete(unitProgress, transcriptMessages)
  ) {
    const progress = unitProgress
      ? { done: unitProgress.done, total: unitProgress.total, noun: "units" as const }
      : undefined;
    const detailParts: string[] = [];
    if (unitProgress) {
      detailParts.push(
        `${unitProgress.done}/${unitProgress.total} units · ${unitProgress.agentLabel}`,
      );
    } else if (hasOpenToolSession(transcriptMessages)) {
      detailParts.push("Tool session in progress");
    }
    return {
      phase: "executing",
      label: "Executing",
      detail: detailParts.length > 0 ? detailParts.join(" · ") : undefined,
      severity: "progress",
      active: true,
      progress,
    };
  }

  if (lastPublishOutcome && publishHadNoEffectiveWork(lastPublishOutcome)) {
    return {
      phase: "partial",
      label: "No effective work",
      detail: formatPublishAcceptanceSummary(lastPublishOutcome),
      severity: "warning",
      active: false,
    };
  }

  if (lastPublishOutcome && dispatchPhase === "live" && hydrateState === "ready") {
    return {
      phase: "complete",
      label: "Complete",
      detail: formatPublishAcceptanceSummary(lastPublishOutcome),
      severity: "success",
      active: false,
    };
  }

  if (contextId) {
    return {
      phase: "idle",
      label: "Observing",
      severity: "neutral",
      active: false,
    };
  }

  return IDLE_RUN_STATUS;
}

const WORKFLOW_PHASES: { key: WorkflowPhaseName; label: string }[] = [
  { key: "discovery", label: "Discovery" },
  { key: "planning", label: "Planning" },
  { key: "execution", label: "Execution" },
  { key: "synthesis", label: "Synthesis" },
];

const WORKFLOW_PHASE_ORDER: Record<WorkflowPhaseName, number> = {
  idle: -1,
  discovery: 0,
  planning: 1,
  execution: 2,
  synthesis: 3,
};

function workflowStepState(
  key: WorkflowPhaseName,
  current: WorkflowPhaseName,
): "done" | "active" | "pending" {
  const currentOrd = WORKFLOW_PHASE_ORDER[current] ?? -1;
  const targetOrd = WORKFLOW_PHASE_ORDER[key] ?? -1;
  if (targetOrd < currentOrd) return "done";
  if (targetOrd === currentOrd) return "active";
  return "pending";
}

function buildWorkflowSteps(progress: WorkflowProgressState): OperatorRunStatusStep[] {
  const current = progress.phase;
  return WORKFLOW_PHASES.map(({ key, label }) => ({
    key,
    label,
    state: workflowStepState(key, current),
  }));
}

function workflowActiveLabel(progress: WorkflowProgressState): string {
  const match = WORKFLOW_PHASES.find((p) => p.key === progress.phase);
  let label = match?.label ?? "Working";
  if (progress.phase === "planning" && progress.iteration && progress.iteration > 1) {
    label = `${label} · iter ${progress.iteration}`;
  }
  return label;
}

function workflowExecutionDetail(progress: WorkflowProgressState): string | undefined {
  if (progress.phase !== "execution" || progress.nodes.length === 0) return undefined;
  const running = progress.nodes.find((n) => n.status === "running");
  if (running) return `Execution · ${running.name} active`;
  const pending = progress.nodes.find((n) => n.status === "pending");
  if (pending) return `Execution · ${pending.name} pending`;
  return undefined;
}

export function deriveChatRunStatus(input: DeriveChatRunStatusInput): OperatorRunStatus {
  const { isLoading, awaitingInput, hydrateState, workflowProgress, messages, contextId } = input;

  if (awaitingInput) {
    return {
      phase: "waiting",
      label: "Awaiting your reply",
      severity: "progress",
      active: true,
    };
  }

  const coordinatorActive =
    workflowProgress.pipelineActive === true && workflowProgress.phase !== "idle";

  if (coordinatorActive) {
    const steps = buildWorkflowSteps(workflowProgress);
    const label = workflowActiveLabel(workflowProgress);
    const detail = workflowExecutionDetail(workflowProgress);
    const phase: OperatorRunPhase =
      workflowProgress.phase === "discovery" || workflowProgress.phase === "planning"
        ? "preparing"
        : "executing";
    return {
      phase,
      label,
      detail,
      severity: "progress",
      active: true,
      steps,
    };
  }

  if (isLoading) {
    return {
      phase: "executing",
      label: "Agent responding…",
      severity: "progress",
      active: true,
    };
  }

  if (isHydrateBusy(hydrateState)) {
    return {
      phase: "recording",
      label: "Recording provenance…",
      severity: "progress",
      active: true,
    };
  }

  if (hasOpenToolSession(messages)) {
    return {
      phase: "executing",
      label: "Executing",
      detail: "Tool session in progress",
      severity: "progress",
      active: true,
    };
  }

  if (contextId && messages.length > 0) {
    return {
      phase: "complete",
      label: "Complete",
      severity: "success",
      active: false,
    };
  }

  if (contextId) {
    return {
      phase: "idle",
      label: "Observing",
      severity: "neutral",
      active: false,
    };
  }

  return IDLE_RUN_STATUS;
}

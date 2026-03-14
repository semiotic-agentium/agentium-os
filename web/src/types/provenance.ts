export type ProvenanceResource = "llm_calls" | "tool_calls" | "messages";

export type ProvenanceOutcome = "failed_only" | "successful_only" | "both";

export type ProvenanceSortDir = "asc" | "desc";

export interface ProvenanceQueryParams {
  contextId?: string;
  taskId?: string;
  agentId?: string;
  provider?: string;
  model?: string;
  toolName?: string;
  bamlPrompt?: string;
  fromTimestampMs?: number;
  toTimestampMs?: number;
  groupBy?: string[];
  sortBy?: string;
  sortDir?: ProvenanceSortDir;
  pageSize?: number;
  cursor?: string;
  topK?: number;
  outcome?: ProvenanceOutcome;
  responseProfile?: "ui_full" | "tool_compact";
}

export interface ProvenanceSummary {
  count: number;
  failedCount: number;
  durationMsTotal: number;
  totalTokens: number;
  promptTokensTotal?: number;
  completionTokensTotal?: number;
  cachedInputTokensTotal?: number;
  latencyHotspots?: {
    p95: number;
    p99: number;
  };
  tokenHotspots?: {
    p95: number;
    p99: number;
  };
}

export interface ProvenanceGroupHotspot {
  groupKey: string;
  groupValues?: Array<string | null>;
  groupDimensions?: string[];
  count: number;
  failed: number;
  failureRate: number;
  avgDurationMs: number;
  avgTotalTokens: number;
}

export interface ProvenanceRowBase {
  activity_id?: string;
  activity_kind?: string;
  timestamp_ms?: number;
  [key: string]: unknown;
}

export interface ProvenanceQueryResponse {
  resource: ProvenanceResource;
  rows: ProvenanceRowBase[];
  summary: ProvenanceSummary;
  hotspotGroups: ProvenanceGroupHotspot[];
  nextCursor?: string;
  truncated: boolean;
  appliedCaps: Record<string, unknown>;
}

export interface PlanningStepView {
  step_id: string;
  description: string;
  order: number;
  depends_on: string[];
  status: string;
}

export interface PlanningIntentView {
  task_id: string;
  intent_id: string;
  description: string;
}

export interface PlanningPlanView {
  task_id: string;
  intent_id: string;
  plan_id: string;
  steps: PlanningStepView[];
}

export interface PlanningStepSummary {
  total: number;
  completed: number;
  failed: number;
  inProgress: number;
  pending: number;
}

export interface ContextPlanningTaskSnapshot {
  taskId: string;
  currentIntent: PlanningIntentView | null;
  currentPlan: PlanningPlanView | null;
  stepSummary: PlanningStepSummary;
}

export interface ContextPlanningResponse {
  contextId: string;
  tasks: ContextPlanningTaskSnapshot[];
}

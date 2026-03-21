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

export type DriftSeverity = "acceptable" | "warn" | "block";

/** One resolved citation: the raw ref the LLM emitted plus the actual evidence text. */
export interface CitationDetail {
  /** Exact string the LLM emitted, e.g. `"#1"`, `"@2:3-5"`, `"!@1"`. */
  raw: string;
  n: number;
  /** `true` = history ref (`#N`), `false` = archive ref (`@N`). */
  isHistory: boolean;
  /** Counter-evidence (`!` prefix) — LLM marked this as contradicting evidence. */
  negated: boolean;
  /** Cosine similarity between the decision text and this citation's content. */
  similarity: number;
  /** Stable provenance event ID for graph lookup. */
  activityAnchor: string;
  /** The actual evidence text the LLM was citing (up to 400 chars). */
  contentPreview: string;
}

/** Per-citation similarity entry on each row's `drift.citation.perCitation` array. */
export interface CitationSimilarityOnRow {
  n: number;
  isHistory: boolean;
  negated: boolean;
  similarity: number;
  raw?: string;
  activityAnchor?: string;
  contentPreview?: string;
}

/** Citation-grounded drift block on an LLM call row. */
export interface CitationDriftOnRow {
  perCitation: CitationSimilarityOnRow[];
  meanSimilarity: number;
  coverage: number;
  totalDecisions: number;
  citedDecisions: number;
}

export interface DriftedCallDetail {
  functionName: string;
  severity: string;
  intentAlignment: number;
  stepAlignment: number | null;
  crossEncoderStepScore: number | null;
  intentTextPreview: string;
  responseTextPreview: string;
  stepTextPreview?: string;
  /** Resolved citations with full evidence text. Empty when no citations were scored. */
  citations?: CitationDetail[];
}

export interface TaskPlanDriftSummary {
  compositeSeverity: DriftSeverity | null;
  intentAlignment: number | null;
  stepAlignment: number | null;
  crossEncoderStepScore: number | null;
  trajectoryDrift: number | null;
  planAdherenceScore: number | null;
  scoredCallCount: number;
  warnCount: number;
  blockCount: number;
  driftedCalls?: DriftedCallDetail[];
}

export interface PlanDriftOnRow {
  intentAlignment?: number;
  stepAlignment?: number;
  /** Cross-encoder logit for (step_description, response). Always present when stepAlignment present. */
  crossEncoderStepScore?: number;
  trajectoryDrift?: number;
  planAdherenceScore?: number;
  compositeSeverity?: DriftSeverity;
}

export interface DriftOnRow {
  score?: number;
  severity?: string;
  mode?: string;
  warnMinScore?: number;
  blockMinScore?: number;
  intentTextPreview?: string;
  responseTextPreview?: string;
  plan?: PlanDriftOnRow;
  /** Citation-grounded drift block. Present when the LLM emitted citations and scoring succeeded. */
  citation?: CitationDriftOnRow;
}

export interface ContextPlanningTaskSnapshot {
  taskId: string;
  currentIntent: PlanningIntentView | null;
  currentPlan: PlanningPlanView | null;
  stepSummary: PlanningStepSummary;
  drift?: TaskPlanDriftSummary;
}

export interface ContextPlanningResponse {
  contextId: string;
  tasks: ContextPlanningTaskSnapshot[];
}

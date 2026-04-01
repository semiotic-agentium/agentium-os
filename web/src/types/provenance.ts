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
  /** Distinct task ids from provenance messages (first-seen order). Episode/transcript discovery. */
  allTaskIds?: string[];
  tasks: ContextPlanningTaskSnapshot[];
}

// Episode streaming types
export type EpisodeTerminalStatus =
  | "completed"
  | "failed"
  | "canceled"
  | "rejected"
  | string; // Other(string)

export interface EpisodeDuration {
  active_ms: number;
  wait_ms: number;
  wall_clock_ms: number;
}

export interface EpisodeTokenSummary {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  llm_call_count: number;
  llm_duration_ms: number;
}

export type EpisodeStepType =
  | "message"
  | "tool_call"
  | "tool_read"
  | "tool_result"
  | "plan_revision"
  | "status_transition"
  | "artifact_emitted";

export type EpisodeContent =
  | { type: "text"; text: string }
  | { type: "tool_invocation"; tool_name: string; description: string }
  | { type: "tool_output"; tool_name: string; summary: string; line_count: number; byte_count: number; lines: string[] }
  | { type: "plan_revision_ref"; summary: string }
  | { type: "status_change"; old: string; new: string; message?: string }
  | { type: "artifact"; name: string; media_type?: string; size_bytes?: number };

export interface EpisodeTranscriptEntry {
  seq: number;
  step_type: EpisodeStepType;
  role: string;
  elapsed_ms: number;
  content: EpisodeContent;
  activity_anchor: string;
  citation_strings?: string[];
}

/** BAML `conversation_history` mirror: role + projection-shaped content (episode-prefixed refs). */
export interface EpisodeSessionHistoryLine {
  role: string;
  content: string;
}

export interface EpisodeIntentRevision {
  intent_id: string;
  description: string;
  activity_anchor: string;
  timestamp_ms: number;
  superseded_by_next: boolean;
  supersession_from_previous?: string;
  derived_citation_strings: string[];
}

export interface EpisodePlanStepEntry {
  step_id: string;
  description: string;
  status: string;
  timestamp_ms?: number;
  citation_strings: string[];
}

export interface EpisodePlanRevision {
  plan_id: string;
  intent_id: string;
  activity_anchor: string;
  timestamp_ms: number;
  superseded_by_next: boolean;
  steps: EpisodePlanStepEntry[];
}

export interface EpisodeArtifactSummary {
  name: string;
  media_type?: string;
}

export interface EpisodeOutcome {
  final_message?: string;
  artifacts: EpisodeArtifactSummary[];
  citation_strings: string[];
  token_summary: EpisodeTokenSummary;
  duration: EpisodeDuration;
}

export interface EpisodeDriftSummary {
  composite_severity: string;
  intent_alignment: number;
  step_alignment?: number;
  trajectory_drift?: number;
  plan_adherence_score: number;
  scored_call_count: number;
  warn_count: number;
  block_count: number;
}

export interface EpisodeDriftCall {
  activity_anchor: string;
  function_name: string;
  severity: string;
  intent_alignment: number;
  step_alignment?: number;
  cross_encoder_step_score?: number;
  trajectory_drift?: number;
  plan_adherence_score: number;
  citation_mean_similarity?: number;
  citation_coverage?: number;
  citation_strings?: string[];
}

export interface EpisodeSnapshot {
  task_id: string;
  context_id: string;
  agent_id: string;
  ref_prefix: string;
  status: EpisodeTerminalStatus;
  started_timestamp_ms: number;
  duration: EpisodeDuration;
  token_summary: EpisodeTokenSummary;
  prior_context: EpisodeTranscriptEntry[];
  goal: EpisodeTranscriptEntry;
  transcript: EpisodeTranscriptEntry[];
  session_history: EpisodeSessionHistoryLine[];
  intents: EpisodeIntentRevision[];
  plans: EpisodePlanRevision[];
  outcome: EpisodeOutcome;
  drift_summary?: EpisodeDriftSummary;
  drift_calls?: EpisodeDriftCall[];
}

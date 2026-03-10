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

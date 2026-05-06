/**
 * Dashboard UX/IA — widget inventory (legacy → narrative mapping):
 * - Active Agents hero → SYSTEM SURFACE (inventory table + capacity).
 * - Last Sync hero → REMOVED (was page-load timestamp, misleading).
 * - Prompt JSON / Token Usage heroes → DEMOTED into focused lane detail + causal metrics strip (no duplicate hero grid).
 * - Provenance Success hero → ATTENTION + compact hero “Prov. health” (real ops aggregate).
 * - Configuration hero → SYSTEM SURFACE (action row).
 * - Agent Plan hero → RUNTIME NOW optional chip when `/planning` returns tasks (provenance-backed planning only).
 * - Agent Inventory table → SYSTEM SURFACE (keep, lower priority).
 * - Session Metrics block → CAUSAL STORY supplement (sparkline + density), not parallel hero cards.
 * - Latest Interpretation → REMOVED (coordinator-era; narrative comes from transcript).
 * - Last Trace miniature → CAUSAL drilldown (“Open traces”) → chat + provenance Live/Explore.
 */

import type { AgentDiscoveryEntry, ChatMessage, ContextMetricsResponse } from "../types/a2a";
import type { ProvenanceGroupHotspot } from "../types/provenance";

export type ProvenancePaneTab = "live" | "failures" | "anomalies" | "drift" | "explore";

export interface DashboardLaneSnapshot {
  tabId: string;
  title: string;
  contextId?: string;
  agent: AgentDiscoveryEntry | null;
  isStreaming: boolean;
  isActive: boolean;
}

export interface RuntimeLane {
  tabId: string;
  title: string;
  contextId?: string;
  agentLabel: string;
  status: "idle" | "active" | "streaming";
  detail: string;
  isFocused: boolean;
}

export interface AttentionItem {
  severity: "critical" | "warning" | "info";
  title: string;
  detail?: string;
  provenanceTab?: ProvenancePaneTab;
  actionLabel: string;
}

export interface CausalStoryLine {
  role: string;
  text: string;
}

export interface SystemCapacityRow {
  id: string;
  name: string;
  description: string | null;
  version: string;
  tools: string[];
  capabilities: string[];
  discovered: boolean;
}

export interface DashboardViewModel {
  lanes: RuntimeLane[];
  otherLaneCount: number;
  attention: AttentionItem[];
  causalLines: CausalStoryLine[];
  /** Focused session: compact operational metrics (not hero duplicates). */
  sessionStrip: {
    tokensTotal: number | null;
    llmCalls: number | null;
    avgLatencyMs: number | null;
    promptKb: string | null;
    turns: number | null;
  } | null;
  hotspots: ProvenanceGroupHotspot[];
  hero: {
    openLanes: number;
    provenanceHealthPct: number | null;
    provenanceOpsTotal: number;
    provenanceFailed: number;
    /** ms epoch from provenance query freshness */
    lastProvenanceUpdateMs: number | null;
  };
  planningChip: {
    completed: number;
    total: number;
    driftSeverity: string | null;
    intent: string | null;
  } | null;
}

function laneFromSnapshot(s: DashboardLaneSnapshot): RuntimeLane {
  const agentLabel = s.agent
    ? `${s.agent.agent_package}/${s.agent.agent_instance_id}`
    : "No agent bound";
  let status: RuntimeLane["status"] = "idle";
  if (s.isActive && s.isStreaming) status = "streaming";
  else if (s.contextId) status = "active";

  const ctx = s.contextId ? `ctx ${s.contextId.slice(0, 8)}…` : "No context yet";
  const detail = `${agentLabel} · ${ctx}`;

  return {
    tabId: s.tabId,
    title: s.title,
    contextId: s.contextId,
    agentLabel,
    status,
    detail,
    isFocused: s.isActive,
  };
}

function truncateOneLine(text: string, max = 140): string {
  const t = text.replace(/\s+/g, " ").trim();
  if (t.length <= max) return t;
  return `${t.slice(0, max - 1)}…`;
}

export function extractCausalLines(messages: ChatMessage[], max = 8): CausalStoryLine[] {
  const out: CausalStoryLine[] = [];
  for (let i = messages.length - 1; i >= 0 && out.length < max; i--) {
    const m = messages[i]!;
    const role = m.role;
    const text = truncateOneLine(m.text ?? "");
    if (!text) continue;
    out.push({ role, text });
  }
  return out.reverse();
}

export interface BuildDashboardViewModelInput {
  laneSnapshots: DashboardLaneSnapshot[];
  contextMetrics: ContextMetricsResponse | null;
  promptContextBytesSessionCurrent?: number | null;
  provenanceSummary?: {
    count: number;
    failedCount: number;
    durationMsTotal: number;
    totalTokens: number;
    llmCount: number;
    toolCount: number;
    hotspotGroups: ProvenanceGroupHotspot[];
    lastUpdatedAt: number;
  } | null;
  messages: ChatMessage[];
  planningChip: DashboardViewModel["planningChip"];
  runnerOnline: boolean;
}

export function buildDashboardViewModel(input: BuildDashboardViewModelInput): DashboardViewModel {
  const lanes = input.laneSnapshots.map(laneFromSnapshot);
  const focused = lanes.find((l) => l.isFocused);
  const otherLaneCount = Math.max(0, lanes.filter((l) => !l.isFocused && (l.status === "active" || l.status === "streaming")).length);

  const total = input.provenanceSummary?.count ?? 0;
  const failed = input.provenanceSummary?.failedCount ?? 0;
  const provenanceHealthPct =
    total > 0 ? Math.round((Math.max(0, total - failed) / total) * 100) : null;

  const attention: AttentionItem[] = [];

  if (!input.runnerOnline) {
    attention.push({
      severity: "critical",
      title: "Runner unreachable",
      detail: "GET /agents failed — verify the runner process and network.",
      actionLabel: "Retry from Chat",
    });
  }

  if (failed > 0) {
    attention.push({
      severity: "critical",
      title: `${failed} failed provenance operation${failed === 1 ? "" : "s"}`,
      detail: "Inspect LLM/tool failures for this context.",
      provenanceTab: "failures",
      actionLabel: "Open Failures",
    });
  }

  const drift = input.planningChip?.driftSeverity?.toLowerCase() ?? "";
  if (drift && drift !== "none" && drift !== "low") {
    attention.push({
      severity: drift.includes("high") || drift.includes("block") ? "critical" : "warning",
      title: "Planning drift signal",
      detail: input.planningChip?.intent ?? "See drift tab for call-level evidence.",
      provenanceTab: "drift",
      actionLabel: "Open Drift",
    });
  }

  const topHotspot = input.provenanceSummary?.hotspotGroups?.[0];
  if (topHotspot && topHotspot.avgDurationMs > 15_000) {
    const gk = topHotspot.groupKey;
    attention.push({
      severity: "info",
      title: "Latency hotspot",
      detail: `${gk.length > 80 ? `${gk.slice(0, 79)}…` : gk} · avg ${Math.round(topHotspot.avgDurationMs)} ms`,
      provenanceTab: "anomalies",
      actionLabel: "Open Anomalies",
    });
  }

  if (!focused?.contextId && input.runnerOnline) {
    attention.push({
      severity: "info",
      title: "No bound context on focused lane",
      detail: "Send a message in Chat to attach context-scoped provenance.",
      provenanceTab: "live",
      actionLabel: "Go to Chat",
    });
  }

  attention.sort((a, b) => {
    const rank = (s: AttentionItem["severity"]) =>
      s === "critical" ? 0 : s === "warning" ? 1 : 2;
    return rank(a.severity) - rank(b.severity);
  });

  const session = input.contextMetrics?.session;
  const sseBytes = input.promptContextBytesSessionCurrent;
  const metricsBytes = session?.prompt_context_bytes_current ?? null;
  const promptBytes = sseBytes ?? metricsBytes;
  const promptKb =
    promptBytes != null ? `${(promptBytes / 1024).toFixed(1)} KB` : null;

  const avgLatencyMs =
    session && session.llm_calls_total > 0
      ? Math.round(session.llm_duration_ms_total / session.llm_calls_total)
      : null;

  const sessionStrip =
    session || promptKb
      ? {
          tokensTotal: session?.tokens_total.total ?? null,
          llmCalls: session?.llm_calls_total ?? null,
          avgLatencyMs,
          promptKb,
          turns: session?.turns_total ?? null,
        }
      : null;

  return {
    lanes,
    otherLaneCount,
    attention,
    causalLines: extractCausalLines(input.messages),
    sessionStrip,
    hotspots: (input.provenanceSummary?.hotspotGroups ?? []).slice(0, 5),
    hero: {
      openLanes: input.laneSnapshots.length,
      provenanceHealthPct,
      provenanceOpsTotal: total,
      provenanceFailed: failed,
      lastProvenanceUpdateMs:
        input.provenanceSummary && input.provenanceSummary.lastUpdatedAt > 0
          ? input.provenanceSummary.lastUpdatedAt
          : null,
    },
    planningChip: input.planningChip,
  };
}

/** Agent inventory rows for system surface */
export function buildSystemCapacityRows(agents: AgentDiscoveryEntry[]): SystemCapacityRow[] {
  return agents.map((a) => ({
    id: a.agent_package,
    name: a.agent_card?.name ?? a.name,
    description: a.agent_card?.description ?? null,
    version: a.version,
    tools: a.agent_card?.tools ?? [],
    capabilities: a.agent_card?.capabilities ?? [],
    discovered: !!a.agent_card,
  }));
}

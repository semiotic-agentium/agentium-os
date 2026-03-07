<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useTheme } from "../composables/useTheme";
import { useMermaidRenderer } from "../composables/useMermaidRenderer";
import { useProvenanceOps } from "../composables/useProvenanceOps";
import type {
  ProvenanceOutcome,
  ProvenanceQueryParams,
  ProvenanceRowBase,
  ProvenanceResource,
} from "../types/provenance";

const props = defineProps<{
  contextId?: string;
  selectedAgentId?: string;
  isStreaming: boolean;
  diagrams?: string[];
}>();

const isOpen = ref(typeof window !== "undefined" ? window.innerWidth >= 1280 : true);
const activeTab = ref<"live" | "failures" | "anomalies" | "explore">("live");

const { theme } = useTheme();
const sources = computed(() => props.diagrams ?? []);
const { rendered } = useMermaidRenderer(sources, theme);
const expandedIdx = ref<number | null>(null);

const { createQuery } = useProvenanceOps();

const liveLlm = createQuery("llm_calls", {
  pageSize: 20,
  groupBy: ["agent_id", "agent_package", "agent_version", "model"],
  sortBy: "timestamp_ms",
  sortDir: "desc",
});
const liveTool = createQuery("tool_calls", {
  pageSize: 20,
  groupBy: ["agent_id", "agent_package", "agent_version", "tool_name"],
  sortBy: "timestamp_ms",
  sortDir: "desc",
});
const anomalyQuery = createQuery("llm_calls", {
  pageSize: 25,
  groupBy: ["agent_id", "agent_package", "agent_version", "provider", "model", "baml_prompt"],
  sortBy: "duration_ms",
  sortDir: "desc",
});
const failedLlmQuery = createQuery("llm_calls", {
  pageSize: 20,
  sortBy: "duration_ms",
  sortDir: "desc",
});
const failedToolQuery = createQuery("tool_calls", {
  pageSize: 20,
  sortBy: "duration_ms",
  sortDir: "desc",
});
const exploreQuery = createQuery("messages", {
  pageSize: 25,
  sortBy: "timestamp_ms",
  sortDir: "desc",
});

const exploreForm = ref<{
  resource: ProvenanceResource;
  outcome: ProvenanceOutcome;
  provider: string;
  model: string;
  toolName: string;
  bamlPrompt: string;
  groupBy: string;
  sortBy: string;
  sortDir: "asc" | "desc";
  pageSize: number;
}>({
  resource: "messages",
  outcome: "both",
  provider: "",
  model: "",
  toolName: "",
  bamlPrompt: "",
  groupBy: "",
  sortBy: "timestamp_ms",
  sortDir: "desc",
  pageSize: 25,
});

const selectedRow = ref<ProvenanceRowBase | null>(null);
const inspectorCollapsed = ref(false);
const backgroundPolling = ref(false);
const isExploreTab = computed(() => activeTab.value === "explore");

function baseScope(): Pick<ProvenanceQueryParams, "contextId" | "agentId"> {
  return {
    contextId: props.contextId,
    agentId: props.selectedAgentId,
  };
}

async function refreshLiveAndAnomalies() {
  if (!props.contextId) return;
  const scope = baseScope();
  await Promise.all([
    liveLlm.run(scope),
    liveTool.run(scope),
    anomalyQuery.run({
      ...scope,
      outcome: "both",
    }),
    failedLlmQuery.run({
      ...scope,
      outcome: "failed_only",
    }),
    failedToolQuery.run({
      ...scope,
      outcome: "failed_only",
    }),
  ]);
}

function stopBackgroundPolling() {
  if (!backgroundPolling.value) return;
  liveLlm.stopAutoRefresh();
  liveTool.stopAutoRefresh();
  anomalyQuery.stopAutoRefresh();
  failedLlmQuery.stopAutoRefresh();
  failedToolQuery.stopAutoRefresh();
  backgroundPolling.value = false;
}

function startBackgroundPolling() {
  if (backgroundPolling.value) return;
  // Keep these queries fresh for Live / Failures / Anomalies, but never while
  // Explore is active to avoid request storms that interfere with form input.
  liveLlm.startAutoRefresh({
    activeRef: computed(() => props.isStreaming && !isExploreTab.value),
    activeIntervalMs: 2500,
    idleIntervalMs: 12000,
  });
  liveTool.startAutoRefresh({
    activeRef: computed(() => props.isStreaming && !isExploreTab.value),
    activeIntervalMs: 2500,
    idleIntervalMs: 12000,
  });
  anomalyQuery.startAutoRefresh({
    activeRef: computed(() => props.isStreaming && !isExploreTab.value),
    activeIntervalMs: 3000,
    idleIntervalMs: 15000,
  });
  failedLlmQuery.startAutoRefresh({
    activeRef: computed(() => props.isStreaming && !isExploreTab.value),
    activeIntervalMs: 2500,
    idleIntervalMs: 12000,
  });
  failedToolQuery.startAutoRefresh({
    activeRef: computed(() => props.isStreaming && !isExploreTab.value),
    activeIntervalMs: 2500,
    idleIntervalMs: 12000,
  });
  backgroundPolling.value = true;
}

type AggregateCard = {
  label: string;
  count: number;
  failed: number;
  durationMs: number;
  tokenValue?: number;
  tokenIn?: number;
  tokenOut?: number;
  tokenLabel?: string;
};

const aggregateCards = computed<AggregateCard[]>(() => [
  {
    label: "LLM Calls",
    count: liveLlm.state.value.response?.summary.count ?? 0,
    failed: liveLlm.state.value.response?.summary.failedCount ?? 0,
    durationMs: liveLlm.state.value.response?.summary.durationMsTotal ?? 0,
    tokenValue: liveLlm.state.value.response?.summary.totalTokens ?? 0,
    tokenIn: liveLlm.state.value.response?.summary.promptTokensTotal ?? 0,
    tokenOut: liveLlm.state.value.response?.summary.completionTokensTotal ?? 0,
    tokenLabel: "in/out/total",
  },
  {
    label: "Tool Calls",
    count: liveTool.state.value.response?.summary.count ?? 0,
    failed: liveTool.state.value.response?.summary.failedCount ?? 0,
    durationMs: liveTool.state.value.response?.summary.durationMsTotal ?? 0,
  },
]);

const anomalyCards = computed(() => {
  const groups = anomalyQuery.state.value.response?.hotspotGroups ?? [];
  return groups
    .slice()
    .sort((a, b) => {
      if (b.failureRate !== a.failureRate) return b.failureRate - a.failureRate;
      if (b.avgDurationMs !== a.avgDurationMs) return b.avgDurationMs - a.avgDurationMs;
      return b.avgTotalTokens - a.avgTotalTokens;
    })
    .slice(0, 12);
});

type FailureRow = {
  kind: "llm" | "tool";
  row: ProvenanceRowBase;
};

const failureRows = computed<FailureRow[]>(() => {
  const llm = (failedLlmQuery.state.value.response?.rows ?? []).map((row) => ({
    kind: "llm" as const,
    row,
  }));
  const tool = (failedToolQuery.state.value.response?.rows ?? []).map((row) => ({
    kind: "tool" as const,
    row,
  }));
  return [...llm, ...tool]
    .sort((a, b) => {
      const ad = typeof a.row.duration_ms === "number" ? a.row.duration_ms : 0;
      const bd = typeof b.row.duration_ms === "number" ? b.row.duration_ms : 0;
      return bd - ad;
    })
    .slice(0, 20);
});

const failureSummary = computed(() => ({
  llm: failedLlmQuery.state.value.response?.summary.count ?? 0,
  tool: failedToolQuery.state.value.response?.summary.count ?? 0,
}));

type LiveHotspotItem = {
  kind: "llm" | "tool";
  groupKey: string;
  groupValues?: Array<string | null>;
  count: number;
  failureRate: number;
  avgDurationMs: number;
  avgTotalTokens: number;
};

const liveHotspotItems = computed<LiveHotspotItem[]>(() => {
  const llm = (liveLlm.state.value.response?.hotspotGroups ?? []).map((group) => ({
    kind: "llm" as const,
    groupKey: group.groupKey,
    groupValues: group.groupValues,
    count: group.count,
    failureRate: group.failureRate,
    avgDurationMs: group.avgDurationMs,
    avgTotalTokens: group.avgTotalTokens,
  }));
  const tool = (liveTool.state.value.response?.hotspotGroups ?? []).map((group) => ({
    kind: "tool" as const,
    groupKey: group.groupKey,
    groupValues: group.groupValues,
    count: group.count,
    failureRate: group.failureRate,
    avgDurationMs: group.avgDurationMs,
    avgTotalTokens: group.avgTotalTokens,
  }));
  return [...llm, ...tool]
    .sort((a, b) => {
      if (b.failureRate !== a.failureRate) return b.failureRate - a.failureRate;
      return b.avgDurationMs - a.avgDurationMs;
    })
    .slice(0, 8);
});

const exploreRows = computed(() => exploreQuery.state.value.response?.rows ?? []);
const exploreColumns = computed(() => {
  if (exploreRows.value.length === 0) return [];
  const keys = Array.from(
    new Set(
      exploreRows.value.flatMap((row) => Object.keys(row)),
    ),
  );
  const preferred = [
    "activity_kind",
    "activity_id",
    "timestamp_ms",
    "context_id",
    "task_id",
    "agent_display",
    "agent_package",
    "agent_version",
    "agent_id",
    "message_id",
    "provider",
    "model",
    "tool_name",
    "baml_prompt",
    "failure_class",
    "failure_evidence",
    "duration_ms",
    "total_tokens",
  ];
  const ordered = preferred.filter((k) => keys.includes(k));
  const extra = keys.filter((k) => !ordered.includes(k));
  return [...ordered, ...extra].slice(0, 10);
});

type SelectedRowEntry = {
  key: string;
  kind: "scalar" | "json";
  display: string;
};

function parseMaybeJson(value: string): unknown {
  let current: unknown = value;
  // Some fields are JSON encoded once (or occasionally twice).
  for (let i = 0; i < 2; i += 1) {
    if (typeof current !== "string") break;
    const trimmed = current.trim();
    const looksJsonContainer =
      (trimmed.startsWith("{") && trimmed.endsWith("}")) ||
      (trimmed.startsWith("[") && trimmed.endsWith("]"));
    const looksJsonString = trimmed.startsWith("\"") && trimmed.endsWith("\"");
    if (!looksJsonContainer && !looksJsonString) break;
    try {
      current = JSON.parse(trimmed);
    } catch {
      break;
    }
  }
  return current;
}

function normalizeDisplayValue(value: unknown): unknown {
  if (typeof value !== "string") return value;
  return parseMaybeJson(value);
}

function formatSelectedValue(value: unknown): Omit<SelectedRowEntry, "key"> {
  const normalized = normalizeDisplayValue(value);
  if (normalized === null || normalized === undefined) {
    return {
      kind: "scalar",
      display: String(normalized),
    };
  }
  if (typeof normalized === "object") {
    return {
      kind: "json",
      display: JSON.stringify(normalized, null, 2),
    };
  }
  return {
    kind: "scalar",
    display: String(normalized),
  };
}

const selectedRowEntries = computed<SelectedRowEntry[]>(() => {
  const row = selectedRow.value;
  if (!row) return [];
  const preferred = [
    "activity_kind",
    "activity_id",
    "timestamp_ms",
    "context_id",
    "task_id",
    "agent_display",
    "agent_package",
    "agent_version",
    "agent_id",
    "message_id",
    "provider",
    "model",
    "tool_name",
    "baml_prompt",
    "failure_class",
    "failure_evidence",
    "duration_ms",
    "total_tokens",
    "message_text",
    "message_content",
    "llm_call",
    "llm_result",
    "tool_call",
    "tool_result",
  ];
  const keys = Object.keys(row);
  const orderedKeys = [...preferred.filter((key) => keys.includes(key)), ...keys.filter((key) => !preferred.includes(key))];
  return orderedKeys.map((key) => {
    const formatted = formatSelectedValue(row[key]);
    return {
      key,
      kind: formatted.kind,
      display: formatted.display,
    };
  });
});

const payloadPriorityKeys = new Set([
  "message_text",
  "message_content",
  "llm_call",
  "llm_result",
  "tool_call",
  "tool_result",
]);

const payloadEntries = computed(() =>
  selectedRowEntries.value.filter((entry) => payloadPriorityKeys.has(entry.key)),
);

const detailEntries = computed(() =>
  selectedRowEntries.value.filter((entry) => !payloadPriorityKeys.has(entry.key)),
);

function openModal(i: number) {
  expandedIdx.value = i;
}

function closeModal() {
  expandedIdx.value = null;
}

function onOverlayClick(e: MouseEvent) {
  if ((e.target as HTMLElement).classList.contains("diagram-modal-overlay")) {
    closeModal();
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") closeModal();
}

function downloadSvg(svg: string, index: number) {
  const blob = new Blob([svg], { type: "image/svg+xml" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `trace-diagram-${index + 1}.svg`;
  a.click();
  URL.revokeObjectURL(url);
}

const activeFilterChips = computed(() => {
  const chips: Array<{ key: string; label: string }> = [];
  const params = exploreQuery.state.value.params;
  if (params.provider) chips.push({ key: "provider", label: `provider:${params.provider}` });
  if (params.model) chips.push({ key: "model", label: `model:${params.model}` });
  if (params.toolName) chips.push({ key: "toolName", label: `tool:${params.toolName}` });
  if (params.bamlPrompt) chips.push({ key: "bamlPrompt", label: `prompt:${params.bamlPrompt}` });
  if (params.outcome && params.outcome !== "both") {
    chips.push({ key: "outcome", label: `outcome:${params.outcome}` });
  }
  if (params.groupBy && params.groupBy.length > 0) {
    chips.push({ key: "groupBy", label: `groupBy:${params.groupBy.join(",")}` });
  }
  return chips;
});

function formatCompact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function formatDuration(ms: number): string {
  if (ms > 1000) return `${(ms / 1000).toFixed(2)}s`;
  return `${ms}ms`;
}

function shortId(id: string): string {
  if (id.length <= 12) return id;
  return `${id.slice(0, 8)}...${id.slice(-4)}`;
}

function asDisplayIdentity(agentId: string | undefined, pkg: string | undefined, ver: string | undefined): string {
  const packageName = pkg && pkg !== "unknown" ? pkg : "";
  const version = ver && ver !== "unknown" ? ver : "";
  if (packageName && version) return `${packageName}/${version}`;
  if (packageName) return packageName;
  if (agentId && agentId !== "unknown") return shortId(agentId);
  return "unknown-agent";
}

function normalizedGroupValue(raw: string | null | undefined): string | undefined {
  if (typeof raw !== "string") return undefined;
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function groupValueAt(
  values: Array<string | null> | undefined,
  groupKey: string,
  index: number,
): string | undefined {
  const fromValues = normalizedGroupValue(values?.[index]);
  if (fromValues) return fromValues;
  return normalizedGroupValue(groupKey.split("|")[index]);
}

function liveHotspotLabel(item: LiveHotspotItem): string {
  const agentIdRaw = groupValueAt(item.groupValues, item.groupKey, 0);
  const pkgRaw = groupValueAt(item.groupValues, item.groupKey, 1);
  const verRaw = groupValueAt(item.groupValues, item.groupKey, 2);
  const dimRaw = groupValueAt(item.groupValues, item.groupKey, 3);
  const agentDisplay = asDisplayIdentity(agentIdRaw, pkgRaw, verRaw);
  if (item.kind === "llm") {
    const model = dimRaw ?? "unknown-model";
    return `LLM · ${agentDisplay} · ${model}`;
  }
  const tool = dimRaw ?? "unknown-tool";
  return `Tool · ${agentDisplay} · ${tool}`;
}

function applyLiveHotspotDrilldown(item: LiveHotspotItem) {
  const agentId = groupValueAt(item.groupValues, item.groupKey, 0);
  const dim = groupValueAt(item.groupValues, item.groupKey, 3);
  exploreForm.value.resource = item.kind === "llm" ? "llm_calls" : "tool_calls";
  if (item.kind === "llm") {
    exploreForm.value.model = dim ?? "";
    exploreForm.value.toolName = "";
    exploreForm.value.sortBy = "duration_ms";
  } else {
    exploreForm.value.toolName = dim ?? "";
    exploreForm.value.model = "";
    exploreForm.value.sortBy = "duration_ms";
  }
  exploreForm.value.outcome = "both";
  exploreForm.value.sortDir = "desc";
  if (agentId) {
    exploreQuery.setParams({ agentId });
  }
  activeTab.value = "explore";
  applyExploreQuery(true);
}

function failureTitle(item: FailureRow): string {
  const row = item.row;
  const agentDisplay = asDisplayIdentity(
    typeof row.agent_id === "string" ? row.agent_id : undefined,
    typeof row.agent_package === "string" ? row.agent_package : undefined,
    typeof row.agent_version === "string" ? row.agent_version : undefined,
  );
  if (item.kind === "llm") {
    const model = typeof row.model === "string" && row.model.length > 0 ? row.model : "unknown-model";
    return `LLM · ${agentDisplay} · ${model}`;
  }
  const tool =
    typeof row.tool_name === "string" && row.tool_name.length > 0 ? row.tool_name : "unknown-tool";
  return `Tool · ${agentDisplay} · ${tool}`;
}

function failureSubtitle(item: FailureRow): string {
  const row = item.row;
  const failureClass =
    typeof row.failure_class === "string" && row.failure_class.length > 0
      ? row.failure_class
      : "missing_failure_class";
  const failureEvidence =
    typeof row.failure_evidence === "string" && row.failure_evidence.length > 0
      ? row.failure_evidence
      : "missing_failure_evidence";
  const duration = typeof row.duration_ms === "number" ? row.duration_ms : 0;
  return `${failureClass} (${failureEvidence}) · ${formatDuration(Math.round(duration))}`;
}

function applyFailureDrilldown(item: FailureRow) {
  const row = item.row;
  exploreForm.value.resource = item.kind === "llm" ? "llm_calls" : "tool_calls";
  exploreForm.value.outcome = "failed_only";
  exploreForm.value.sortBy = "duration_ms";
  exploreForm.value.sortDir = "desc";
  exploreForm.value.provider = typeof row.provider === "string" ? row.provider : "";
  exploreForm.value.model = typeof row.model === "string" ? row.model : "";
  exploreForm.value.toolName = typeof row.tool_name === "string" ? row.tool_name : "";
  exploreForm.value.bamlPrompt = typeof row.baml_prompt === "string" ? row.baml_prompt : "";
  const agentId = typeof row.agent_id === "string" ? row.agent_id : "";
  if (agentId) {
    exploreQuery.setParams({ agentId });
  }
  activeTab.value = "explore";
  applyExploreQuery(true);
}

function rowKey(row: ProvenanceRowBase, idx: number): string {
  const activityId = row.activity_id;
  if (typeof activityId === "string" && activityId.length > 0) {
    return activityId;
  }
  const activityKind = typeof row.activity_kind === "string" ? row.activity_kind : "unknown";
  const contextId = typeof row.context_id === "string" ? row.context_id : "unknown";
  const messageId = typeof row.message_id === "string" ? row.message_id : "unknown";
  return `${activityKind}:${contextId}:${messageId}:${idx}`;
}

function selectRow(row: ProvenanceRowBase) {
  selectedRow.value = row;
  inspectorCollapsed.value = false;
}

function applyExploreQuery(resetCursor = true) {
  exploreQuery.setResource(exploreForm.value.resource);
  const groupBy = exploreForm.value.groupBy
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  void exploreQuery.run({
    ...baseScope(),
    outcome: exploreForm.value.outcome,
    provider: exploreForm.value.provider || undefined,
    model: exploreForm.value.model || undefined,
    toolName: exploreForm.value.toolName || undefined,
    bamlPrompt: exploreForm.value.bamlPrompt || undefined,
    groupBy: groupBy.length > 0 ? groupBy : undefined,
    sortBy: exploreForm.value.sortBy || undefined,
    sortDir: exploreForm.value.sortDir,
    pageSize: exploreForm.value.pageSize,
    cursor: resetCursor ? undefined : exploreQuery.state.value.params.cursor,
  });
}

function applyAnomalyDrilldown(anomaly: { groupKey: string; groupValues?: Array<string | null> }) {
  const agentId = groupValueAt(anomaly.groupValues, anomaly.groupKey, 0);
  const provider = groupValueAt(anomaly.groupValues, anomaly.groupKey, 3);
  const model = groupValueAt(anomaly.groupValues, anomaly.groupKey, 4);
  const bamlPrompt = groupValueAt(anomaly.groupValues, anomaly.groupKey, 5);
  exploreForm.value.resource = "llm_calls";
  exploreForm.value.provider = provider ?? "";
  exploreForm.value.model = model ?? "";
  exploreForm.value.bamlPrompt = bamlPrompt ?? "";
  if (agentId) {
    exploreQuery.setParams({ agentId });
  }
  exploreForm.value.outcome = "both";
  exploreForm.value.sortBy = "duration_ms";
  exploreForm.value.sortDir = "desc";
  activeTab.value = "explore";
  applyExploreQuery(true);
}

function anomalyLabel(anomaly: { groupKey: string; groupValues?: Array<string | null> }): string {
  const agentId = groupValueAt(anomaly.groupValues, anomaly.groupKey, 0);
  const pkg = groupValueAt(anomaly.groupValues, anomaly.groupKey, 1);
  const ver = groupValueAt(anomaly.groupValues, anomaly.groupKey, 2);
  const provider = groupValueAt(anomaly.groupValues, anomaly.groupKey, 3);
  const model = groupValueAt(anomaly.groupValues, anomaly.groupKey, 4);
  const prompt = groupValueAt(anomaly.groupValues, anomaly.groupKey, 5);
  const agentDisplay = asDisplayIdentity(agentId, pkg, ver);
  const providerLabel = provider ?? "unknown-provider";
  const modelLabel = model ?? "unknown-model";
  const promptLabel = prompt ?? "unknown-prompt";
  return `${agentDisplay} · ${providerLabel}/${modelLabel} · ${promptLabel}`;
}

function removeChip(key: string) {
  if (key === "provider") exploreForm.value.provider = "";
  if (key === "model") exploreForm.value.model = "";
  if (key === "toolName") exploreForm.value.toolName = "";
  if (key === "bamlPrompt") exploreForm.value.bamlPrompt = "";
  if (key === "groupBy") exploreForm.value.groupBy = "";
  if (key === "outcome") exploreForm.value.outcome = "both";
  applyExploreQuery(true);
}

function clearAllFilters() {
  exploreForm.value.provider = "";
  exploreForm.value.model = "";
  exploreForm.value.toolName = "";
  exploreForm.value.bamlPrompt = "";
  exploreForm.value.groupBy = "";
  exploreForm.value.outcome = "both";
  applyExploreQuery(true);
}

watch(
  () => [props.contextId, props.selectedAgentId],
  () => {
    if (!props.contextId) return;
    void refreshLiveAndAnomalies();
    if (isExploreTab.value) {
      applyExploreQuery(true);
    }
  },
  { immediate: true },
);

watch(
  () => activeTab.value,
  (tab) => {
    if (tab === "explore") {
      stopBackgroundPolling();
      applyExploreQuery(true);
      return;
    }
    startBackgroundPolling();
    void refreshLiveAndAnomalies();
  },
  { immediate: true },
);

onMounted(() => {
  if (isExploreTab.value) {
    stopBackgroundPolling();
  } else {
    startBackgroundPolling();
  }
});
</script>

<template>
  <aside class="provenance-pane" :class="{ open: isOpen }">
    <button
      class="provenance-toggle"
      :title="isOpen ? 'Collapse provenance pane' : 'Expand provenance pane'"
      :aria-label="isOpen ? 'Collapse provenance pane' : 'Expand provenance pane'"
      @click="isOpen = !isOpen"
    >
      <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
        <polyline :points="isOpen ? '9 18 15 12 9 6' : '15 18 9 12 15 6'" />
      </svg>
    </button>

    <div class="provenance-pane-inner">
      <header class="provenance-header">
        <div class="provenance-header-title">Traces</div>
        <div class="provenance-header-status">
          <span class="status-dot" />
          {{ props.isStreaming ? "Live" : "Idle" }}
        </div>
      </header>

      <div class="provenance-tabs">
        <button class="provenance-tab" :class="{ active: activeTab === 'live' }" @click="activeTab = 'live'">Live</button>
        <button class="provenance-tab" :class="{ active: activeTab === 'failures' }" @click="activeTab = 'failures'">Failures</button>
        <button class="provenance-tab" :class="{ active: activeTab === 'anomalies' }" @click="activeTab = 'anomalies'">Anomalies</button>
        <button class="provenance-tab" :class="{ active: activeTab === 'explore' }" @click="activeTab = 'explore'">Explore</button>
      </div>

      <div class="provenance-body">
        <div v-if="!props.contextId" class="provenance-empty">
          Start a chat turn to attach context-scoped provenance.
        </div>

        <template v-else-if="activeTab === 'live'">
          <div class="provenance-card-grid">
            <article v-for="card in aggregateCards" :key="card.label" class="provenance-card">
              <div class="provenance-card-label">{{ card.label }}</div>
              <div class="provenance-card-value">{{ formatCompact(card.count) }}</div>
              <div class="provenance-card-sub">
                failed {{ card.failed }} · {{ formatDuration(card.durationMs) }}<template
                  v-if="card.tokenValue !== undefined"
                >
                  · {{ card.tokenLabel }}
                  <template v-if="card.tokenIn !== undefined && card.tokenOut !== undefined">
                    {{ formatCompact(card.tokenIn) }}/{{ formatCompact(card.tokenOut) }}/{{ formatCompact(card.tokenValue) }}
                  </template>
                  <template v-else>
                    {{ formatCompact(card.tokenValue) }}
                  </template>
                </template>
              </div>
            </article>
          </div>

          <section class="provenance-section">
            <div class="provenance-section-title">Live Trace Diagram</div>
            <div v-if="rendered.length === 0" class="provenance-empty">
              The conversation sequence diagram will appear here after the first reply.
            </div>
            <div v-else class="reasoning-diagrams">
              <div
                v-for="(item, i) in rendered.slice(0, 1)"
                :key="i"
                class="diagram-card"
                :class="{ clickable: !item.error }"
                @click="!item.error && openModal(i)"
                :title="item.error ? undefined : 'Click to expand'"
              >
                <div v-if="item.error" class="diagram-error">
                  <span class="diagram-error-label">Render error</span>
                  <pre>{{ item.error }}</pre>
                </div>
                <template v-else>
                  <div class="diagram-svg" v-html="item.svg" />
                  <div class="diagram-expand-hint">
                    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <polyline points="15 3 21 3 21 9" /><polyline points="9 21 3 21 3 15" />
                      <line x1="21" y1="3" x2="14" y2="10" /><line x1="3" y1="21" x2="10" y2="14" />
                    </svg>
                  </div>
                </template>
              </div>
            </div>
          </section>

          <section class="provenance-section">
            <div class="provenance-section-title">Hotspot Groups</div>
            <div
              v-if="liveHotspotItems.length === 0"
              class="provenance-empty"
            >
              No hotspot groups yet.
            </div>
            <ul v-else class="provenance-list">
              <li
                v-for="item in liveHotspotItems"
                :key="`${item.kind}:${item.groupKey}`"
                class="provenance-list-item"
              >
                <button
                  class="anomaly-card"
                  @click="applyLiveHotspotDrilldown(item)"
                >
                  <div class="group-key">{{ liveHotspotLabel(item) }}</div>
                <div class="group-metrics">
                    <span>count {{ item.count }}</span>
                    <span>fail {{ (item.failureRate * 100).toFixed(1) }}%</span>
                    <span>lat {{ formatDuration(Math.round(item.avgDurationMs)) }}</span>
                </div>
                </button>
              </li>
            </ul>
          </section>
        </template>

        <template v-else-if="activeTab === 'failures'">
          <div class="provenance-section-title">
            Failed Activities · LLM {{ failureSummary.llm }} · Tool {{ failureSummary.tool }}
          </div>
          <div v-if="failureRows.length === 0" class="provenance-empty">
            No failed activities in this context.
          </div>
          <div v-else class="anomaly-grid">
            <button
              v-for="item in failureRows"
              :key="`${item.kind}:${rowKey(item.row, 0)}`"
              class="anomaly-card"
              @click="applyFailureDrilldown(item)"
            >
              <div class="anomaly-key">{{ failureTitle(item) }}</div>
              <div class="anomaly-metrics">
                <span>{{ failureSubtitle(item) }}</span>
                <span>click to drill down</span>
              </div>
            </button>
          </div>
        </template>

        <template v-else-if="activeTab === 'anomalies'">
          <div class="provenance-section-title">Top Anomalous Groups</div>
          <div v-if="anomalyCards.length === 0" class="provenance-empty">
            No anomalies detected.
          </div>
          <div v-else class="anomaly-grid">
            <button
              v-for="anomaly in anomalyCards"
              :key="anomaly.groupKey"
              class="anomaly-card"
              @click="applyAnomalyDrilldown(anomaly)"
            >
              <div class="anomaly-key">{{ anomalyLabel(anomaly) }}</div>
              <div class="anomaly-metrics">
                <span>{{ (anomaly.failureRate * 100).toFixed(1) }}% failures</span>
                <span>{{ formatDuration(Math.round(anomaly.avgDurationMs)) }} avg</span>
                <span>{{ formatCompact(Math.round(anomaly.avgTotalTokens)) }} tok avg</span>
              </div>
            </button>
          </div>
        </template>

        <template v-else>
          <div class="explore-controls">
            <select v-model="exploreForm.resource">
              <option value="llm_calls">LLM calls</option>
              <option value="tool_calls">Tool calls</option>
              <option value="messages">Messages</option>
            </select>
            <select v-model="exploreForm.outcome">
              <option value="both">both</option>
              <option value="failed_only">failed_only</option>
              <option value="successful_only">successful_only</option>
            </select>
            <input
              v-if="exploreForm.resource === 'llm_calls'"
              v-model="exploreForm.provider"
              placeholder="provider"
            />
            <input
              v-if="exploreForm.resource === 'llm_calls'"
              v-model="exploreForm.model"
              placeholder="model"
            />
            <input
              v-if="exploreForm.resource === 'tool_calls'"
              v-model="exploreForm.toolName"
              placeholder="toolName"
            />
            <input
              v-if="exploreForm.resource !== 'messages'"
              v-model="exploreForm.bamlPrompt"
              placeholder="bamlPrompt"
            />
            <input v-model="exploreForm.groupBy" placeholder="groupBy (comma)" />
            <input v-model="exploreForm.sortBy" placeholder="sortBy" />
            <select v-model="exploreForm.sortDir">
              <option value="desc">desc</option>
              <option value="asc">asc</option>
            </select>
            <input v-model.number="exploreForm.pageSize" type="number" min="1" max="200" />
            <button class="action-btn" :disabled="exploreQuery.state.value.loading" @click="applyExploreQuery(true)">
              {{ exploreQuery.state.value.loading ? "Running..." : "Run" }}
            </button>
          </div>

          <div class="filter-chips">
            <button
              v-for="chip in activeFilterChips"
              :key="chip.key"
              class="chip"
              @click="removeChip(chip.key)"
            >
              {{ chip.label }} ×
            </button>
            <button v-if="activeFilterChips.length > 0" class="chip clear" @click="clearAllFilters">
              clear all
            </button>
          </div>

          <div class="explore-pagination">
            <button
              class="action-btn"
              :disabled="exploreQuery.state.value.loading || !exploreQuery.hasPreviousPage.value"
              @click="void exploreQuery.previousPage()"
            >
              Prev
            </button>
            <button
              class="action-btn"
              :disabled="exploreQuery.state.value.loading || !exploreQuery.hasNextPage.value"
              @click="void exploreQuery.nextPage()"
            >
              Next
            </button>
          </div>

          <div v-if="exploreQuery.state.value.error" class="provenance-error">
            {{ exploreQuery.state.value.error }}
          </div>

          <div class="explore-table-wrap">
            <table class="explore-table">
              <thead>
                <tr>
                  <th v-for="col in exploreColumns" :key="col">{{ col }}</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(row, idx) in exploreRows"
                  :key="rowKey(row, idx)"
                  @click="selectRow(row)"
                >
                  <td v-for="col in exploreColumns" :key="col">
                    {{ String(row[col] ?? "") }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>

          <section v-if="selectedRow" class="row-inspector">
            <div class="row-inspector-title">
              <span>Selected activity details</span>
              <div class="row-inspector-actions">
                <button class="action-btn small" @click="inspectorCollapsed = !inspectorCollapsed">
                  {{ inspectorCollapsed ? "Expand" : "Collapse" }}
                </button>
                <button
                  class="action-btn small"
                  @click="
                    selectedRow = null;
                    inspectorCollapsed = false;
                  "
                >
                  Close
                </button>
              </div>
            </div>

            <div v-if="!inspectorCollapsed" class="row-inspector-body">
              <section v-if="payloadEntries.length > 0" class="row-inspector-section">
                <div class="row-inspector-section-title">Call/Result payloads</div>
                <div
                  v-for="entry in payloadEntries"
                  :key="`payload:${entry.key}`"
                  class="row-inspector-item payload"
                >
                  <div class="row-inspector-key">{{ entry.key }}</div>
                  <pre
                    v-if="entry.kind === 'json'"
                    class="row-inspector-json"
                  >{{ entry.display }}</pre>
                  <div v-else class="row-inspector-value">{{ entry.display }}</div>
                </div>
              </section>

              <section class="row-inspector-section">
                <div class="row-inspector-section-title">Activity fields</div>
                <div
                  v-for="entry in detailEntries"
                  :key="`detail:${entry.key}`"
                  class="row-inspector-item"
                >
                  <div class="row-inspector-key">{{ entry.key }}</div>
                  <pre
                    v-if="entry.kind === 'json'"
                    class="row-inspector-json"
                  >{{ entry.display }}</pre>
                  <div v-else class="row-inspector-value">{{ entry.display }}</div>
                </div>
              </section>
            </div>
          </section>
        </template>
      </div>
    </div>
  </aside>

  <Teleport to="body">
    <div
      v-if="expandedIdx !== null && rendered[expandedIdx] && !rendered[expandedIdx]!.error"
      class="diagram-modal-overlay"
      @click="onOverlayClick"
      @keydown="onKeydown"
      tabindex="-1"
    >
      <div class="diagram-modal" role="dialog" aria-modal="true" aria-label="Trace diagram fullscreen view">
        <header class="diagram-modal-header">
          <span class="diagram-modal-title">Trace Diagram</span>
          <div class="diagram-modal-actions">
            <button
              class="diagram-modal-btn"
              title="Download SVG"
              @click="downloadSvg(rendered[expandedIdx!]!.svg, expandedIdx!)"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                <polyline points="7 10 12 15 17 10" />
                <line x1="12" y1="15" x2="12" y2="3" />
              </svg>
              Download
            </button>
            <button
              class="diagram-modal-btn diagram-modal-close"
              title="Close"
              @click="closeModal"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
                <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
        </header>
        <div
          class="diagram-modal-body"
          v-html="rendered[expandedIdx!]!.svg"
        />
      </div>
    </div>
  </Teleport>
</template>

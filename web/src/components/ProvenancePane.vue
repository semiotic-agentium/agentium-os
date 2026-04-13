<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { useTheme } from "../composables/useTheme";
import { useMermaidRenderer } from "../composables/useMermaidRenderer";
import { useProvenanceOps } from "../composables/useProvenanceOps";
import { groupValueAt } from "../utils/format";
import type {
  ContextPlanningResponse,
  ContextPlanningTaskSnapshot,
  ProvenanceQueryParams,
} from "../types/provenance";

import ExploreTab from "./provenance/ExploreTab.vue";
import ProvenanceLiveTab from "./provenance/ProvenanceLiveTab.vue";
import type { HotspotDrilldownParams } from "./provenance/ProvenanceLiveTab.vue";
import ProvenanceDriftTab from "./provenance/ProvenanceDriftTab.vue";
import ProvenanceFailuresTab from "./provenance/ProvenanceFailuresTab.vue";
import type { DrilldownFromFailure } from "./provenance/ProvenanceFailuresTab.vue";
import ProvenanceAnomaliesTab from "./provenance/ProvenanceAnomaliesTab.vue";
import type { DrilldownFromAnomaly } from "./provenance/ProvenanceAnomaliesTab.vue";

const props = defineProps<{
  contextId?: string;
  taskId?: string;
  selectedAgentId?: string;
  isStreaming: boolean;
  diagrams?: string[];
  /** Bumps when A2A SSE signals new provenance (tool done, task state, final); edge-triggers Live refresh */
  traceRefreshTick?: number;
}>();

const isOpen = ref(typeof window !== "undefined" ? window.innerWidth > 1400 : true);
const activeTab = ref<"live" | "failures" | "anomalies" | "drift" | "explore">("live");

const { theme } = useTheme();
const sources = computed(() => props.diagrams ?? []);
const { rendered } = useMermaidRenderer(sources, theme);
const expandedIdx = ref<number | null>(null);
const exploreTabRef = ref<InstanceType<typeof ExploreTab> | null>(null);

const { createQuery } = useProvenanceOps();

// ── Query controllers ───────────────────────────────────────────────────────

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

// ── Polling & data refresh ──────────────────────────────────────────────────

const isExploreTab = computed(() => activeTab.value === "explore");
const pollTimer = ref<number | null>(null);
const pollInFlight = ref(false);
const planningState = ref<{
  loading: boolean;
  error: string | null;
  response: ContextPlanningResponse | null;
}>({
  loading: false,
  error: null,
  response: null,
});

function baseScope(): Pick<ProvenanceQueryParams, "contextId" | "agentId"> {
  return {
    contextId: props.contextId,
    agentId: props.selectedAgentId,
  };
}

async function refreshForActiveTab() {
  if (!props.contextId || pollInFlight.value) return;
  pollInFlight.value = true;
  try {
    const scope = baseScope();
    if (activeTab.value === "live") {
      await Promise.all([liveLlm.run(scope), liveTool.run(scope), refreshPlanning()]);
      return;
    }
    if (activeTab.value === "failures") {
      await Promise.all([
        failedLlmQuery.run({ ...scope, outcome: "failed_only" }),
        failedToolQuery.run({ ...scope, outcome: "failed_only" }),
      ]);
      return;
    }
    if (activeTab.value === "anomalies") {
      await anomalyQuery.run({ ...scope, outcome: "both" });
      return;
    }
    if (activeTab.value === "drift") {
      await refreshPlanning();
    }
  } finally {
    pollInFlight.value = false;
  }
}

async function refreshPlanning() {
  if (!props.contextId) return;
  planningState.value.loading = true;
  planningState.value.error = null;
  try {
    const response = await fetch(`/contexts/${props.contextId}/planning`);
    if (!response.ok) {
      if (response.status === 404) {
        planningState.value.response = null;
        return;
      }
      throw new Error(`Planning request failed: ${response.status}`);
    }
    planningState.value.response = (await response.json()) as ContextPlanningResponse;
  } catch (error) {
    planningState.value.error = (error as Error).message;
  } finally {
    planningState.value.loading = false;
  }
}

function stopPolling() {
  if (pollTimer.value !== null) {
    window.clearTimeout(pollTimer.value);
    pollTimer.value = null;
  }
}

function schedulePolling(immediate = false) {
  stopPolling();
  if (!props.contextId || activeTab.value === "explore") return;
  const delay = immediate ? 0 : props.isStreaming ? 45000 : 12000;
  pollTimer.value = window.setTimeout(async () => {
    await refreshForActiveTab();
    schedulePolling(false);
  }, delay);
}

// ── Derived state for sub-components ────────────────────────────────────────

const planningTasks = computed<ContextPlanningTaskSnapshot[]>(() => {
  return planningState.value.response?.tasks ?? [];
});

function uniqueNonEmpty(values: Array<string | undefined>): string[] {
  return [...new Set(values.filter((value): value is string => typeof value === "string" && value.length > 0))];
}

const HOTSPOT_PKG_IDX = 1;
const HOTSPOT_DIM_IDX = 3;

const traceAgentPackages = computed(() =>
  uniqueNonEmpty([
    ...(liveLlm.state.value.response?.hotspotGroups ?? []).map((g) => groupValueAt(g.groupValues, g.groupKey, HOTSPOT_PKG_IDX)),
    ...(liveTool.state.value.response?.hotspotGroups ?? []).map((g) => groupValueAt(g.groupValues, g.groupKey, HOTSPOT_PKG_IDX)),
  ]),
);

const traceModels = computed(() =>
  uniqueNonEmpty(
    (liveLlm.state.value.response?.hotspotGroups ?? []).map((g) => groupValueAt(g.groupValues, g.groupKey, HOTSPOT_DIM_IDX)),
  ),
);

const traceTools = computed(() =>
  uniqueNonEmpty(
    (liveTool.state.value.response?.hotspotGroups ?? []).map((g) => groupValueAt(g.groupValues, g.groupKey, HOTSPOT_DIM_IDX)),
  ),
);

const episodeTaskIds = computed<string[]>(() => {
  const ids: string[] = [];
  const seen = new Set<string>();
  if (props.taskId) {
    ids.push(props.taskId);
    seen.add(props.taskId);
  }
  for (const tid of planningState.value.response?.allTaskIds ?? []) {
    if (!seen.has(tid)) { ids.push(tid); seen.add(tid); }
  }
  for (const t of planningTasks.value) {
    if (!seen.has(t.taskId)) { ids.push(t.taskId); seen.add(t.taskId); }
  }
  return ids;
});

// ── Drill-down routing (tabs → Explore) ─────────────────────────────────────

function drillToExplore(params: Record<string, unknown>) {
  activeTab.value = "explore";
  requestAnimationFrame(() => {
    exploreTabRef.value?.applyDrilldown(params);
  });
}

function onHotspotDrilldown(params: HotspotDrilldownParams) {
  drillToExplore({
    resource: params.kind === "llm" ? "llm_calls" : "tool_calls",
    model: params.model,
    toolName: params.toolName,
    outcome: params.outcome,
    sortBy: params.sortBy,
    sortDir: params.sortDir,
    agentId: params.agentId,
  });
}

function onFailureDrilldown(params: DrilldownFromFailure) {
  drillToExplore(params);
}

function onAnomalyDrilldown(params: DrilldownFromAnomaly) {
  drillToExplore(params);
}

function onDriftDrilldown(taskId: string) {
  drillToExplore({
    resource: "llm_calls",
    outcome: "both",
    taskId,
    model: "",
    toolName: "",
    bamlPrompt: "",
    provider: "",
    sortBy: "timestamp_ms",
    sortDir: "desc",
  });
}

// ── Diagram modal ───────────────────────────────────────────────────────────

function openModal(i: number) { expandedIdx.value = i; }
function closeModal() { expandedIdx.value = null; }

function onOverlayClick(e: MouseEvent) {
  if ((e.target as HTMLElement).classList.contains("diagram-modal-overlay")) closeModal();
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

async function downloadEpisodeText(taskId: string) {
  const response = await fetch(`/tasks/${taskId}/episode/text`);
  if (!response.ok) {
    console.error("episode/text fetch failed:", response.status);
    return;
  }
  const blob = await response.blob();
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `episode-${taskId}.txt`;
  link.style.display = "none";
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  URL.revokeObjectURL(url);
}

// ── Watchers ────────────────────────────────────────────────────────────────

watch(
  () => [props.contextId, props.selectedAgentId],
  () => {
    if (!props.contextId) return;
    void refreshForActiveTab();
    if (!isExploreTab.value) {
      schedulePolling(true);
    }
  },
  { immediate: true },
);

watch(
  () => props.traceRefreshTick ?? 0,
  (tick, prev) => {
    if (tick === prev) return;
    if (!props.contextId || isExploreTab.value) return;
    void refreshForActiveTab();
    schedulePolling(false);
  },
);

watch(
  () => [activeTab.value] as const,
  ([tab]) => {
    if (tab === "explore") {
      stopPolling();
      return;
    }
    schedulePolling(true);
  },
  { immediate: true },
);

onMounted(() => {
  if (isExploreTab.value) {
    stopPolling();
  } else {
    schedulePolling(true);
  }
});

onUnmounted(() => {
  stopPolling();
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

    <div v-show="isOpen" class="provenance-pane-inner">
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
        <button class="provenance-tab" :class="{ active: activeTab === 'drift' }" @click="activeTab = 'drift'">Drift</button>
        <button class="provenance-tab" :class="{ active: activeTab === 'explore' }" @click="activeTab = 'explore'">Explore</button>
      </div>

      <div class="provenance-body">
        <div v-if="!props.contextId" class="provenance-empty">
          Start a chat turn to attach context-scoped provenance.
        </div>

        <ProvenanceLiveTab
          v-else-if="activeTab === 'live'"
          :live-llm-response="liveLlm.state.value.response"
          :live-tool-response="liveTool.state.value.response"
          :planning-tasks="planningTasks"
          :planning-loading="planningState.loading"
          :planning-error="planningState.error"
          :rendered="rendered"
          :task-id="props.taskId"
          :is-streaming="props.isStreaming"
          :episode-task-ids="episodeTaskIds"
          :trace-agent-packages="traceAgentPackages"
          :trace-models="traceModels"
          :trace-tools="traceTools"
          @hotspot-drilldown="onHotspotDrilldown"
          @open-modal="openModal"
          @download-episode-text="downloadEpisodeText"
        />

        <ProvenanceFailuresTab
          v-else-if="activeTab === 'failures'"
          :failed-llm-response="failedLlmQuery.state.value.response"
          :failed-tool-response="failedToolQuery.state.value.response"
          @drilldown="onFailureDrilldown"
        />

        <ProvenanceAnomaliesTab
          v-else-if="activeTab === 'anomalies'"
          :anomaly-response="anomalyQuery.state.value.response"
          @drilldown="onAnomalyDrilldown"
        />

        <ProvenanceDriftTab
          v-else-if="activeTab === 'drift'"
          :planning-tasks="planningTasks"
          @drill-to-drift-calls="onDriftDrilldown"
        />

        <ExploreTab
          v-else
          ref="exploreTabRef"
          :context-id="props.contextId"
          :selected-agent-id="props.selectedAgentId"
        />
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

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { AgentDiscoveryEntry, ChatMessage, ContextMetricsResponse } from "../types/a2a";
import type { ContextPlanningResponse, ProvenanceGroupHotspot } from "../types/provenance";
import { useMermaidRenderer } from "../composables/useMermaidRenderer";
import { useTheme } from "../composables/useTheme";
import {
  buildDashboardViewModel,
  buildSystemCapacityRows,
  type DashboardLaneSnapshot,
  type ProvenancePaneTab,
} from "../composables/useDashboardViewModel";
import DashboardRuntimeSection from "./dashboard/DashboardRuntimeSection.vue";
import DashboardCausalSection from "./dashboard/DashboardCausalSection.vue";
import DashboardAttentionSection from "./dashboard/DashboardAttentionSection.vue";
import DashboardSystemSection from "./dashboard/DashboardSystemSection.vue";
import { encodeContextIdForPath } from "../utils/contextPath";

const props = defineProps<{
  agents: AgentDiscoveryEntry[];
  laneSnapshots: DashboardLaneSnapshot[];
  contextMetrics: ContextMetricsResponse | null;
  promptMessageCharsSessionCurrent?: number | null;
  provenanceDiagram: string;
  messages: ChatMessage[];
  contextId?: string;
  runnerOnline: boolean;
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
}>();

const emit = defineEmits<{
  "open-settings": [];
  "go-chat": [payload?: { tabId?: string; provenanceTab?: ProvenancePaneTab }];
}>();

// ── Planning (optional provenance-backed tasks only) ────────────────────────
const planningData = ref<ContextPlanningResponse | null>(null);

watch(
  () => props.contextId,
  async (ctxId) => {
    if (!ctxId) {
      planningData.value = null;
      return;
    }
    try {
      const res = await fetch(`/contexts/${encodeContextIdForPath(ctxId)}/planning`);
      if (res.ok) planningData.value = await res.json();
    } catch {
      // planning endpoint may not be available
    }
  },
  { immediate: true },
);

const planningStatus = computed(() => {
  const tasks = planningData.value?.tasks ?? [];
  if (tasks.length === 0) return null;
  const task = tasks[0]!;
  const intent = task.currentIntent?.description ?? null;
  const summary = task.stepSummary;
  const total = summary?.total ?? 0;
  const completed = summary?.completed ?? 0;
  const driftSeverity = task.drift?.compositeSeverity ?? null;
  return { intent, total, completed, driftSeverity };
});

const planningChip = computed(() => {
  const ps = planningStatus.value;
  if (!ps) return null;
  return {
    completed: ps.completed,
    total: ps.total,
    driftSeverity: ps.driftSeverity,
    intent: ps.intent,
  };
});

const vm = computed(() =>
  buildDashboardViewModel({
    laneSnapshots: props.laneSnapshots,
    contextMetrics: props.contextMetrics,
    promptMessageCharsSessionCurrent: props.promptMessageCharsSessionCurrent,
    provenanceSummary: props.provenanceSummary ?? null,
    messages: props.messages,
    planningChip: planningChip.value,
    runnerOnline: props.runnerOnline,
  }),
);

const agentRows = computed(() => buildSystemCapacityRows(props.agents));

const { theme } = useTheme();
const diagramSources = computed(() => (props.provenanceDiagram ? [props.provenanceDiagram] : []));
const { rendered: renderedDiagrams } = useMermaidRenderer(diagramSources, theme);
const expandedDiagram = ref(false);

const diagramSvg = computed(() => {
  const r = renderedDiagrams.value[0];
  if (!r || r.error) return null;
  return r.svg;
});

const diagramHasError = computed(() => !!renderedDiagrams.value[0]?.error);

function onSelectLane(tabId: string) {
  emit("go-chat", { tabId });
}

function onOpenChat() {
  emit("go-chat", {});
}

function onAttentionAct(payload: { provenanceTab?: ProvenancePaneTab; goChatOnly?: boolean }) {
  emit("go-chat", { provenanceTab: payload.provenanceTab });
}

function onDrillProvenance(tab: ProvenancePaneTab) {
  emit("go-chat", { provenanceTab: tab });
}
</script>

<template>
  <div class="dashboard dashboard-narrative">
    <DashboardRuntimeSection
      :lanes="vm.lanes"
      :other-lane-count="vm.otherLaneCount"
      :planning-chip="vm.planningChip"
      :hero-open-lanes="vm.hero.openLanes"
      :provenance-health-pct="vm.hero.provenanceHealthPct"
      :provenance-ops-total="vm.hero.provenanceOpsTotal"
      :provenance-failed="vm.hero.provenanceFailed"
      :last-provenance-update-ms="vm.hero.lastProvenanceUpdateMs"
      @select-lane="onSelectLane"
      @open-chat="onOpenChat"
    />

    <DashboardAttentionSection :items="vm.attention" @act="onAttentionAct" />

    <DashboardCausalSection
      :causal-lines="vm.causalLines"
      :context-metrics="contextMetrics"
      :session-strip="vm.sessionStrip"
      :hotspots="vm.hotspots"
      :diagram-svg="diagramSvg"
      :diagram-has-error="diagramHasError"
      @expand-diagram="expandedDiagram = true"
      @drill="onDrillProvenance"
    />

    <DashboardSystemSection
      :agent-rows="agentRows"
      :runner-online="runnerOnline"
      @open-settings="emit('open-settings')"
    />

    <Teleport to="body">
      <div
        v-if="expandedDiagram && diagramSvg"
        class="diagram-modal-overlay"
        tabindex="-1"
        @click.self="expandedDiagram = false"
        @keydown.escape="expandedDiagram = false"
      >
        <div class="diagram-modal" role="dialog" aria-modal="true" aria-label="Provenance trace preview">
          <header class="diagram-modal-header">
            <span class="diagram-modal-title">Trace preview</span>
            <button
              class="diagram-modal-btn diagram-modal-close"
              title="Close"
              type="button"
              @click="expandedDiagram = false"
            >
              <svg
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              >
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </header>
          <!-- eslint-disable-next-line vue/no-v-html -->
          <div class="diagram-modal-body" v-html="diagramSvg"></div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

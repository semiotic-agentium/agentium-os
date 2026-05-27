<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useTheme } from "../composables/useTheme";
import { useToast } from "../composables/useToast";
import { useMermaidRenderer } from "../composables/useMermaidRenderer";
import { useProvenanceOps } from "../composables/useProvenanceOps";
import {
  provenanceQueryFromScope,
  useObservationScope,
} from "../composables/useObservationScope";
import type {
  ContextPlanningResponse,
  ContextPlanningTaskSnapshot,
  ProvenanceQueryParams,
} from "../types/provenance";
import ProvenanceLiveTab from "./provenance/ProvenanceLiveTab.vue";
import type { HotspotDrilldownParams } from "./provenance/ProvenanceLiveTab.vue";
import ProvenanceFailuresTab from "./provenance/ProvenanceFailuresTab.vue";
import type { DrilldownFromFailure } from "./provenance/ProvenanceFailuresTab.vue";
import ProvenanceAnomaliesTab from "./provenance/ProvenanceAnomaliesTab.vue";
import type { DrilldownFromAnomaly } from "./provenance/ProvenanceAnomaliesTab.vue";
import ProvenanceDriftTab from "./provenance/ProvenanceDriftTab.vue";
import ExploreTab from "./provenance/ExploreTab.vue";
import type { DrilldownParams } from "./provenance/ExploreTab.vue";
import type { LlmPromptOperation } from "../types/a2a";
import RunStatusIndicator from "./RunStatusIndicator.vue";
import { IDLE_RUN_STATUS, type OperatorRunStatus } from "../operator/runStatus";

const TRACES_STORAGE_KEY = "agentium:showTraces";

function readTracesPreference(): boolean | null {
  if (typeof window === "undefined") return null;
  try {
    const stored = localStorage.getItem(TRACES_STORAGE_KEY);
    if (stored === "0") return false;
    if (stored === "1") return true;
  } catch {
    return null;
  }
  return null;
}

function defaultPaneOpen(preferOpen?: boolean): boolean {
  if (preferOpen === true) return true;
  const stored = readTracesPreference();
  if (stored !== null) return stored;
  return typeof window !== "undefined" ? window.innerWidth >= 1280 : true;
}

const props = defineProps<{
  contextId?: string;
  taskId?: string;
  selectedAgentId?: string;
  /** Filter ops queries by agent package (Event Console compose agent). */
  selectedAgentPackage?: string;
  runStatus?: OperatorRunStatus;
  /** @deprecated Use runStatus.active */
  isStreaming?: boolean;
  diagrams?: string[];
  /** Bumps when evented provenance signals new rows; edge-triggers Live refresh */
  traceRefreshTick?: number;
  llmPromptOperations?: LlmPromptOperation[];
  /** Dashboard / deep-link: bump `nonce` to switch tabs and expand the pane */
  externalTabFocus?: { nonce: number; tab: "live" | "failures" | "anomalies" | "drift" | "explore" };
  /** Initial open state; Event Console passes false until a context is observed. */
  defaultOpen?: boolean;
  /** When true, expand the pane (e.g. after publish). */
  preferOpen?: boolean;
  /** Chat vs Event Console empty-state copy. */
  surface?: "chat" | "event";
}>();

function initialPaneOpen(): boolean {
  if (props.defaultOpen === true) return true;
  if (props.defaultOpen === false) return false;
  return defaultPaneOpen(props.preferOpen);
}

const isOpen = ref(initialPaneOpen());

const resolvedRunStatus = computed(() => props.runStatus ?? IDLE_RUN_STATUS);

const traceActive = computed(
  () => props.runStatus?.active ?? props.isStreaming ?? false,
);

const emptyStateCopy = computed(() => {
  if (props.surface === "event") {
    return "Publish an event or select an event run to load context-scoped traces.";
  }
  return "Start a chat turn to attach context-scoped provenance.";
});

const activeTab = ref<"live" | "failures" | "anomalies" | "drift" | "explore">("live");

const { theme } = useTheme();
const toast = useToast();
const sources = computed(() => props.diagrams ?? []);
const { rendered } = useMermaidRenderer(sources, theme);
const expandedIdx = ref<number | null>(null);

const { createQuery } = useProvenanceOps();

// ── Query instances ────────────────────────────────────────────────────────

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

const exploreTabRef = ref<InstanceType<typeof ExploreTab> | null>(null);

// ── Polling & planning ─────────────────────────────────────────────────────

const isExploreTab = computed(() => activeTab.value === "explore");
const pollInFlight = ref(false);
let pollPending = false;
const planningState = ref<{
  loading: boolean;
  error: string | null;
  response: ContextPlanningResponse | null;
}>({
  loading: false,
  error: null,
  response: null,
});

const observationScope = useObservationScope(
  () => props.contextId,
  () => props.taskId,
  () => props.selectedAgentPackage,
);

function baseScope(): Pick<
  ProvenanceQueryParams,
  "contextId" | "taskId" | "agentId" | "agentPackage"
> {
  return provenanceQueryFromScope(observationScope.value, props.selectedAgentId);
}

async function refreshForActiveTab() {
  if (!props.contextId) return;
  if (pollInFlight.value) {
    pollPending = true;
    return;
  }
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
      await anomalyQuery.run({
        ...scope,
        outcome: "both",
      });
      return;
    }
    if (activeTab.value === "drift") {
      await refreshPlanning();
    }
  } finally {
    pollInFlight.value = false;
    if (pollPending) {
      pollPending = false;
      void refreshForActiveTab();
    }
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

// ── Derived data for child components ──────────────────────────────────────

const planningTasks = computed<ContextPlanningTaskSnapshot[]>(() => {
  return planningState.value.response?.tasks ?? [];
});


// ── Drilldown handlers ─────────────────────────────────────────────────────

function switchToExploreWithDrilldown(params: DrilldownParams) {
  activeTab.value = "explore";
  setTimeout(() => {
    exploreTabRef.value?.applyDrilldown(params);
  }, 0);
}

function onHotspotDrilldown(params: HotspotDrilldownParams) {
  switchToExploreWithDrilldown({
    resource: params.kind === "llm" ? "llm_calls" : "tool_calls",
    model: params.model || undefined,
    toolName: params.toolName || undefined,
    agentId: params.agentId || undefined,
    outcome: params.outcome,
    sortBy: params.sortBy,
    sortDir: params.sortDir,
  });
}

function onFailureDrilldown(params: DrilldownFromFailure) {
  switchToExploreWithDrilldown({
    resource: params.resource,
    outcome: params.outcome,
    provider: params.provider || undefined,
    model: params.model || undefined,
    toolName: params.toolName || undefined,
    bamlPrompt: params.bamlPrompt || undefined,
    agentId: params.agentId || undefined,
    sortBy: params.sortBy,
    sortDir: params.sortDir,
  });
}

function onAnomalyDrilldown(params: DrilldownFromAnomaly) {
  switchToExploreWithDrilldown({
    resource: params.resource,
    provider: params.provider || undefined,
    model: params.model || undefined,
    bamlPrompt: params.bamlPrompt || undefined,
    agentId: params.agentId || undefined,
    outcome: params.outcome,
    sortBy: params.sortBy,
    sortDir: params.sortDir,
  });
}

function onDrillToDriftCalls(taskId: string) {
  switchToExploreWithDrilldown({
    resource: "llm_calls",
    outcome: "both",
    taskId,
    sortBy: "timestamp_ms",
    sortDir: "desc",
  });
}

// ── Episode download ───────────────────────────────────────────────────────

async function downloadEpisodeText(taskId: string) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 15_000);
  let response: Response;
  try {
    response = await fetch(`/tasks/${encodeURIComponent(taskId)}/episode/text`, {
      signal: controller.signal,
    });
  } catch (error) {
    if ((error as Error).name === "AbortError") {
      toast.error("Episode download timed out — the task graph may still be populating.");
    } else {
      toast.error("Episode download failed — check the runner and try again.");
    }
    return;
  } finally {
    clearTimeout(timer);
  }
  if (!response.ok) {
    if (response.status === 404) {
      toast.error(
        "No episode transcript yet for this task — wait for dispatch to finish, then retry.",
      );
    } else {
      toast.error(`Episode download failed (${response.status}).`);
    }
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

// ── Diagram modal ──────────────────────────────────────────────────────────

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

// ── Watchers & lifecycle ───────────────────────────────────────────────────

watch(
  () => props.preferOpen,
  (open) => {
    if (open) isOpen.value = true;
  },
);

watch(
  () => props.externalTabFocus?.nonce,
  (nonce) => {
    if (nonce == null) return;
    const tab = props.externalTabFocus?.tab;
    if (!tab) return;
    activeTab.value = tab;
    isOpen.value = true;
  },
);

watch(
  () => [props.contextId, props.taskId, props.selectedAgentId],
  () => {
    if (!props.contextId || isExploreTab.value) return;
    void refreshForActiveTab();
  },
  { immediate: true },
);

watch(
  () => props.traceRefreshTick ?? 0,
  (tick, prev) => {
    if (tick === prev) return;
    void refreshForActiveTab();
  },
);

watch(
  () => traceActive.value,
  (active) => {
    if (!active || !props.contextId || isExploreTab.value) return;
    void refreshForActiveTab();
  },
);

watch(
  () => [activeTab.value] as const,
  ([tab]) => {
    if (!props.contextId || tab === "explore") return;
    void refreshForActiveTab();
  },
  { immediate: true },
);
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
        <RunStatusIndicator variant="compact" :status="resolvedRunStatus" />
      </header>

      <div class="provenance-tabs">
        <button class="provenance-tab" :class="{ active: activeTab === 'live' }" @click="activeTab = 'live'">Live</button>
        <button class="provenance-tab" :class="{ active: activeTab === 'failures' }" @click="activeTab = 'failures'">Failures</button>
        <button class="provenance-tab" :class="{ active: activeTab === 'anomalies' }" @click="activeTab = 'anomalies'">Anomalies</button>
        <button
          class="provenance-tab"
          :class="{ active: activeTab === 'drift' }"
          @click="activeTab = 'drift'"
        >
          Drift
        </button>
        <button class="provenance-tab" :class="{ active: activeTab === 'explore' }" @click="activeTab = 'explore'">Explore</button>
      </div>

      <div class="provenance-body">
        <div v-if="!props.contextId" class="provenance-empty">
          {{ emptyStateCopy }}
        </div>

        <template v-else-if="activeTab === 'live'">
          <ProvenanceLiveTab
            :live-llm-response="liveLlm.state.value.response"
            :live-tool-response="liveTool.state.value.response"
            :planning-tasks="planningTasks"
            :planning-loading="planningState.loading"
            :planning-error="planningState.error"
            :rendered="rendered"
            :task-id="props.taskId"
            :is-streaming="traceActive"
            :all-task-ids="planningState.response?.allTaskIds ?? []"
            :llm-prompt-operations="props.llmPromptOperations"
            @hotspot-drilldown="onHotspotDrilldown"
            @open-modal="openModal"
            @download-episode-text="downloadEpisodeText"
          />
        </template>

        <template v-else-if="activeTab === 'failures'">
          <ProvenanceFailuresTab
            :failed-llm-response="failedLlmQuery.state.value.response"
            :failed-tool-response="failedToolQuery.state.value.response"
            @drilldown="onFailureDrilldown"
          />
        </template>

        <template v-else-if="activeTab === 'anomalies'">
          <ProvenanceAnomaliesTab
            :anomaly-response="anomalyQuery.state.value.response"
            @drilldown="onAnomalyDrilldown"
          />
        </template>

        <template v-else-if="activeTab === 'drift'">
          <ProvenanceDriftTab
            :planning-tasks="planningTasks"
            @drill-to-drift-calls="onDrillToDriftCalls"
          />
        </template>

        <template v-else>
          <ExploreTab
            ref="exploreTabRef"
            :context-id="props.contextId"
            :selected-agent-id="props.selectedAgentId"
          />
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

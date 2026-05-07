<script setup lang="ts">
import { computed } from "vue";
import {
  formatCompact,
  formatDuration,
  shortId,
  groupValueAt,
  asDisplayIdentity,
} from "../../utils/format";
import {
  driftSeverityClass,
  driftSeverityLabel,
  formatDriftScore,
  planningTaskTitle,
  planningIntentLabel,
  planningPlanLabel,
  planningStepLabel,
  planningStatusLabel,
  planProgressPercent,
  stepStatusClass,
  taskHasDrift,
  taskKindLabel,
} from "../../utils/provenanceHelpers";
import type {
  ContextPlanningTaskSnapshot,
  ProvenanceQueryResponse,
} from "../../types/provenance";
import type { LlmPromptOperation } from "../../types/a2a";

const props = defineProps<{
  liveLlmResponse: ProvenanceQueryResponse | null;
  liveToolResponse: ProvenanceQueryResponse | null;
  planningTasks: ContextPlanningTaskSnapshot[];
  planningLoading: boolean;
  planningError: string | null;
  rendered: Array<{ svg: string; error: string | null }>;
  taskId?: string;
  isStreaming: boolean;
  /** allTaskIds from the planning response, used to build episodeTaskIds. */
  allTaskIds: string[];
  /** Prompt JSON telemetry (moved from Primary transcript column). */
  llmPromptOperations?: LlmPromptOperation[];
}>();

const emit = defineEmits<{
  (e: "hotspotDrilldown", params: HotspotDrilldownParams): void;
  (e: "openModal", index: number): void;
  (e: "downloadEpisodeText", taskId: string): void;
}>();

export type HotspotDrilldownParams = {
  kind: "llm" | "tool";
  model: string;
  toolName: string;
  agentId: string;
  sortBy: string;
  sortDir: "asc" | "desc";
  outcome: "both";
};

// ── Aggregate cards ────────────────────────────────────────────────────────

type AggregateCard = {
  label: string;
  count: number;
  failed: number;
  durationMs: number;
  tokenValue?: number;
  tokenIn?: number;
  tokenCached?: number;
  tokenOut?: number;
  tokenLabel?: string;
};

const aggregateCards = computed<AggregateCard[]>(() => [
  {
    label: "LLM Calls",
    count: props.liveLlmResponse?.summary.count ?? 0,
    failed: props.liveLlmResponse?.summary.failedCount ?? 0,
    durationMs: props.liveLlmResponse?.summary.durationMsTotal ?? 0,
    tokenValue: props.liveLlmResponse?.summary.totalTokens ?? 0,
    tokenIn: props.liveLlmResponse?.summary.promptTokensTotal ?? 0,
    tokenCached: props.liveLlmResponse?.summary.cachedInputTokensTotal ?? 0,
    tokenOut: props.liveLlmResponse?.summary.completionTokensTotal ?? 0,
    tokenLabel: "in/cached/out/total",
  },
  {
    label: "Tool Calls",
    count: props.liveToolResponse?.summary.count ?? 0,
    failed: props.liveToolResponse?.summary.failedCount ?? 0,
    durationMs: props.liveToolResponse?.summary.durationMsTotal ?? 0,
  },
]);

// ── Trace snapshot ─────────────────────────────────────────────────────────

const traceSnapshot = computed(() => {
  const llmCount = props.liveLlmResponse?.summary.count ?? 0;
  const toolCount = props.liveToolResponse?.summary.count ?? 0;
  const totalTokens = props.liveLlmResponse?.summary.totalTokens ?? 0;
  const totalDurationMs =
    (props.liveLlmResponse?.summary.durationMsTotal ?? 0) +
    (props.liveToolResponse?.summary.durationMsTotal ?? 0);
  return {
    llmCount,
    toolCount,
    totalTokens,
    totalDurationMs,
    taskCount: props.planningTasks.length,
  };
});

// ── Trace dimension summaries ───────────────────────────────────────────────

function uniqueNonEmpty(values: Array<string | undefined>): string[] {
  return [...new Set(values.filter((value): value is string => typeof value === "string" && value.length > 0))];
}

// Hotspot group-key positions: 0 = agentId, 1 = package, 2 = version, 3 = model (LLM) or tool name (Tool)
const HOTSPOT_PKG_IDX = 1;
const HOTSPOT_DIM_IDX = 3;

const traceAgentPackages = computed(() =>
  uniqueNonEmpty([
    ...(props.liveLlmResponse?.hotspotGroups ?? []).map((group) => groupValueAt(group.groupValues, group.groupKey, HOTSPOT_PKG_IDX)),
    ...(props.liveToolResponse?.hotspotGroups ?? []).map((group) => groupValueAt(group.groupValues, group.groupKey, HOTSPOT_PKG_IDX)),
  ]),
);

const traceModels = computed(() =>
  uniqueNonEmpty(
    (props.liveLlmResponse?.hotspotGroups ?? []).map((group) => groupValueAt(group.groupValues, group.groupKey, HOTSPOT_DIM_IDX)),
  ),
);

const traceTools = computed(() =>
  uniqueNonEmpty(
    (props.liveToolResponse?.hotspotGroups ?? []).map((group) => groupValueAt(group.groupValues, group.groupKey, HOTSPOT_DIM_IDX)),
  ),
);

const episodeTaskIds = computed<string[]>(() => {
  const ids: string[] = [];
  const seen = new Set<string>();
  if (props.taskId) {
    ids.push(props.taskId);
    seen.add(props.taskId);
  }
  for (const tid of props.allTaskIds) {
    if (!seen.has(tid)) {
      ids.push(tid);
      seen.add(tid);
    }
  }
  for (const t of props.planningTasks) {
    if (!seen.has(t.taskId)) {
      ids.push(t.taskId);
      seen.add(t.taskId);
    }
  }
  return ids;
});

// ── Hotspot groups ─────────────────────────────────────────────────────────

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
  const llm = (props.liveLlmResponse?.hotspotGroups ?? []).map((group) => ({
    kind: "llm" as const,
    groupKey: group.groupKey,
    groupValues: group.groupValues,
    count: group.count,
    failureRate: group.failureRate,
    avgDurationMs: group.avgDurationMs,
    avgTotalTokens: group.avgTotalTokens,
  }));
  const tool = (props.liveToolResponse?.hotspotGroups ?? []).map((group) => ({
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

function liveHotspotLabel(item: LiveHotspotItem): string {
  const agentIdRaw = groupValueAt(item.groupValues, item.groupKey, 0);
  const pkgRaw = groupValueAt(item.groupValues, item.groupKey, 1);
  const verRaw = groupValueAt(item.groupValues, item.groupKey, 2);
  const dimRaw = groupValueAt(item.groupValues, item.groupKey, HOTSPOT_DIM_IDX);
  const agentDisplay = asDisplayIdentity(agentIdRaw, pkgRaw, verRaw);
  if (item.kind === "llm") {
    const model = dimRaw ?? "unknown-model";
    return `LLM · ${agentDisplay} · ${model}`;
  }
  const tool = dimRaw ?? "unknown-tool";
  return `Tool · ${agentDisplay} · ${tool}`;
}

function sortLlmPromptOperations(a: LlmPromptOperation, b: LlmPromptOperation): number {
  return a.eventOrder - b.eventOrder || a.activityAnchor.localeCompare(b.activityAnchor);
}

const promptOpsForDisplay = computed(() =>
  [...(props.llmPromptOperations ?? [])]
    .filter(
      (op): op is LlmPromptOperation =>
        !!op &&
        typeof op.activityAnchor === "string" &&
        op.activityAnchor.length > 0 &&
        typeof op.eventOrder === "number" &&
        Number.isFinite(op.eventOrder),
    )
    .sort(sortLlmPromptOperations),
);

/** Temporal tail: last prompt op after sorting by event order (then anchor). */
const latestPromptOp = computed(() => {
  const ops = promptOpsForDisplay.value;
  return ops.length > 0 ? ops[ops.length - 1]! : null;
});

function applyLiveHotspotDrilldown(item: LiveHotspotItem) {
  const agentId = groupValueAt(item.groupValues, item.groupKey, 0);
  const dim = groupValueAt(item.groupValues, item.groupKey, HOTSPOT_DIM_IDX);
  emit("hotspotDrilldown", {
    kind: item.kind,
    model: item.kind === "llm" ? (dim ?? "") : "",
    toolName: item.kind === "tool" ? (dim ?? "") : "",
    agentId: agentId ?? "",
    sortBy: "duration_ms",
    sortDir: "desc",
    outcome: "both",
  });
}

</script>

<template>
  <div
    v-if="latestPromptOp"
    class="prompt-ops-telemetry"
    aria-label="Latest LLM prompt character count"
  >
    <span class="prompt-ops-label">Prompt</span>
    <span
      class="prompt-op-chip"
      :title="`${latestPromptOp.activityAnchor} · event order ${latestPromptOp.eventOrder}`"
    >
      <span class="prompt-op-order">#{{ latestPromptOp.eventOrder }}</span>
      <span class="prompt-op-kb">
        {{ formatCompact(latestPromptOp.promptMessageCharsCurrent ?? 0) }} chars
      </span>
      <span class="prompt-op-anchor">{{ shortId(latestPromptOp.activityAnchor) }}</span>
    </span>
  </div>
  <div class="provenance-card-grid" role="region" aria-label="Live aggregate counts">
    <article v-for="card in aggregateCards" :key="card.label" class="provenance-card">
      <div class="provenance-card-label">{{ card.label }}</div>
      <div class="provenance-card-value">{{ formatCompact(card.count) }}</div>
      <div class="provenance-card-sub">
        failed {{ card.failed }} · {{ formatDuration(card.durationMs) }}<template
          v-if="card.tokenValue !== undefined"
        >
          · {{ card.tokenLabel }}
          <template
            v-if="card.tokenIn !== undefined && card.tokenOut !== undefined && card.tokenCached !== undefined"
          >
            {{ formatCompact(card.tokenIn) }}/{{ formatCompact(card.tokenCached) }}/{{ formatCompact(card.tokenOut) }}/{{ formatCompact(card.tokenValue) }}
          </template>
          <template v-else-if="card.tokenIn !== undefined && card.tokenOut !== undefined">
            {{ formatCompact(card.tokenIn) }}/{{ formatCompact(card.tokenOut) }}/{{ formatCompact(card.tokenValue) }}
          </template>
          <template v-else>
            {{ formatCompact(card.tokenValue) }}
          </template>
        </template>
      </div>
    </article>
  </div>

  <section class="provenance-section" role="region" aria-label="Intent and plan progress">
    <div class="provenance-section-title">Intent / Plan Progress</div>
    <div v-if="planningLoading && planningTasks.length === 0" class="provenance-empty">
      Loading planning state...
    </div>
    <div v-else-if="planningError" class="provenance-error">
      {{ planningError }}
    </div>
    <div v-else-if="planningTasks.length === 0" class="provenance-empty">
      No planning records captured for this context yet.
    </div>
    <ul v-else class="provenance-list">
      <li v-for="task in planningTasks" :key="task.taskId" class="provenance-list-item">
        <div class="group-key">{{ planningTaskTitle(task) }}</div>
        <div class="planning-labeled-row">
          <span class="planning-row-label">Intent</span>
          <span class="planning-row-value">{{ planningIntentLabel(task) }}</span>
        </div>
        <div class="planning-labeled-row">
          <span class="planning-row-label">Plan</span>
          <span class="planning-row-value">{{ planningPlanLabel(task) }}</span>
        </div>
        <div class="planning-labeled-row" v-if="task.currentPlan?.plan_id">
          <span class="planning-row-label">Plan ID</span>
          <span class="planning-row-value">{{ task.currentPlan?.plan_id }}</span>
        </div>
        <div class="planning-metrics-row">
          <span>{{ task.stepSummary.completed }}/{{ task.stepSummary.total }} complete</span>
          <span>{{ planProgressPercent(task) }}%</span>
        </div>
        <div class="planning-progress-track">
          <div
            class="planning-progress-fill"
            :style="{ transform: `scaleX(${planProgressPercent(task) / 100})` }"
          />
        </div>

        <template v-if="taskHasDrift(task)">
          <div class="planning-labeled-row">
            <span class="planning-row-label">Drift</span>
            <span class="planning-row-value">
              <span :class="['planning-step-pill', driftSeverityClass(task.drift?.compositeSeverity)]">
                {{ formatDriftScore(task.drift?.planAdherenceScore) }} {{ driftSeverityLabel(task.drift?.compositeSeverity) }}
              </span>
            </span>
          </div>
          <div class="planning-progress-track drift-gauge">
            <div
              class="planning-progress-fill"
              :class="driftSeverityClass(task.drift?.compositeSeverity)"
              :style="{ transform: `scaleX(${task.drift?.planAdherenceScore ?? 0})` }"
            />
          </div>
        </template>

        <ul
          v-if="task.currentPlan && task.currentPlan.steps && task.currentPlan.steps.length > 0"
          class="planning-step-list"
        >
          <li
            v-for="step in task.currentPlan.steps"
            :key="`${task.taskId}:${step.step_id}`"
            class="planning-step-row"
          >
            <span :class="['planning-step-pill', stepStatusClass(step.status)]">
              {{ planningStatusLabel(step.status) }}
            </span>
            <span class="planning-step-desc">{{ planningStepLabel(step) }}</span>
          </li>
        </ul>
      </li>
    </ul>
  </section>

  <section class="provenance-section" role="region" aria-label="Live trace mermaid diagram">
    <div class="provenance-section-title">Execution Trace</div>
    <div class="trace-summary-card">
      <div class="trace-summary-eyebrow">Execution Record</div>
      <p class="trace-summary-lede">
        This trace is rendered from the persisted provenance graph for this answer, not from a mock flow or a separate logging layer.
      </p>

      <div class="trace-summary-metrics">
        <div class="trace-summary-stat">
          <span class="trace-summary-stat-value">{{ traceSnapshot.taskCount }}</span>
          <span class="trace-summary-stat-label">task<span v-if="traceSnapshot.taskCount !== 1">s</span></span>
        </div>
        <div class="trace-summary-stat">
          <span class="trace-summary-stat-value">{{ traceSnapshot.llmCount }}</span>
          <span class="trace-summary-stat-label">LLM calls</span>
        </div>
        <div class="trace-summary-stat">
          <span class="trace-summary-stat-value">{{ traceSnapshot.toolCount }}</span>
          <span class="trace-summary-stat-label">tool calls</span>
        </div>
        <div class="trace-summary-stat">
          <span class="trace-summary-stat-value">{{ formatDuration(traceSnapshot.totalDurationMs) }}</span>
          <span class="trace-summary-stat-label">runtime</span>
        </div>
        <div class="trace-summary-stat">
          <span class="trace-summary-stat-value">{{ formatCompact(traceSnapshot.totalTokens) }}</span>
          <span class="trace-summary-stat-label">tokens</span>
        </div>
      </div>

      <div class="trace-summary-groups">
        <div class="trace-summary-group">
          <span class="trace-summary-group-label">Actors</span>
          <div class="trace-summary-chips">
            <span class="trace-summary-chip">User</span>
            <span
              v-for="agentPackage in traceAgentPackages"
              :key="`trace-agent-${agentPackage}`"
              class="trace-summary-chip"
            >
              {{ agentPackage }}
            </span>
          </div>
        </div>

        <div v-if="traceModels.length > 0" class="trace-summary-group">
          <span class="trace-summary-group-label">Models</span>
          <div class="trace-summary-chips">
            <span
              v-for="model in traceModels"
              :key="`trace-model-${model}`"
              class="trace-summary-chip trace-summary-chip-accent"
            >
              {{ model }}
            </span>
          </div>
        </div>

        <div v-if="traceTools.length > 0" class="trace-summary-group">
          <span class="trace-summary-group-label">Tools</span>
          <div class="trace-summary-chips">
            <span
              v-for="tool in traceTools"
              :key="`trace-tool-${tool}`"
              class="trace-summary-chip trace-summary-chip-muted"
            >
              {{ tool }}
            </span>
          </div>
        </div>

        <div class="trace-summary-group trace-summary-transcripts">
          <span class="trace-summary-group-label">Transcripts</span>
          <p v-if="episodeTaskIds.length > 1" class="trace-summary-transcripts-hint">
            {{ episodeTaskIds.length }} tasks in this context — download one per task.
          </p>
          <div v-if="planningLoading && episodeTaskIds.length === 0" class="trace-summary-transcripts-empty">
            Loading tasks...
          </div>
          <div v-else-if="episodeTaskIds.length === 0" class="trace-summary-transcripts-empty">
            Complete a chat turn to generate episode transcripts for this context.
          </div>
          <ul v-else class="trace-summary-transcript-list" role="list">
            <li
              v-for="tid in episodeTaskIds"
              :key="`transcript-${tid}`"
              class="trace-summary-transcript-row"
            >
              <div class="trace-summary-transcript-meta">
                <span class="trace-summary-transcript-kind">{{ taskKindLabel(tid) }}</span>
                <span class="trace-summary-transcript-id">{{ shortId(tid) }}</span>
                <span
                  v-if="tid === taskId && isStreaming"
                  class="trace-summary-transcript-live"
                  title="Active chat task"
                >&#9679;</span>
              </div>
              <button
                type="button"
                class="trace-summary-transcript-download"
                title="Download episode as plain-text transcript"
                @click="emit('downloadEpisodeText', tid)"
              >
                Download
              </button>
            </li>
          </ul>
          <p class="trace-summary-transcripts-footnote">
            Generated on demand from the provenance graph; download after the task completes for full detail.
          </p>
        </div>
      </div>
    </div>
    <div v-if="rendered.length === 0" class="provenance-empty">
      The execution trace will appear here after the first reply.
    </div>
    <div v-else class="reasoning-diagrams" aria-live="polite">
      <div
        v-for="(item, i) in rendered.slice(0, 1)"
        :key="i"
        class="diagram-card"
        :class="{ clickable: !item.error }"
        @click="!item.error && emit('openModal', i)"
        :title="item.error ? undefined : 'Click to expand'"
      >
        <div v-if="item.error" class="diagram-error">
          <span class="diagram-error-label">Render error</span>
          <pre>{{ item.error }}</pre>
        </div>
        <template v-else>
          <div class="trace-diagram-caption">
            Exact event order for this run. Click to expand.
          </div>
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

  <section class="provenance-section" role="region" aria-label="Live hotspot groups">
    <div class="provenance-section-title">Hotspot Groups</div>
    <div
      v-if="liveHotspotItems.length === 0"
      class="provenance-empty"
    >
      No hotspot groups yet.
    </div>
    <ul v-else class="provenance-list" aria-live="polite">
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

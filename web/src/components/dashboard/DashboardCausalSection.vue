<script setup lang="ts">
import { computed } from "vue";
import type { ContextMetricsResponse } from "../../types/a2a";
import type { ProvenanceGroupHotspot } from "../../types/provenance";
import type { CausalStoryLine, ProvenancePaneTab } from "../../composables/useDashboardViewModel";
import {
  formatCompact as formatTokenCount,
  formatDuration,
  normalizeGroupValue,
  asDisplayIdentity,
} from "../../utils/format";

const props = defineProps<{
  causalLines: CausalStoryLine[];
  contextMetrics: ContextMetricsResponse | null;
  sessionStrip: {
    tokensTotal: number | null;
    llmCalls: number | null;
    avgLatencyMs: number | null;
    promptChars: string | null;
    turns: number | null;
  } | null;
  hotspots: ProvenanceGroupHotspot[];
  diagramSvg: string | null;
  diagramHasError: boolean;
}>();

const emit = defineEmits<{
  "expand-diagram": [];
  drill: [tab: ProvenancePaneTab];
}>();

const SPARK_W = 320;
const SPARK_H = 56;

const turnTokens = computed(() => {
  if (!props.contextMetrics) return [];
  return props.contextMetrics.turns.map((t) => t.tokens.total);
});

const sparkPoints = computed(() => {
  const data = turnTokens.value;
  if (data.length < 2) return "";
  const max = Math.max(...data);
  const min = Math.min(...data);
  const range = max - min || 1;
  const pad = 4;
  return data
    .map((v, i) => {
      const x = ((i / (data.length - 1)) * SPARK_W).toFixed(1);
      const y = (pad + ((max - v) / range) * (SPARK_H - pad * 2)).toFixed(1);
      return `${x},${y}`;
    })
    .join(" ");
});

const sparkFillPath = computed(() => {
  if (!sparkPoints.value) return "";
  const coords = sparkPoints.value.split(" ");
  return `M ${coords.join(" L ")} L ${SPARK_W},${SPARK_H} L 0,${SPARK_H} Z`;
});

function groupDimensionValue(group: ProvenanceGroupHotspot, dimension: string): string | undefined {
  const dimensions = Array.isArray(group.groupDimensions) ? group.groupDimensions : [];
  const values = Array.isArray(group.groupValues) ? group.groupValues : [];
  const idx = dimensions.indexOf(dimension);
  if (idx >= 0) return normalizeGroupValue(values[idx]);
  const legacyValues = group.groupKey.split("|");
  const legacyIdx =
    dimension === "agent_id"
      ? 0
      : dimension === "agent_package"
        ? 1
        : dimension === "agent_version"
          ? 2
          : dimension === "model" || dimension === "tool_name"
            ? 3
            : -1;
  if (legacyIdx >= 0) return normalizeGroupValue(legacyValues[legacyIdx]);
  return undefined;
}

function hotspotLabel(group: ProvenanceGroupHotspot): string {
  const agentDisplay = asDisplayIdentity(
    groupDimensionValue(group, "agent_id"),
    groupDimensionValue(group, "agent_package"),
    groupDimensionValue(group, "agent_version"),
  );
  const model = groupDimensionValue(group, "model");
  const toolName = groupDimensionValue(group, "tool_name");
  if (model) return `${agentDisplay} · ${model}`;
  if (toolName) return `${agentDisplay} · ${toolName}`;
  return agentDisplay;
}

</script>

<template>
  <section class="dashboard-narrative-section" aria-labelledby="dash-causal-heading">
    <div class="dashboard-narrative-head">
      <h2 id="dash-causal-heading" class="dashboard-narrative-title">Causal story</h2>
      <p class="dashboard-narrative-lede">
        Focused lane transcript tail, session shape, and trace drill-in. Mermaid is secondary — open Traces for the full graph.
      </p>
    </div>

    <div class="dashboard-causal-grid">
      <div class="dashboard-card dashboard-causal-card">
        <div class="dashboard-card-header">Transcript tail</div>
        <div class="dashboard-causal-transcript">
          <template v-if="causalLines.length > 0">
            <div v-for="(line, i) in causalLines" :key="i" class="dashboard-transcript-line">
              <span class="dashboard-transcript-role">{{ line.role }}</span>
              <span class="dashboard-transcript-text">{{ line.text }}</span>
            </div>
          </template>
          <div v-else class="dashboard-empty-well">
            <strong>No messages yet</strong>
            <span>Open Chat and send a turn — the story fills from the active lane.</span>
          </div>
        </div>
        <div class="dashboard-causal-actions">
          <button type="button" class="btn-linkish" @click="emit('drill', 'live')">Open Live traces</button>
          <button type="button" class="btn-linkish" @click="emit('drill', 'explore')">Explore graph</button>
        </div>
      </div>

      <div class="dashboard-card dashboard-causal-card">
        <div class="dashboard-card-header">Session shape</div>
        <div class="sparkline-card-body">
          <template v-if="contextMetrics">
            <div class="dashboard-metrics-strip" v-if="sessionStrip">
              <span v-if="sessionStrip.turns != null"
                ><strong>{{ sessionStrip.turns }}</strong> turns</span
              >
              <span v-if="sessionStrip.llmCalls != null"
                ><strong>{{ sessionStrip.llmCalls }}</strong> LLM calls</span
              >
              <span v-if="sessionStrip.avgLatencyMs != null"
                ><strong>{{ sessionStrip.avgLatencyMs }}</strong> ms avg</span
              >
              <span v-if="sessionStrip.tokensTotal != null"
                ><strong>{{ formatTokenCount(sessionStrip.tokensTotal) }}</strong> tokens</span
              >
              <span v-if="sessionStrip.promptChars"
                ><strong>{{ sessionStrip.promptChars }}</strong> prompt</span
              >
            </div>

            <div v-if="turnTokens.length >= 2">
              <div class="stat-card-label" style="margin-bottom: 6px">Tokens per turn</div>
              <svg
                class="sparkline-svg"
                :viewBox="`0 0 ${SPARK_W} ${SPARK_H}`"
                preserveAspectRatio="none"
                role="img"
                :aria-label="`Token usage per turn across ${turnTokens.length} turns`"
              >
                <defs>
                  <linearGradient id="dashSparkGrad" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stop-color="var(--primary)" stop-opacity="0.3" />
                    <stop offset="100%" stop-color="var(--primary)" stop-opacity="0" />
                  </linearGradient>
                </defs>
                <path :d="sparkFillPath" fill="url(#dashSparkGrad)" />
                <polyline
                  :points="sparkPoints"
                  fill="none"
                  stroke="var(--primary)"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            </div>
            <div v-else class="dashboard-muted-note">Need ≥2 turns for a sparkline.</div>

            <div v-if="hotspots.length > 0" class="dashboard-hotspots">
              <div class="stat-card-label">Hotspots</div>
              <ul>
                <li v-for="group in hotspots" :key="group.groupKey">
                  <span class="group-key">{{ hotspotLabel(group) }}</span>
                  <span>{{ group.count }} · {{ formatDuration(Math.round(group.avgDurationMs)) }}</span>
                </li>
              </ul>
              <button type="button" class="btn-linkish" @click="emit('drill', 'anomalies')">
                View anomalies tab
              </button>
            </div>
          </template>
          <div v-else class="dashboard-empty-well">
            <strong>No session metrics</strong>
            <span>Metrics bind once this lane has a context with telemetry.</span>
          </div>
        </div>
      </div>

      <div class="dashboard-card dashboard-causal-card">
        <div class="dashboard-card-header">Trace preview</div>
        <div class="sparkline-card-body">
          <template v-if="diagramSvg && !diagramHasError">
            <div class="provenance-miniature" title="Expand trace preview" @click="emit('expand-diagram')">
              <!-- eslint-disable-next-line vue/no-v-html -->
              <div class="diagram-svg" v-html="diagramSvg"></div>
              <div class="diagram-expand-hint" aria-hidden="true">⛶</div>
            </div>
            <button type="button" class="btn-linkish" @click="emit('drill', 'live')">
              Open trace pane
            </button>
          </template>
          <div v-else class="dashboard-empty-well">
            <strong>No diagram yet</strong>
            <span>Graph renders after provenance emits a Mermaid block for this context.</span>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

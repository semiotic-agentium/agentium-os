<script setup lang="ts">
import { computed } from "vue";
import { formatCompact, formatDuration, groupValueAt, asDisplayIdentity } from "../../utils/format";
import type { ProvenanceGroupHotspot, ProvenanceQueryResponse } from "../../types/provenance";

const props = defineProps<{
  anomalyResponse: ProvenanceQueryResponse | null;
}>();

const emit = defineEmits<{
  (e: "drilldown", params: DrilldownFromAnomaly): void;
}>();

export type DrilldownFromAnomaly = {
  resource: "llm_calls";
  provider: string;
  model: string;
  bamlPrompt: string;
  agentId: string;
  outcome: "both";
  sortBy: string;
  sortDir: "asc" | "desc";
};

const anomalyCards = computed(() => {
  const groups = props.anomalyResponse?.hotspotGroups ?? [];
  return groups
    .slice()
    .sort((a, b) => {
      if (b.failureRate !== a.failureRate) return b.failureRate - a.failureRate;
      if (b.avgDurationMs !== a.avgDurationMs) return b.avgDurationMs - a.avgDurationMs;
      return b.avgTotalTokens - a.avgTotalTokens;
    })
    .slice(0, 12);
});

function anomalyLabel(anomaly: ProvenanceGroupHotspot): string {
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

function applyAnomalyDrilldown(anomaly: ProvenanceGroupHotspot) {
  const agentId = groupValueAt(anomaly.groupValues, anomaly.groupKey, 0);
  const provider = groupValueAt(anomaly.groupValues, anomaly.groupKey, 3);
  const model = groupValueAt(anomaly.groupValues, anomaly.groupKey, 4);
  const bamlPrompt = groupValueAt(anomaly.groupValues, anomaly.groupKey, 5);
  emit("drilldown", {
    resource: "llm_calls",
    provider: provider ?? "",
    model: model ?? "",
    bamlPrompt: bamlPrompt ?? "",
    agentId: agentId ?? "",
    outcome: "both",
    sortBy: "duration_ms",
    sortDir: "desc",
  });
}
</script>

<template>
  <div class="provenance-section-title">Top Anomalous Groups</div>
  <div v-if="anomalyCards.length === 0" class="provenance-empty">
    No anomalies detected.
  </div>
  <div v-else class="anomaly-grid" role="region" aria-label="Anomaly cards" aria-live="polite">
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

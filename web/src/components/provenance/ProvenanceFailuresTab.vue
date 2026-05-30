<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed } from "vue";
import { formatDuration, asDisplayIdentity } from "../../utils/format";
import type { ProvenanceRowBase, ProvenanceQueryResponse } from "../../types/provenance";

const props = defineProps<{
  failedLlmResponse: ProvenanceQueryResponse | null;
  failedToolResponse: ProvenanceQueryResponse | null;
}>();

const emit = defineEmits<{
  (e: "drilldown", params: DrilldownFromFailure): void;
}>();

export type DrilldownFromFailure = {
  resource: "llm_calls" | "tool_calls";
  outcome: "failed_only";
  provider: string;
  model: string;
  toolName: string;
  bamlPrompt: string;
  sortBy: string;
  sortDir: "asc" | "desc";
  agentId: string;
};

type FailureRow = {
  kind: "llm" | "tool";
  row: ProvenanceRowBase;
};

const failureRows = computed<FailureRow[]>(() => {
  const llm = (props.failedLlmResponse?.rows ?? []).map((row) => ({
    kind: "llm" as const,
    row,
  }));
  const tool = (props.failedToolResponse?.rows ?? []).map((row) => ({
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
  llm: props.failedLlmResponse?.summary.count ?? 0,
  tool: props.failedToolResponse?.summary.count ?? 0,
}));

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
  emit("drilldown", {
    resource: item.kind === "llm" ? "llm_calls" : "tool_calls",
    outcome: "failed_only",
    sortBy: "duration_ms",
    sortDir: "desc",
    provider: typeof row.provider === "string" ? row.provider : "",
    model: typeof row.model === "string" ? row.model : "",
    toolName: typeof row.tool_name === "string" ? row.tool_name : "",
    bamlPrompt: typeof row.baml_prompt === "string" ? row.baml_prompt : "",
    agentId: typeof row.agent_id === "string" ? row.agent_id : "",
  });
}
</script>

<template>
  <div class="provenance-section-title">
    Failed Activities · LLM {{ failureSummary.llm }} · Tool {{ failureSummary.tool }}
  </div>
  <div v-if="failureRows.length === 0" class="provenance-empty">
    No failed activities in this context.
  </div>
  <div v-else class="anomaly-grid" role="region" aria-label="Failure cards" aria-live="polite">
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

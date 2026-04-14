<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useProvenanceOps } from "../../composables/useProvenanceOps";
import type {
  ProvenanceOutcome,
  ProvenanceQueryParams,
  ProvenanceRowBase,
  ProvenanceResource,
} from "../../types/provenance";
import {
  formatCellValue,
  formatColumnHeader,
} from "../../utils/provenanceHelpers";
import ProvenanceRowInspector from "./ProvenanceRowInspector.vue";

const props = defineProps<{
  contextId?: string;
  selectedAgentId?: string;
}>();

const { createQuery } = useProvenanceOps();

const exploreQuery = createQuery("messages", {
  pageSize: 25,
  sortBy: "timestamp_ms",
  sortDir: "desc",
});

const exploreForm = ref<{
  resource: ProvenanceResource;
  outcome: ProvenanceOutcome;
  taskId: string;
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
  taskId: "",
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

function baseScope(): Pick<ProvenanceQueryParams, "contextId" | "agentId"> {
  return {
    contextId: props.contextId,
    agentId: props.selectedAgentId,
  };
}

const exploreRows = computed(() => exploreQuery.state.value.response?.rows ?? []);
const exploreColumns = computed(() => {
  if (exploreRows.value.length === 0) return [];
  const keys = Array.from(new Set(exploreRows.value.flatMap((row) => Object.keys(row))));
  const preferred = [
    "baml_prompt", "model", "agent_display", "duration_ms", "total_tokens",
    "activity_outcome", "drift", "failure_class", "failure_evidence", "tool_name",
    "provider", "timestamp_ms", "task_id", "activity_kind", "activity_id",
    "context_id", "agent_package", "agent_version", "agent_id", "message_id",
    "cached_input_tokens",
  ];
  const hidden = new Set([
    "llm_call", "llm_result", "llm_result_raw", "llm_call_ref",
    "llm_result_ref", "activity_ref", "llm_call_payload_id",
  ]);
  const ordered = preferred.filter((k) => keys.includes(k));
  const extra = keys.filter((k) => !ordered.includes(k) && !hidden.has(k));
  return [...ordered, ...extra].slice(0, 12);
});

const activeFilterChips = computed(() => {
  const chips: Array<{ key: string; label: string }> = [];
  if (exploreForm.value.taskId) {
    const short = exploreForm.value.taskId.length > 24
      ? exploreForm.value.taskId.slice(0, 20) + "..."
      : exploreForm.value.taskId;
    chips.push({ key: "taskId", label: `task:${short}` });
  }
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

function applyExploreQuery(resetCursor = true) {
  exploreQuery.setResource(exploreForm.value.resource);
  const groupBy = exploreForm.value.groupBy
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  void exploreQuery.run({
    ...baseScope(),
    outcome: exploreForm.value.outcome,
    taskId: exploreForm.value.taskId || undefined,
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

function removeChip(key: string) {
  if (key === "taskId") exploreForm.value.taskId = "";
  if (key === "provider") exploreForm.value.provider = "";
  if (key === "model") exploreForm.value.model = "";
  if (key === "toolName") exploreForm.value.toolName = "";
  if (key === "bamlPrompt") exploreForm.value.bamlPrompt = "";
  if (key === "groupBy") exploreForm.value.groupBy = "";
  if (key === "outcome") exploreForm.value.outcome = "both";
  applyExploreQuery(true);
}

function clearAllFilters() {
  exploreForm.value.taskId = "";
  exploreForm.value.provider = "";
  exploreForm.value.model = "";
  exploreForm.value.toolName = "";
  exploreForm.value.bamlPrompt = "";
  exploreForm.value.groupBy = "";
  exploreForm.value.outcome = "both";
  applyExploreQuery(true);
}

function rowKey(row: ProvenanceRowBase, idx: number): string {
  const activityId = row.activity_id;
  if (typeof activityId === "string" && activityId.length > 0) return activityId;
  const activityKind = typeof row.activity_kind === "string" ? row.activity_kind : "unknown";
  const contextId = typeof row.context_id === "string" ? row.context_id : "unknown";
  const messageId = typeof row.message_id === "string" ? row.message_id : "unknown";
  return `${activityKind}:${contextId}:${messageId}:${idx}`;
}

function selectRow(row: ProvenanceRowBase) {
  selectedRow.value = row;
}

export interface DrilldownParams {
  resource?: ProvenanceResource;
  outcome?: ProvenanceOutcome;
  taskId?: string;
  provider?: string;
  model?: string;
  toolName?: string;
  bamlPrompt?: string;
  sortBy?: string;
  sortDir?: "asc" | "desc";
  agentId?: string;
}

function applyDrilldown(params: DrilldownParams) {
  if (params.resource) exploreForm.value.resource = params.resource;
  if (params.outcome !== undefined) exploreForm.value.outcome = params.outcome;
  if (params.taskId !== undefined) exploreForm.value.taskId = params.taskId;
  if (params.provider !== undefined) exploreForm.value.provider = params.provider;
  if (params.model !== undefined) exploreForm.value.model = params.model;
  if (params.toolName !== undefined) exploreForm.value.toolName = params.toolName;
  if (params.bamlPrompt !== undefined) exploreForm.value.bamlPrompt = params.bamlPrompt;
  if (params.sortBy) exploreForm.value.sortBy = params.sortBy;
  if (params.sortDir) exploreForm.value.sortDir = params.sortDir;
  if (params.agentId) exploreQuery.setParams({ agentId: params.agentId });
  applyExploreQuery(true);
}

watch(
  () => [props.contextId, props.selectedAgentId],
  () => {
    if (!props.contextId) return;
    applyExploreQuery(true);
  },
  { immediate: true },
);

defineExpose({ applyDrilldown, applyExploreQuery });
</script>

<template>
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
    <button
      class="action-btn"
      :disabled="exploreQuery.state.value.loading"
      @click="applyExploreQuery(true)"
    >
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
      {{ chip.label }} &times;
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

  <div class="explore-table-wrap" role="region" aria-label="Explore results table">
    <table class="explore-table">
      <thead>
        <tr>
          <th v-for="col in exploreColumns" :key="col">{{ formatColumnHeader(col) }}</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="(row, idx) in exploreRows"
          :key="rowKey(row, idx)"
          @click="selectRow(row)"
        >
          <td v-for="col in exploreColumns" :key="col">
            {{ formatCellValue(col, row[col]) }}
          </td>
        </tr>
      </tbody>
    </table>
  </div>

  <ProvenanceRowInspector
    v-if="selectedRow"
    :selected-row="selectedRow"
    :resource="exploreForm.resource"
    @close="selectedRow = null"
  />
</template>

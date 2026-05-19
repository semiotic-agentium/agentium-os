<script setup lang="ts">
import type { AttentionItem, ProvenancePaneTab } from "../../composables/useDashboardViewModel";

defineProps<{
  items: AttentionItem[];
}>();

const emit = defineEmits<{
  act: [payload: { provenanceTab?: ProvenancePaneTab; goChatOnly?: boolean }];
}>();

function onAct(item: AttentionItem) {
  emit("act", {
    provenanceTab: item.provenanceTab,
    goChatOnly: !item.provenanceTab,
  });
}
</script>

<template>
  <section class="dashboard-narrative-section" aria-labelledby="dash-attention-heading">
    <div class="dashboard-narrative-head">
      <h2 id="dash-attention-heading" class="dashboard-narrative-title">Attention needed</h2>
      <p class="dashboard-narrative-lede">
        Ranked issues linked to provenance. Jump opens Chat and focuses the trace tab when available.
      </p>
    </div>

    <ul v-if="items.length > 0" class="dashboard-attention-list">
      <li
        v-for="(item, idx) in items"
        :key="idx"
        class="dashboard-attention-row"
        :data-severity="item.severity"
      >
        <div class="dashboard-attention-copy">
          <div class="dashboard-attention-title">{{ item.title }}</div>
          <div v-if="item.detail" class="dashboard-attention-detail">{{ item.detail }}</div>
        </div>
        <button type="button" class="btn-attention" @click="onAct(item)">
          {{ item.actionLabel }}
        </button>
      </li>
    </ul>

    <div v-else class="dashboard-empty-well">
      <strong>All clear</strong>
      <span>No ranked anomalies for the current scope. Open Chat to generate provenance if this workspace is quiet.</span>
    </div>
  </section>
</template>

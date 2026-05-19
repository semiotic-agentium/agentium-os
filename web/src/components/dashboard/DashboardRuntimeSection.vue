<script setup lang="ts">
import type { RuntimeLane } from "../../composables/useDashboardViewModel";
import { formatRelativeProvenanceAge } from "./dashboardFormatters";

defineProps<{
  lanes: RuntimeLane[];
  otherLaneCount: number;
  planningChip: {
    completed: number;
    total: number;
    driftSeverity: string | null;
    intent: string | null;
  } | null;
  heroOpenLanes: number;
  provenanceHealthPct: number | null;
  provenanceOpsTotal: number;
  provenanceFailed: number;
  lastProvenanceUpdateMs: number | null;
}>();

const emit = defineEmits<{
  "select-lane": [tabId: string];
  "open-chat": [];
}>();

function statusLabel(s: RuntimeLane["status"]): string {
  if (s === "streaming") return "Streaming";
  if (s === "active") return "Active";
  return "Idle";
}
</script>

<template>
  <section class="dashboard-narrative-section" aria-labelledby="dash-runtime-heading">
    <div class="dashboard-narrative-head">
      <h2 id="dash-runtime-heading" class="dashboard-narrative-title">Runtime now</h2>
      <p class="dashboard-narrative-lede">
        Concurrent chat lanes across tabs. The focused lane drives metrics and provenance scope — not the whole cluster.
      </p>
    </div>

    <div class="dashboard-hero-row">
      <div class="dashboard-hero-card">
        <div class="dashboard-hero-label">Open lanes</div>
        <div class="dashboard-hero-value">{{ heroOpenLanes }}</div>
        <div class="dashboard-hero-sub">Chat tabs</div>
      </div>
      <div class="dashboard-hero-card">
        <div class="dashboard-hero-label">Prov. health</div>
        <div
          class="dashboard-hero-value"
          :class="{ 'dashboard-hero-value--ok': provenanceHealthPct === 100 }"
        >
          {{ provenanceHealthPct !== null ? `${provenanceHealthPct}%` : "—" }}
        </div>
        <div class="dashboard-hero-sub">
          <template v-if="provenanceOpsTotal > 0">
            {{ provenanceOpsTotal }} ops · {{ provenanceFailed }} failed
          </template>
          <template v-else>No provenance yet</template>
        </div>
      </div>
      <div class="dashboard-hero-card">
        <div class="dashboard-hero-label">Provenance refresh</div>
        <div class="dashboard-hero-value dashboard-hero-value--sm">
          {{
            lastProvenanceUpdateMs
              ? formatRelativeProvenanceAge(lastProvenanceUpdateMs)
              : "—"
          }}
        </div>
        <div class="dashboard-hero-sub">From ops queries (not page load)</div>
      </div>
    </div>

    <div v-if="planningChip" class="dashboard-plan-chip" role="status">
      <span class="dashboard-plan-chip-label">Planning (context)</span>
      <span class="dashboard-plan-chip-stat"
        >{{ planningChip.completed }}/{{ planningChip.total }} steps</span
      >
      <span v-if="planningChip.driftSeverity" :class="['drift-pill', `drift-${planningChip.driftSeverity}`]">
        Drift {{ planningChip.driftSeverity }}
      </span>
      <span v-if="planningChip.intent" class="dashboard-plan-chip-intent">{{
        planningChip.intent
      }}</span>
    </div>

    <p v-if="otherLaneCount > 0" class="dashboard-concurrency-hint">
      {{ otherLaneCount }} other lane{{ otherLaneCount === 1 ? "" : "s" }} with activity — switch tabs in Chat to refocus.
    </p>

    <ul class="dashboard-lane-list">
      <li v-for="lane in lanes" :key="lane.tabId" class="dashboard-lane-row">
        <button
          type="button"
          class="dashboard-lane-hit"
          :class="{ 'dashboard-lane-hit--focused': lane.isFocused }"
          @click="emit('select-lane', lane.tabId)"
        >
          <span class="dashboard-lane-title">{{ lane.title }}</span>
          <span class="dashboard-lane-status" :data-status="lane.status">{{ statusLabel(lane.status) }}</span>
          <span class="dashboard-lane-detail">{{ lane.detail }}</span>
        </button>
      </li>
    </ul>

    <div class="dashboard-runtime-actions">
      <button type="button" class="btn-primary-soft" @click="emit('open-chat')">
        Open Chat
      </button>
    </div>
  </section>
</template>

<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useConfigApi } from "../composables/useConfigApi";
import type { SemioticActivityDto, SemioticAgentActivityDto } from "../types/config";
import {
  formatIncidentTime,
  incidentSeverityClass,
} from "../chat/semioticPolicyUi";
import { preventionRatioLabel } from "../utils/gateHelpers";
import { navigateToGateIncident } from "../events/operatorRoute";

const props = defineProps<{
  /** UI-only substring filter for incident cards; does not refetch the API. */
  searchFilter?: string | null;
}>();

const { fetchSemioticActivity } = useConfigApi();
const activity = ref<SemioticActivityDto | null>(null);
const error = ref<string | null>(null);
const loading = ref(false);

async function load() {
  loading.value = true;
  error.value = null;
  const result = await fetchSemioticActivity({
    windowHours: 24,
    limit: 20,
  });
  loading.value = false;
  if ("error" in result) {
    error.value = result.error.detail ?? result.error.title;
    activity.value = null;
    return;
  }
  activity.value = result.data;
}

onMounted(load);

const fleetAlert = computed(
  () => (activity.value?.fleet.frictionDenialCount ?? 0) > 0,
);

const visibleAgents = computed((): SemioticAgentActivityDto[] => {
  const agents = activity.value?.agents ?? [];
  const q = props.searchFilter?.trim().toLowerCase();
  if (!q) return agents;
  return agents.filter((a) => a.agentPackage.toLowerCase().includes(q));
});

function openIncident(agentPackage: string, incident: SemioticAgentActivityDto["recentIncidents"][number]) {
  navigateToGateIncident(incident.drill, agentPackage);
}
</script>

<template>
  <div class="config-routing-block semiotic-activity-panel">
    <div class="config-routing-block-header">
      <div>
        <h3 class="config-section-title">Recent gate activity</h3>
        <p class="config-hint">Last 24h from provenance tool_calls (current policy shown).</p>
      </div>
      <button type="button" class="btn btn--ghost btn--sm" :disabled="loading" @click="load">
        Refresh
      </button>
    </div>

    <div v-if="error" class="settings-error" role="alert">{{ error }}</div>
    <p v-else-if="loading && !activity" class="semiotic-activity-empty">Loading activity…</p>
    <template v-else-if="activity">
      <div
        :class="['semiotic-activity-fleet', { 'semiotic-activity-fleet--alert': fleetAlert }]"
      >
        <span>{{ activity.fleet.denyCount }} deny</span>
        <span>{{ activity.fleet.askCount }} ask</span>
        <span>{{ activity.fleet.frictionDenialCount }} friction</span>
        <span>{{ activity.fleet.preventedErrorCount }} prevented</span>
        <span>Prevention {{ preventionRatioLabel(activity.fleet.preventionRatio) }}</span>
        <span>{{ activity.fleet.agentsWithActivity }} agents with activity</span>
      </div>
      <p v-if="activity.emptyReason" class="semiotic-activity-empty">{{ activity.emptyReason }}</p>

      <div v-for="agent in visibleAgents" :key="agent.agentPackage" class="config-agent-override-card">
        <div class="config-agent-override-header">
          <span class="config-agent-override-name">{{ agent.agentPackage }}</span>
          <span class="config-agent-summary">{{ agent.effective.summary }}</span>
        </div>
        <div v-if="agent.recentIncidents.length === 0" class="semiotic-activity-empty">
          No incidents in window.
        </div>
        <table v-else class="semiotic-incident-table">
          <thead>
            <tr>
              <th>Time</th>
              <th>Tool</th>
              <th>Tier</th>
              <th>Decision</th>
              <th>Reason</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="(incident, idx) in agent.recentIncidents"
              :key="`${agent.agentPackage}:${idx}`"
              :class="incidentSeverityClass(incident.severity)"
            >
              <td>{{ formatIncidentTime(incident.occurredAtMs) }}</td>
              <td>{{ incident.toolName }}</td>
              <td>{{ incident.tier }}</td>
              <td>{{ incident.decision }}</td>
              <td>{{ incident.reasonCode || "—" }}</td>
              <td>
                <button
                  type="button"
                  class="btn btn--ghost btn--sm semiotic-incident-open"
                  @click="openIncident(agent.agentPackage, incident)"
                >
                  Open in Provenance
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </div>
</template>

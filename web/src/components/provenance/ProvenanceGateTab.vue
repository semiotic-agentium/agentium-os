<!--
SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.

SPDX-License-Identifier: Apache-2.0
-->

<script setup lang="ts">
import type { ContextPlanningTaskSnapshot } from "../../types/provenance";
import { computed } from "vue";
import {
  gateDecisionClass,
  gateDecisionLabel,
  gateHelp,
  gatePostureClass,
  gateReasonLabel,
  planningIntentLabel,
  planningTaskTitle,
  preventionRatioLabel,
  taskGatePosture,
  taskHasGateActivity,
} from "../../utils/gateHelpers";

const props = defineProps<{
  planningTasks: ContextPlanningTaskSnapshot[];
}>();

const gateTasks = computed(() => props.planningTasks.filter(taskHasGateActivity));

const emit = defineEmits<{
  (e: "drillToGateCalls", taskId: string): void;
}>();
</script>

<template>
  <div class="provenance-section-title gate-section-title">
    Semiotic gate
    <span class="gate-help gate-tab-help" :data-tooltip="gateHelp.gateTab">&#9432;</span>
  </div>
  <p class="gate-tab-lede">
    Structural grounding before action — pass / deny / ask / pass_gated by tier. Does not catch deliberate misparse.
  </p>
  <div v-if="planningTasks.length === 0" class="provenance-empty">
    No planning data available. Gate summaries require committed tasks when semiotic is enabled.
  </div>
  <div v-else-if="gateTasks.length === 0" class="provenance-empty">
    No gate evaluations yet (tier&ge;1 tool calls when semiotic enabled).
  </div>
  <div v-else class="gate-task-list">
    <article
      v-for="task in gateTasks"
      :key="`gate:${task.taskId}`"
      class="gate-task-card"
    >
      <div class="gate-task-header">
        <span class="group-key">{{ planningTaskTitle(task) }}</span>
        <button
          :class="['planning-step-pill', 'gate-count-link', gatePostureClass(taskGatePosture(task))]"
          @click="emit('drillToGateCalls', task.taskId)"
          title="View gated tool calls in Explore"
        >
          {{ taskGatePosture(task) }}
        </button>
      </div>
      <div class="gate-task-intent">{{ planningIntentLabel(task) }}</div>

      <div class="gate-call-summary">
        <button
          v-if="(task.gate?.denyCount ?? 0) > 0"
          class="gate-count-link gate-deny-count"
          @click="emit('drillToGateCalls', task.taskId)"
        >{{ task.gate?.denyCount }} deny</button>
        <button
          v-if="(task.gate?.askCount ?? 0) > 0"
          class="gate-count-link gate-ask-count"
          @click="emit('drillToGateCalls', task.taskId)"
        >{{ task.gate?.askCount }} ask</button>
        <button
          v-if="(task.gate?.passGatedCount ?? 0) > 0"
          class="gate-count-link gate-gated-count"
          @click="emit('drillToGateCalls', task.taskId)"
        >{{ task.gate?.passGatedCount }} pass_gated</button>
        <span v-if="(task.gate?.passCount ?? 0) > 0" class="gate-pass-count">
          {{ task.gate?.passCount }} pass
        </span>
      </div>

      <div class="gate-bar-row">
        <span class="gate-bar-label gate-help" :data-tooltip="gateHelp.preventionRatio">Prevention</span>
        <div class="gate-bar-track">
          <div
            class="gate-bar-fill"
            :style="{ transform: `scaleX(${task.gate?.preventionRatio ?? 0})` }"
          />
        </div>
        <span class="gate-bar-value">{{ preventionRatioLabel(task.gate?.preventionRatio) }}</span>
      </div>

      <div
        v-if="task.gate?.gateEvents && task.gate.gateEvents.length > 0"
        class="gate-event-list"
      >
        <div
          v-for="(ev, i) in task.gate.gateEvents"
          :key="`ev:${task.taskId}:${i}`"
          class="gate-event-card"
        >
          <div class="gate-event-header">
            <span :class="['planning-step-pill', gateDecisionClass(ev.decision)]">
              {{ gateDecisionLabel(ev.decision) }}
            </span>
            <span class="gate-event-tool">T{{ ev.tier }} · {{ ev.toolName }}</span>
          </div>
          <div class="gate-event-reason">{{ gateReasonLabel(ev.reasonCode) }}</div>
          <div v-if="ev.deficientNodes.length > 0" class="gate-deficit-list">
            <span
              v-for="node in ev.deficientNodes"
              :key="node"
              class="gate-deficit-chip"
            >{{ node }}</span>
          </div>
        </div>
      </div>
    </article>
  </div>
</template>

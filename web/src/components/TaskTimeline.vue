<script setup lang="ts">
import type { StateTransition } from "../types/a2a";

defineProps<{ transitions: StateTransition[] }>();

function stateLabel(state: string): string {
  switch (state) {
    case "TASK_STATE_SUBMITTED": return "Submitted";
    case "TASK_STATE_WORKING": return "Working";
    case "TASK_STATE_INPUT_REQUIRED": return "Input";
    case "TASK_STATE_COMPLETED": return "Done";
    case "TASK_STATE_FAILED": return "Failed";
    case "TASK_STATE_CANCELED": return "Canceled";
    default: return state.replace("TASK_STATE_", "");
  }
}

function stateColor(state: string): string {
  switch (state) {
    case "TASK_STATE_SUBMITTED": return "state-submitted";
    case "TASK_STATE_WORKING": return "state-working";
    case "TASK_STATE_INPUT_REQUIRED": return "state-input";
    case "TASK_STATE_COMPLETED": return "state-completed";
    case "TASK_STATE_FAILED":
    case "TASK_STATE_CANCELED": return "state-failed";
    default: return "state-submitted";
  }
}

function formatTime(date: Date): string {
  return new Date(date).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}
</script>

<template>
  <div v-if="transitions.length > 0" class="task-timeline">
    <template v-for="(t, idx) in transitions" :key="idx">
      <div v-if="idx > 0" class="timeline-connector" />
      <div class="timeline-step">
        <div :class="['timeline-dot', stateColor(t.state)]" />
        <span class="timeline-label">{{ stateLabel(t.state) }}</span>
        <span class="timeline-time">{{ formatTime(t.timestamp) }}</span>
      </div>
    </template>
  </div>
</template>

<style scoped>
.task-timeline {
  display: flex;
  align-items: center;
  gap: 0;
  margin: 6px 0 2px;
}

.timeline-step {
  display: flex;
  flex-direction: column;
  align-items: center;
  min-width: 48px;
  gap: 2px;
}

.timeline-connector {
  width: 16px;
  height: 2px;
  background: var(--border);
  margin-bottom: 20px;
  flex-shrink: 0;
}

.timeline-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.timeline-dot.state-submitted {
  background: var(--text-muted);
}

.timeline-dot.state-working {
  background: var(--primary);
}

.timeline-dot.state-input {
  background: var(--color-warning);
}

.timeline-dot.state-completed {
  background: var(--status-green);
}

.timeline-dot.state-failed {
  background: var(--color-error);
}

.timeline-label {
  font-size: 10px;
  color: var(--text-muted);
  white-space: nowrap;
}

.timeline-time {
  font-size: 10px;
  color: var(--text-muted);
  white-space: nowrap;
}
</style>

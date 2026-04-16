<script setup lang="ts">
import type { FsmStepState } from "../../chat/toolCardDisplay";

defineProps<{ steps: FsmStepState[] }>();
</script>

<template>
  <div class="fsm-progress" role="status" aria-label="FSM progress">
    <div
      v-for="(step, idx) in steps"
      :key="`${step.key}-${idx}`"
      class="fsm-step"
      :data-status="step.status"
    >
      <span class="fsm-dot" aria-hidden="true"></span>
      <span class="fsm-label">{{ step.label }}</span>
    </div>
  </div>
</template>

<style scoped>
.fsm-progress {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem 0.6rem;
  padding: 0.45rem 0.75rem;
  border-bottom: 1px solid var(--border-subtle);
  background: color-mix(in srgb, var(--primary-subtle) 35%, transparent);
}

.fsm-step {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
  font-size: 0.72rem;
  line-height: 1;
  text-transform: lowercase;
  color: var(--text-muted);
}

.fsm-dot {
  width: 6px;
  height: 6px;
  border-radius: 999px;
  background: var(--text-muted);
  opacity: 0.5;
}

.fsm-step[data-status="done"] {
  color: var(--text-secondary);
}

.fsm-step[data-status="done"] .fsm-dot {
  opacity: 0.95;
  background: var(--status-green);
}

.fsm-step[data-status="active"] {
  color: var(--primary);
  font-weight: 600;
}

.fsm-step[data-status="active"] .fsm-dot {
  opacity: 1;
  background: var(--primary);
  animation: pulse-dot 1.2s ease-in-out infinite;
}

@keyframes pulse-dot {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.35;
  }
}
</style>

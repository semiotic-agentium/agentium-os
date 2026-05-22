<script setup lang="ts">
import { computed } from "vue";
import type { EventDispatchPhase } from "../../types/events";
import { isEventDispatchInFlight } from "../../events/dispatchPhases";

const props = defineProps<{
  phase: EventDispatchPhase;
}>();

const PHASES: { key: EventDispatchPhase; label: string }[] = [
  { key: "validating", label: "Validate" },
  { key: "publishing", label: "Publish" },
  { key: "recording", label: "Transcript" },
  { key: "live", label: "Live" },
];

const ORDER: Partial<Record<EventDispatchPhase, number>> = {
  idle: -1,
  validating: 0,
  publishing: 1,
  recording: 2,
  live: 3,
  empty: 3,
  failed: -2,
};

function phaseStatus(key: EventDispatchPhase): "done" | "active" | "pending" | "failed" {
  if (props.phase === "failed") {
    if (key === "validating") return "failed";
    return "pending";
  }
  const current = ORDER[props.phase] ?? -1;
  const target = ORDER[key] ?? -1;
  if (target < 0) return "pending";
  if (target < current) return "done";
  if (target === current) return "active";
  return "pending";
}

const visible = computed(
  () => isEventDispatchInFlight(props.phase) || props.phase === "failed",
);
</script>

<template>
  <div v-if="visible" class="event-dispatch-progress" role="status" aria-live="polite">
    <div class="dispatch-phases">
      <template v-for="(phase, idx) in PHASES" :key="phase.key">
        <div
          v-if="idx > 0"
          class="dispatch-phase-connector"
          :class="{ done: phaseStatus(phase.key) !== 'pending' }"
        ></div>
        <div class="dispatch-phase">
          <div :class="['dispatch-phase-dot', phaseStatus(phase.key)]">
            <svg
              v-if="phaseStatus(phase.key) === 'done'"
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="3"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <polyline points="20 6 9 17 4 12" />
            </svg>
          </div>
          <span class="dispatch-phase-label">{{ phase.label }}</span>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.event-dispatch-progress {
  padding: 0.35rem 0;
}

.dispatch-phases {
  display: flex;
  align-items: center;
  gap: 0;
  flex-wrap: wrap;
}

.dispatch-phase {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 0.2rem;
  min-width: 3.5rem;
}

.dispatch-phase-dot {
  width: 1.25rem;
  height: 1.25rem;
  border-radius: 50%;
  border: 2px solid var(--border);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 0.65rem;
}

.dispatch-phase-dot.done {
  border-color: var(--color-success);
  background: var(--color-success-subtle);
  color: var(--color-success);
}

.dispatch-phase-dot.active {
  border-color: var(--color-accent);
  background: color-mix(in srgb, var(--color-accent) 18%, var(--surface));
}

.dispatch-phase-dot.failed {
  border-color: var(--color-error);
  background: var(--color-error-subtle);
}

.dispatch-phase-dot.pending {
  opacity: 0.45;
}

.dispatch-phase-dot svg {
  width: 0.7rem;
  height: 0.7rem;
}

.dispatch-phase-connector {
  width: 1.25rem;
  height: 2px;
  background: var(--border);
  margin-bottom: 1rem;
}

.dispatch-phase-connector.done {
  background: var(--color-success);
}

.dispatch-phase-label {
  font-size: 0.625rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-muted);
}
</style>

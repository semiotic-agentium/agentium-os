<script setup lang="ts">
import { computed } from "vue";
import type { WorkflowProgressState, WorkflowPhaseName } from "../types/a2a";

const props = defineProps<{ progress: WorkflowProgressState }>();

const PHASES: { key: WorkflowPhaseName; label: string }[] = [
  { key: "discovery", label: "Discovery" },
  { key: "planning", label: "Planning" },
  { key: "execution", label: "Execution" },
  { key: "synthesis", label: "Synthesis" },
];

const PHASE_ORDER: Record<WorkflowPhaseName, number> = {
  idle: -1,
  discovery: 0,
  planning: 1,
  execution: 2,
  synthesis: 3,
};

function phaseStatus(key: WorkflowPhaseName): "done" | "active" | "pending" {
  const current = PHASE_ORDER[props.progress.phase] ?? -1;
  const target = PHASE_ORDER[key] ?? -1;
  if (target < current) return "done";
  if (target === current) return "active";
  return "pending";
}

const showNodes = computed(
  () => props.progress.phase === "execution" && props.progress.nodes.length > 0,
);
</script>

<template>
  <div class="workflow-progress">
    <div class="workflow-phases">
      <template v-for="(phase, idx) in PHASES" :key="phase.key">
        <div
          v-if="idx > 0"
          class="workflow-phase-connector"
          :class="{ done: phaseStatus(phase.key) !== 'pending' }"
        ></div>
        <div class="workflow-phase">
          <div :class="['workflow-phase-dot', phaseStatus(phase.key)]">
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
          <span class="workflow-phase-label">
            {{ phase.label }}
            <span
              v-if="phase.key === 'planning' && progress.iteration && progress.iteration > 1"
              class="workflow-iteration-badge"
            >
              iter {{ progress.iteration }}
            </span>
          </span>
        </div>
      </template>
    </div>
    <div v-if="showNodes" class="workflow-nodes">
      <div v-for="node in progress.nodes" :key="node.name" class="workflow-node-card">
        <span :class="['workflow-node-dot', `node-${node.status}`]"></span>
        <span class="workflow-node-name">{{ node.name }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.workflow-progress {
  background: var(--surface);
  border-bottom: 1px solid var(--border);
  padding: 10px 16px;
}

.workflow-phases {
  display: flex;
  align-items: center;
  gap: 0;
}

.workflow-phase {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}

.workflow-phase-connector {
  flex: 1;
  height: 2px;
  background: var(--border);
  min-width: 20px;
  margin: 0 4px;
  margin-bottom: 18px;
}

.workflow-phase-connector.done {
  background: var(--status-green);
}

.workflow-phase-dot {
  width: 14px;
  height: 14px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.workflow-phase-dot.pending {
  border: 2px solid var(--border);
  background: transparent;
}

.workflow-phase-dot.active {
  background: var(--primary);
  border: 2px solid var(--primary);
  animation: pulse-dot 1.5s ease-in-out infinite;
}

.workflow-phase-dot.done {
  background: var(--status-green);
  border: 2px solid var(--status-green);
  color: #fff;
}

.workflow-phase-dot.done svg {
  width: 8px;
  height: 8px;
}

@keyframes pulse-dot {
  0%,
  100% {
    opacity: 1;
    transform: scale(1);
  }
  50% {
    opacity: 0.7;
    transform: scale(1.15);
  }
}

.workflow-phase-label {
  font-size: 11px;
  color: var(--text-secondary);
  white-space: nowrap;
}

.workflow-iteration-badge {
  font-size: 10px;
  color: var(--primary);
  font-weight: 600;
}

.workflow-nodes {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
  padding-left: 4px;
}

.workflow-node-card {
  display: flex;
  align-items: center;
  gap: 5px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  padding: 3px 10px;
  font-size: 11px;
  color: var(--text-secondary);
  background: var(--surface-raised);
}

.workflow-node-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  flex-shrink: 0;
}

.workflow-node-dot.node-pending {
  background: var(--text-muted);
}

.workflow-node-dot.node-running {
  background: var(--primary);
  animation: pulse-dot 1.5s ease-in-out infinite;
}

.workflow-node-dot.node-completed {
  background: var(--status-green);
}

.workflow-node-dot.node-failed {
  background: var(--color-error);
}

.workflow-node-name {
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>

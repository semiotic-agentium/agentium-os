<script setup lang="ts">
import { computed, ref, watch } from "vue";
import RunStatusIndicator from "../RunStatusIndicator.vue";
import type { EventPublishResponse } from "../../types/events";
import type { OperatorRunStatus } from "../../operator/runStatus";

const props = defineProps<{
  status: OperatorRunStatus;
  contextId: string | null;
  lastPublishOutcome: EventPublishResponse | null;
  publishError: string | null;
  transcriptShowsDispatchFailures?: boolean;
}>();

const dismissComplete = ref(false);

const visible = computed(
  () =>
    props.status.phase !== "idle" ||
    Boolean(props.contextId) ||
    Boolean(props.lastPublishOutcome) ||
    Boolean(props.publishError),
);

const hasFailure = computed(() => props.status.phase === "failed");

const showFailureDetails = computed(
  () =>
    hasFailure.value &&
    props.lastPublishOutcome?.failures.length &&
    !props.transcriptShowsDispatchFailures &&
    !props.publishError,
);

const displayStatus = computed((): OperatorRunStatus => {
  if (dismissComplete.value && props.status.phase === "complete") {
    return {
      ...props.status,
      detail: undefined,
      phase: "idle",
      label: "Observing",
      severity: "neutral",
      active: false,
    };
  }
  return props.status;
});

const showDismiss = computed(
  () => props.status.phase === "complete" && !dismissComplete.value && !props.publishError,
);

watch(
  () => props.lastPublishOutcome,
  () => {
    dismissComplete.value = false;
  },
);
</script>

<template>
  <RunStatusIndicator
    v-if="visible"
    variant="banner"
    :status="displayStatus"
    :context-id="contextId"
    :show-progress-bar="displayStatus.active"
    aria-label="Run status"
  >
    <template #actions>
      <button
        v-if="showDismiss"
        type="button"
        class="run-status-dismiss"
        @click="dismissComplete = true"
      >
        Dismiss
      </button>
    </template>
  </RunStatusIndicator>

  <details v-if="showFailureDetails" class="run-status-failures">
    <summary>Subscriber failures</summary>
    <ul class="failure-list">
      <li v-for="(f, i) in lastPublishOutcome!.failures" :key="i">
        <strong>{{ f.agent_package }}/{{ f.agent_instance_id }}</strong>: {{ f.detail }}
      </li>
    </ul>
  </details>
</template>

<style scoped>
.run-status-dismiss {
  font-size: 0.6875rem;
  padding: 0.125rem 0.4375rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
  margin-left: auto;
}

.run-status-dismiss:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}

.run-status-failures {
  margin: 0;
  padding: 0 12px 4px;
  font-size: 0.75rem;
  color: var(--text-secondary);
  background: var(--surface);
  border-bottom: 1px solid var(--border);
}

.run-status-failures summary {
  cursor: pointer;
  font-weight: 600;
}

.failure-list {
  margin: 0.25rem 0 0;
  padding-left: 1.1rem;
}
</style>

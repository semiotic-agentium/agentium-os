<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { EventDispatchPhase, EventPublishResponse } from "../../types/events";
import type { TraceObserveState } from "../../composables/useEventObservation";
import { isEventDispatchInFlight } from "../../events/dispatchPhases";
import {
  formatPublishAcceptanceSummary,
  publishHadNoEffectiveWork,
} from "../../events/publishOutcome";
import { transcriptPhaseLabel } from "../../events/eventTranscriptModel";

const props = defineProps<{
  dispatchPhase: EventDispatchPhase;
  hydrateState: TraceObserveState;
  contextId: string | null;
  lastPublishOutcome: EventPublishResponse | null;
  publishError: string | null;
  waitingForIngress: boolean;
  transcriptShowsDispatchFailures?: boolean;
}>();

const dismissSuccess = ref(false);

const visible = computed(
  () =>
    Boolean(props.contextId) ||
    Boolean(props.lastPublishOutcome) ||
    Boolean(props.publishError) ||
    isEventDispatchInFlight(props.dispatchPhase),
);

const phaseLabel = computed(() =>
  transcriptPhaseLabel(props.dispatchPhase, props.hydrateState),
);

const inFlight = computed(
  () => isEventDispatchInFlight(props.dispatchPhase) && !hasFailure.value,
);

const hasFailure = computed(() => {
  if (props.publishError) return true;
  const o = props.lastPublishOutcome;
  return Boolean(o && (o.failures.length > 0 || o.subscribers_accepted < o.subscribers_matched));
});

const showWaitingIngress = computed(
  () => props.waitingForIngress && !props.publishError && !hasFailure.value,
);

const fleetSummary = computed(() => {
  const o = props.lastPublishOutcome;
  if (!o) return null;
  return formatPublishAcceptanceSummary(o);
});

const showNoEffectiveWork = computed(
  () =>
    Boolean(props.lastPublishOutcome) &&
    publishHadNoEffectiveWork(props.lastPublishOutcome) &&
    !props.publishError &&
    !hasFailure.value &&
    !inFlight.value,
);

const showSuccess = computed(() => {
  if (dismissSuccess.value || props.publishError || hasFailure.value) return false;
  if (props.waitingForIngress || inFlight.value || showNoEffectiveWork.value) return false;
  const o = props.lastPublishOutcome;
  if (!o) return false;
  return o.subscribers_accepted > 0 && props.dispatchPhase === "live";
});

const stripSeverity = computed((): "neutral" | "progress" | "success" | "warning" | "error" => {
  if (hasFailure.value || props.publishError) return "error";
  if (showNoEffectiveWork.value) return "warning";
  if (showSuccess.value) return "success";
  if (inFlight.value || showWaitingIngress.value) return "progress";
  return "neutral";
});

watch(
  () => props.lastPublishOutcome,
  () => {
    dismissSuccess.value = false;
  },
);
</script>

<template>
  <div
    v-if="visible"
    class="event-run-status-strip"
    :class="`event-run-status-strip--${stripSeverity}`"
    role="region"
    aria-label="Run status"
  >
    <div class="event-run-status-strip__row">
      <div class="event-run-status-strip__primary">
        <span class="event-run-status-strip__phase">{{ phaseLabel }}</span>
        <span v-if="contextId" class="event-run-status-strip__context" translate="no">{{
          contextId
        }}</span>
      </div>

      <p v-if="publishError" class="event-run-status-strip__inline event-run-status-strip__inline--error">
        {{ publishError }}
      </p>

      <p
        v-else-if="hasFailure && lastPublishOutcome"
        class="event-run-status-strip__inline event-run-status-strip__inline--error"
      >
        {{ lastPublishOutcome.subscribers_accepted }} of
        {{ lastPublishOutcome.subscribers_matched }} accepted
        <span v-if="lastPublishOutcome.failures.length > 0">
          · {{ lastPublishOutcome.failures.length }} failed
        </span>
      </p>

      <p v-else-if="showNoEffectiveWork" class="event-run-status-strip__inline">
        No agent work ran — {{ fleetSummary }}
      </p>

      <p v-else-if="showWaitingIngress" class="event-run-status-strip__inline">
        Waiting for host ingress…
      </p>

      <div v-else-if="showSuccess" class="event-run-status-strip__success-row">
        <p class="event-run-status-strip__inline">{{ fleetSummary }}</p>
        <button type="button" class="event-run-status-strip__dismiss" @click="dismissSuccess = true">
          Dismiss
        </button>
      </div>
    </div>

    <div v-if="inFlight" class="event-run-status-strip__progress" aria-hidden="true">
      <span class="event-run-status-strip__progress-bar" />
    </div>

    <details
      v-if="hasFailure && lastPublishOutcome?.failures.length && !transcriptShowsDispatchFailures && !publishError"
      class="event-run-status-strip__details"
    >
      <summary>Subscriber failures</summary>
      <ul class="failure-list">
        <li v-for="(f, i) in lastPublishOutcome!.failures" :key="i">
          <strong>{{ f.agent_package }}/{{ f.agent_instance_id }}</strong>: {{ f.detail }}
        </li>
      </ul>
    </details>
  </div>
</template>

<style scoped>
.event-run-status-strip {
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  gap: 0.375rem;
  padding: 0.375rem 1rem;
  min-height: 2rem;
  border-bottom: 1px solid var(--border);
  font-size: 0.8125rem;
  background: var(--surface);
}

.event-run-status-strip__row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.375rem 0.75rem;
  min-height: 1.375rem;
}

.event-run-status-strip__primary {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.375rem 0.625rem;
}

.event-run-status-strip__phase {
  font-size: 0.6875rem;
  font-weight: 700;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--text-muted);
}

.event-run-status-strip__context {
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--text-secondary);
}

.event-run-status-strip__inline {
  margin: 0;
  font-size: 0.8125rem;
  line-height: 1.4;
  color: var(--text-secondary);
}

.event-run-status-strip__inline--error {
  color: var(--color-error);
}

.event-run-status-strip__progress {
  height: 2px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--border) 80%, transparent);
  overflow: hidden;
}

.event-run-status-strip__progress-bar {
  display: block;
  height: 100%;
  width: 35%;
  border-radius: inherit;
  background: var(--primary);
  animation: event-strip-progress 1.2s ease-in-out infinite alternate;
}

@keyframes event-strip-progress {
  from {
    transform: translateX(-10%);
    width: 28%;
  }
  to {
    transform: translateX(220%);
    width: 38%;
  }
}

@media (prefers-reduced-motion: reduce) {
  .event-run-status-strip__progress-bar {
    animation: none;
    width: 100%;
    opacity: 0.45;
  }
}

.event-run-status-strip--error {
  border-left: 3px solid var(--color-error);
}

.event-run-status-strip--success {
  border-left: 3px solid var(--color-success);
}

.event-run-status-strip--warning {
  border-left: 3px solid var(--color-warning);
}

.event-run-status-strip--progress {
  border-left: 3px solid var(--primary);
}

.event-run-status-strip__details {
  margin: 0;
  font-size: 0.75rem;
  color: var(--text-secondary);
}

.event-run-status-strip__details summary {
  cursor: pointer;
  font-weight: 600;
}

.failure-list {
  margin: 0.25rem 0 0;
  padding-left: 1.1rem;
}

.event-run-status-strip__success-row {
  display: inline-flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.5rem;
}

.event-run-status-strip__dismiss {
  font-size: 0.6875rem;
  padding: 0.125rem 0.4375rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
}

.event-run-status-strip__dismiss:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}
</style>

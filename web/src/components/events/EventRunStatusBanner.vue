<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { EventDispatchPhase, EventPublishResponse } from "../../types/events";
import { isEventDispatchInFlight } from "../../events/dispatchPhases";
import {
  formatPublishAcceptanceSummary,
  publishHadNoEffectiveWork,
} from "../../events/publishOutcome";
import EventDispatchProgress from "./EventDispatchProgress.vue";

const props = defineProps<{
  dispatchPhase: EventDispatchPhase;
  lastPublishOutcome: EventPublishResponse | null;
  publishError: string | null;
  waitingForIngress: boolean;
}>();

const dismissSuccess = ref(false);

const hasFailure = computed(() => {
  if (props.publishError) return true;
  const o = props.lastPublishOutcome;
  return Boolean(o && (o.failures.length > 0 || o.subscribers_accepted < o.subscribers_matched));
});

const showInFlight = computed(
  () => isEventDispatchInFlight(props.dispatchPhase) && !hasFailure.value,
);

const showWaitingIngress = computed(
  () => props.waitingForIngress && !props.publishError && !hasFailure.value,
);

const showSuccess = computed(() => {
  if (dismissSuccess.value || props.publishError || hasFailure.value) return false;
  if (props.waitingForIngress || showInFlight.value || showNoEffectiveWork.value) return false;
  const o = props.lastPublishOutcome;
  if (!o) return false;
  return o.subscribers_accepted > 0 && props.dispatchPhase === "live";
});

const fleetSummary = computed(() => {
  const o = props.lastPublishOutcome;
  if (!o) return null;
  return formatPublishAcceptanceSummary(o);
});

const publishNoEffectiveWork = computed(() =>
  publishHadNoEffectiveWork(props.lastPublishOutcome),
);

const showNoEffectiveWork = computed(
  () =>
    Boolean(props.lastPublishOutcome) &&
    publishNoEffectiveWork.value &&
    !props.publishError &&
    !hasFailure.value &&
    !showInFlight.value,
);

watch(
  () => props.lastPublishOutcome,
  () => {
    dismissSuccess.value = false;
  },
);

watch(showSuccess, (visible) => {
  if (!visible) return;
  const t = window.setTimeout(() => {
    dismissSuccess.value = true;
  }, 8000);
  return () => window.clearTimeout(t);
});
</script>

<template>
  <div
    v-if="showInFlight || hasFailure || showWaitingIngress || showSuccess || showNoEffectiveWork"
    class="event-run-status-banner"
  >
    <EventDispatchProgress v-if="showInFlight" :phase="dispatchPhase" />

    <div
      v-else-if="hasFailure"
      class="status-banner status-banner--error"
      role="alert"
    >
      <p class="status-banner__title">
        <template v-if="publishError">{{ publishError }}</template>
        <template v-else-if="lastPublishOutcome">
          {{ lastPublishOutcome.subscribers_accepted }} of
          {{ lastPublishOutcome.subscribers_matched }} subscriber(s) accepted
          <span v-if="lastPublishOutcome.failures.length > 0">
            · {{ lastPublishOutcome.failures.length }} failed
          </span>
        </template>
      </p>
      <details v-if="lastPublishOutcome?.failures.length" class="status-banner__details">
        <summary>Subscriber failures</summary>
        <ul class="failure-list">
          <li v-for="(f, i) in lastPublishOutcome!.failures" :key="i">
            <strong>{{ f.agent_package }}/{{ f.agent_instance_id }}</strong>: {{ f.detail }}
          </li>
        </ul>
      </details>
    </div>

    <div
      v-else-if="showNoEffectiveWork"
      class="status-banner status-banner--warn"
      role="alert"
    >
      <p class="status-banner__title">Publish accepted but no agent work ran</p>
      <pre class="status-banner__acceptances">{{ fleetSummary }}</pre>
    </div>

    <div
      v-else-if="showWaitingIngress"
      class="status-banner status-banner--waiting"
      role="status"
      aria-live="polite"
    >
      <span class="status-banner__spinner" aria-hidden="true" />
      <p class="status-banner__title">
        Subscribers accepted. Waiting for host ingress in transcript…
      </p>
    </div>

    <div
      v-else-if="showSuccess"
      class="status-banner status-banner--success"
      role="status"
    >
      <p class="status-banner__title">{{ fleetSummary }}</p>
      <button type="button" class="status-banner__dismiss" @click="dismissSuccess = true">
        Dismiss
      </button>
    </div>
  </div>
</template>

<style scoped>
.event-run-status-banner {
  flex-shrink: 0;
  border-bottom: 1px solid var(--border);
}

.status-banner {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.5rem 0.75rem;
  padding: 0.5rem 1rem;
  font-size: 0.8125rem;
}

.status-banner__title {
  margin: 0;
  flex: 1;
  min-width: 12rem;
}

.status-banner--error {
  background: var(--color-error-subtle);
  color: var(--color-error);
  border-left: 3px solid var(--color-error);
}

.status-banner--waiting {
  background: color-mix(in srgb, var(--primary) 8%, var(--surface));
  color: var(--text-secondary);
}

.status-banner--warn {
  background: color-mix(in srgb, var(--color-warning, #b45309) 12%, var(--surface));
  color: var(--text);
  border-left: 3px solid var(--color-warning, #b45309);
}

.status-banner__acceptances {
  margin: 0;
  width: 100%;
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 0.75rem;
  white-space: pre-wrap;
  color: var(--text-secondary);
}

.status-banner--success {
  background: var(--color-success-subtle);
  color: var(--color-success);
}

.status-banner__spinner {
  width: 1rem;
  height: 1rem;
  border: 2px solid var(--border);
  border-top-color: var(--primary);
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
  flex-shrink: 0;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .status-banner__spinner {
    animation: none;
    border-top-color: var(--border);
  }
}

.status-banner__details {
  width: 100%;
  margin: 0;
  font-size: 0.8125rem;
}

.status-banner__details summary {
  cursor: pointer;
  font-weight: 600;
}

.failure-list {
  margin: 0.35rem 0 0;
  padding-left: 1.1rem;
  color: var(--text);
}

.status-banner__dismiss {
  font-size: 0.75rem;
  padding: 0.2rem 0.5rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: transparent;
  color: inherit;
  cursor: pointer;
}

.status-banner__dismiss:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}
</style>

<script setup lang="ts">
import type { OperatorRunSeverity, OperatorRunStatus } from "../operator/runStatus";

const props = withDefaults(
  defineProps<{
    status: OperatorRunStatus;
    variant?: "compact" | "banner";
    contextId?: string | null;
    showProgressBar?: boolean;
  }>(),
  { variant: "banner", contextId: null, showProgressBar: true },
);

const emit = defineEmits<{
  dismiss: [];
}>();

const severityClass = (severity: OperatorRunSeverity): string =>
  `run-status--${severity}`;
</script>

<template>
  <div
    class="run-status"
    :class="[severityClass(status.severity), `run-status--${variant}`]"
    role="status"
    aria-live="polite"
  >
    <div class="run-status__main">
      <span class="run-status__dot" aria-hidden="true" />

      <div class="run-status__text">
        <span class="run-status__label">{{ status.label }}</span>
        <span
          v-if="contextId && variant === 'banner'"
          class="run-status__context"
          translate="no"
        >
          {{ contextId }}
        </span>
        <p v-if="status.detail" class="run-status__detail">{{ status.detail }}</p>
      </div>

      <span
        v-if="status.progress && variant === 'banner'"
        class="run-status__progress-chip"
      >
        {{ status.progress.done }}/{{ status.progress.total }} {{ status.progress.noun }}
      </span>

      <slot name="actions" />
    </div>

    <div v-if="status.steps?.length && variant === 'banner'" class="run-status__steps">
      <template v-for="(step, idx) in status.steps" :key="step.key">
        <div
          v-if="idx > 0"
          class="run-status__step-connector"
          :class="{ done: step.state !== 'pending' }"
        />
        <div class="run-status__step" :class="`run-status__step--${step.state}`">
          <span class="run-status__step-dot" aria-hidden="true" />
          <span class="run-status__step-label">{{ step.label }}</span>
        </div>
      </template>
    </div>

    <div
      v-if="showProgressBar && status.active && variant === 'banner'"
      class="run-status__bar"
      aria-hidden="true"
    >
      <span class="run-status__bar-fill" />
    </div>
  </div>
</template>

<style scoped>
.run-status {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
}

.run-status--banner {
  padding: 4px 12px;
  border-bottom: 1px solid var(--border);
  background: var(--surface);
  font-size: 0.8125rem;
}

.run-status--compact {
  display: inline-flex;
  flex-direction: row;
  align-items: center;
  gap: 0.375rem;
  font-size: 0.75rem;
  color: var(--text-secondary);
}

.run-status--neutral.run-status--banner {
  border-left: 3px solid var(--border);
}

.run-status--progress.run-status--banner {
  border-left: 3px solid var(--primary);
}

.run-status--success.run-status--banner {
  border-left: 3px solid var(--color-success);
}

.run-status--warning.run-status--banner {
  border-left: 3px solid var(--color-warning);
}

.run-status--error.run-status--banner {
  border-left: 3px solid var(--color-error);
}

.run-status__main {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.375rem 0.625rem;
  min-height: 1.375rem;
}

.run-status--compact .run-status__main {
  min-height: 0;
}

.run-status__dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
  background: var(--text-muted);
  opacity: 0.55;
}

.run-status--progress .run-status__dot {
  background: var(--primary);
  opacity: 1;
  animation: run-status-pulse 1.2s ease-in-out infinite;
}

.run-status--success .run-status__dot {
  background: var(--color-success);
  opacity: 1;
}

.run-status--warning .run-status__dot {
  background: var(--color-warning);
  opacity: 1;
}

.run-status--error .run-status__dot {
  background: var(--color-error);
  opacity: 1;
}

.run-status__text {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.25rem 0.5rem;
  min-width: 0;
  flex: 1;
}

.run-status--compact .run-status__text {
  flex: 0 1 auto;
}

.run-status__label {
  font-size: 0.6875rem;
  font-weight: 700;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--text-muted);
}

.run-status--compact .run-status__label {
  text-transform: none;
  letter-spacing: 0;
  font-size: 0.75rem;
  font-weight: 600;
  color: var(--text-secondary);
}

.run-status__context {
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--text-secondary);
}

.run-status__detail {
  margin: 0;
  flex: 1 1 100%;
  font-size: 0.8125rem;
  line-height: 1.4;
  color: var(--text-secondary);
}

.run-status--compact .run-status__detail {
  display: none;
}

.run-status__progress-chip {
  font-family: var(--font-mono);
  font-size: 0.6875rem;
  padding: 0.125rem 0.375rem;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  color: var(--text-secondary);
  background: var(--bg-subtle);
}

.run-status__bar {
  height: 2px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--border) 80%, transparent);
  overflow: hidden;
}

.run-status__bar-fill {
  display: block;
  height: 100%;
  width: 35%;
  border-radius: inherit;
  background: var(--primary);
  animation: run-status-bar 1.2s ease-in-out infinite alternate;
}

.run-status__steps {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 0.25rem 0;
  padding: 0.125rem 0 0.25rem;
}

.run-status__step {
  display: inline-flex;
  align-items: center;
  gap: 0.3rem;
  font-size: 0.6875rem;
  color: var(--text-muted);
}

.run-status__step--active {
  color: var(--primary);
  font-weight: 600;
}

.run-status__step--done {
  color: var(--text-secondary);
}

.run-status__step-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--border);
}

.run-status__step--done .run-status__step-dot {
  background: var(--color-success);
}

.run-status__step--active .run-status__step-dot {
  background: var(--primary);
  animation: run-status-pulse 1.2s ease-in-out infinite;
}

.run-status__step-connector {
  width: 1rem;
  height: 1px;
  background: var(--border);
  margin: 0 0.15rem;
}

.run-status__step-connector.done {
  background: var(--color-success);
}

@keyframes run-status-pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.35;
  }
}

@keyframes run-status-bar {
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
  .run-status--progress .run-status__dot,
  .run-status__step--active .run-status__step-dot {
    animation: none;
  }

  .run-status__bar-fill {
    animation: none;
    width: 100%;
    opacity: 0.45;
  }
}
</style>

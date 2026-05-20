<script setup lang="ts">
import MessageBubble from "../MessageBubble.vue";
import type { ChatMessage } from "../../types/a2a";
import type { EventDispatchPhase, EventPublishResponse } from "../../types/events";

defineProps<{
  dispatchPhase: EventDispatchPhase;
  hydrateState: string;
  messages: ChatMessage[];
  usePublishSummary: boolean;
  lastPublishOutcome: EventPublishResponse | null;
  publishError: string | null;
  contextId: string | null;
  taskId: string | null;
  validationValid: boolean;
  validationStale: boolean;
  showScopeCard: boolean;
  busy: boolean;
}>();

const emit = defineEmits<{
  refresh: [];
  openLive: [];
  copyContextId: [];
}>();

const phaseLabels: Record<EventDispatchPhase, string> = {
  idle: "",
  validating: "Validating draft…",
  publishing: "Publishing to subscribers…",
  recording: "Fetching provenance transcript…",
  live: "Provenance transcript",
  empty: "No agent transcript yet — publish summary shown",
  failed: "Publish failed",
};
</script>

<template>
  <div class="event-run-observe">
    <div class="observe-toolbar">
      <h3>Run observation</h3>
      <div class="observe-actions">
        <button
          type="button"
          class="btn btn--sm"
          :disabled="!contextId || busy"
          @click="emit('refresh')"
        >
          Refresh trace
        </button>
        <button
          type="button"
          class="btn btn--sm btn--primary"
          :disabled="!contextId"
          @click="emit('openLive')"
        >
          Open Live provenance
        </button>
      </div>
    </div>

    <p v-if="phaseLabels[dispatchPhase]" class="phase-label">{{ phaseLabels[dispatchPhase] }}</p>

    <div v-if="validationStale" class="banner banner--warn">
      Draft changed — re-validate before publish.
    </div>
    <div v-else-if="validationValid && dispatchPhase === 'idle'" class="banner banner--ok">
      Validation current — ready to publish.
    </div>

    <div v-if="showScopeCard && contextId" class="scope-card">
      <h4>Run scope</h4>
      <div class="scope-ids">
        <span class="field-label">context_id</span>
        <code>{{ contextId }}</code>
        <button type="button" class="btn btn--sm" @click="emit('copyContextId')">Copy</button>
        <template v-if="taskId">
          <span class="field-label">task_id</span>
          <code>{{ taskId }}</code>
        </template>
      </div>
    </div>

    <div v-if="lastPublishOutcome || publishError" class="outcome-card">
      <h4>Publish outcome</h4>
      <p v-if="publishError" class="detail-error">{{ publishError }}</p>
      <template v-else-if="lastPublishOutcome">
        <p>
          <strong>
            {{ lastPublishOutcome.subscribers_accepted }}/{{
              lastPublishOutcome.subscribers_matched
            }}
            subscriber(s) accepted
          </strong>
        </p>
        <ul v-if="lastPublishOutcome.failures.length > 0" class="failure-list">
          <li v-for="(f, i) in lastPublishOutcome.failures" :key="i">
            {{ f.agent_package }}/{{ f.agent_instance_id }}: {{ f.detail }}
          </li>
        </ul>
        <p
          v-if="
            lastPublishOutcome.subscribers_accepted > 0 && hydrateState === 'empty'
          "
          class="field-hint outcome-hint"
        >
          Subscribers accepted the event. Agent chat lines appear when the agent records A2A
          messages; host ingress lines should appear above once provenance catches up.
        </p>
      </template>
    </div>

    <div class="transcript-section">
      <p v-if="usePublishSummary" class="field-hint operator-publish-trace-label">
        Publish summary — host ingress and agent transcript load from provenance history.
      </p>
      <p v-if="hydrateState === 'loading'">Loading provenance transcript…</p>
      <p v-else-if="hydrateState === 'waiting'">Waiting for provenance-backed transcript…</p>
      <MessageBubble
        v-for="(msg, i) in messages"
        :key="msg.id ?? i"
        :message="msg"
      />
      <p
        v-if="hydrateState === 'idle' && messages.length === 0 && !usePublishSummary"
        class="field-hint"
      >
        Validate to preview scope, publish to run, or pick a prior context from the toolbar
        Context dropdown.
      </p>
    </div>
  </div>
</template>

<style scoped>
.event-run-observe {
  display: flex;
  flex-direction: column;
  gap: 0.65rem;
  min-height: 0;
  flex: 1;
  overflow: hidden;
}

.observe-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
  flex-shrink: 0;
}

.observe-toolbar h3 {
  margin: 0;
  font-size: 0.875rem;
}

.observe-actions {
  display: flex;
  gap: 0.35rem;
}

.phase-label {
  margin: 0;
  font-size: 0.8125rem;
  font-weight: 600;
  color: var(--color-accent);
}

.banner {
  font-size: 0.8125rem;
  padding: 0.4rem 0.6rem;
  border-radius: var(--radius-sm);
  margin: 0;
}

.banner--warn {
  background: color-mix(in srgb, var(--color-warning, #c90) 15%, var(--surface));
  border: 1px solid color-mix(in srgb, var(--color-warning, #c90) 40%, transparent);
}

.banner--ok {
  background: color-mix(in srgb, var(--color-success, #2a8) 12%, var(--surface));
  border: 1px solid color-mix(in srgb, var(--color-success, #2a8) 35%, transparent);
}

.scope-card,
.outcome-card {
  padding: 0.5rem 0.65rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface);
  flex-shrink: 0;
}

.scope-card h4,
.outcome-card h4 {
  margin: 0 0 0.35rem;
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  color: var(--text-muted);
}

.scope-ids {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.35rem 0.5rem;
  font-size: 0.75rem;
}

.scope-ids code {
  font-size: 0.7rem;
  word-break: break-all;
}

.field-label {
  font-size: 0.6875rem;
  font-weight: 600;
  text-transform: uppercase;
  color: var(--text-muted);
}

.transcript-section {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding-right: 0.25rem;
}

.operator-publish-trace-label {
  font-style: italic;
  margin: 0 0 0.35rem;
}

.detail-error {
  color: var(--color-error);
  margin: 0;
}

.failure-list {
  margin: 0.35rem 0 0;
  padding-left: 1.1rem;
  font-size: 0.8125rem;
}

.outcome-hint {
  margin-top: 0.35rem;
}
</style>

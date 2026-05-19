<script setup lang="ts">
import { computed, watch, toRef } from "vue";
import type { AgentDiscoveryEntry } from "../types/a2a";
import { useEventConsole } from "../eventConsole/useEventConsole";
import { summarizeSubscriptions } from "../eventConsole/agentFilter";
import { EVENT_SAMPLES } from "../eventConsole/sampleCatalog";

const props = defineProps<{
  agents: ReadonlyArray<AgentDiscoveryEntry>;
}>();

const emit = defineEmits<{
  openContext: [payload: { contextId: string; agent: AgentDiscoveryEntry }];
}>();

const ec = useEventConsole({ agents: toRef(props, "agents") });

const noEventCapableAgents = computed(() => ec.eventCapableAgents.value.length === 0);

const validationMessage = computed<string | null>(() => {
  const parsed = ec.parsedMessages.value;
  if (parsed.ok) return null;
  return parsed.error;
});

const dispatchDisabledReason = computed<string | null>(() => {
  if (!ec.selectedAgent.value) return "Pick an event-capable agent.";
  if (!ec.selectedSample.value) return "Pick a sample event.";
  const parsed = ec.parsedMessages.value;
  if (!parsed.ok) return `Fix JSON: ${parsed.error}`;
  if (ec.scope.kind === "existing_context" && !ec.continueContextId.value.trim()) {
    return "Provide a context id to continue, or switch to New context.";
  }
  return null;
});

const outcomeBadgeClass = computed(() => {
  const o = ec.outcome.value;
  if (!o) return "";
  if (o.status === "accepted") return "ec-ack-badge--accepted";
  if (o.status === "rejected") return "ec-ack-badge--rejected";
  return "ec-ack-badge--error";
});

watch(
  () => ec.eventCapableAgents.value.map(ec.agentKey).join(","),
  () => {
    if (
      ec.selectedAgentKey.value &&
      !ec.eventCapableAgents.value.some(
        (a) => ec.agentKey(a) === ec.selectedAgentKey.value,
      )
    ) {
      ec.selectedAgentKey.value = null;
    }
    if (!ec.selectedAgentKey.value && ec.eventCapableAgents.value.length > 0) {
      const first = ec.eventCapableAgents.value[0]!;
      ec.selectAgent(ec.agentKey(first));
    }
  },
  { immediate: true },
);

function onOpenContextClick(): void {
  const o = ec.outcome.value;
  const agent = ec.selectedAgent.value;
  if (!o || !o.contextId || !agent) return;
  emit("openContext", { contextId: o.contextId, agent });
}

function formatTimestamp(ms: number): string {
  return new Date(ms).toLocaleTimeString();
}
</script>

<template>
  <section class="event-console" aria-label="Event Console">
    <header class="ec-header">
      <h2 class="ec-title">Event Console</h2>
      <p class="ec-subtitle">
        Dispatch a sample event through
        <code>POST /agents/{pkg}/{inst}/dispatch</code> and observe the resulting
        context. Exploratory testing — not a chat composer.
      </p>
    </header>

    <div v-if="noEventCapableAgents" class="ec-empty">
      <p>
        No agents currently advertise dispatch subscriptions. Deploy an agent
        whose manifest declares <code>discovery.subscriptions</code> (e.g. the
        <code>dispatch-echo</code> fixture) to use the Event Console.
      </p>
    </div>

    <div v-else class="ec-grid">
      <!-- Left column: selection + editor -->
      <div class="ec-col ec-col--left">
        <div class="ec-card">
          <label class="ec-field">
            <span class="ec-label">Target agent</span>
            <select
              class="ec-input"
              :value="ec.selectedAgentKey.value ?? ''"
              @change="(e) => ec.selectAgent((e.target as HTMLSelectElement).value)"
            >
              <option value="" disabled>Pick an event-capable agent…</option>
              <option
                v-for="agent in ec.eventCapableAgents.value"
                :key="ec.agentKey(agent)"
                :value="ec.agentKey(agent)"
              >
                {{ agent.agent_package }} / {{ agent.agent_instance_id }}
              </option>
            </select>
          </label>
          <p v-if="ec.selectedAgent.value" class="ec-hint">
            Subscriptions: {{ summarizeSubscriptions(ec.selectedAgent.value) }}
          </p>
        </div>

        <div class="ec-card">
          <label class="ec-field">
            <span class="ec-label">Sample event</span>
            <select
              class="ec-input"
              :value="ec.selectedSampleId.value"
              @change="(e) => ec.loadSampleIntoEditor((e.target as HTMLSelectElement).value)"
            >
              <option v-for="s in EVENT_SAMPLES" :key="s.id" :value="s.id">
                {{ s.label }}
              </option>
            </select>
          </label>
          <p v-if="ec.selectedSample.value" class="ec-hint">
            {{ ec.selectedSample.value.summary }}
          </p>
          <p
            v-if="ec.selectedSample.value?.notes"
            class="ec-sample-note"
          >
            {{ ec.selectedSample.value.notes }}
          </p>
          <div class="ec-row">
            <button
              type="button"
              class="btn btn--secondary btn--sm"
              @click="ec.loadSampleIntoEditor()"
            >
              Reset from sample
            </button>
          </div>
        </div>

        <div class="ec-card">
          <label class="ec-field">
            <span class="ec-label">Scope</span>
            <div class="ec-scope-row">
              <label class="ec-radio">
                <input
                  type="radio"
                  value="new_context"
                  :checked="ec.scope.kind === 'new_context'"
                  @change="ec.scope.kind = 'new_context'"
                />
                New context
              </label>
              <label class="ec-radio">
                <input
                  type="radio"
                  value="existing_context"
                  :checked="ec.scope.kind === 'existing_context'"
                  @change="ec.scope.kind = 'existing_context'"
                />
                Continue context
              </label>
            </div>
          </label>
          <label
            v-if="ec.scope.kind === 'existing_context'"
            class="ec-field"
          >
            <span class="ec-label">Existing context id</span>
            <input
              type="text"
              class="ec-input ec-input--mono"
              placeholder="ctx-…"
              :value="ec.continueContextId.value"
              @input="(e) => (ec.continueContextId.value = (e.target as HTMLInputElement).value)"
            />
          </label>
          <label class="ec-field">
            <span class="ec-label">Operator note (optional)</span>
            <input
              type="text"
              class="ec-input"
              placeholder="Why this dispatch? (recorded in metadata)"
              :value="ec.operatorNote.value"
              @input="(e) => (ec.operatorNote.value = (e.target as HTMLInputElement).value)"
            />
          </label>
        </div>

        <div class="ec-card">
          <label class="ec-field">
            <span class="ec-label">messages[] (raw JSON)</span>
            <textarea
              class="ec-input ec-textarea ec-input--mono"
              spellcheck="false"
              rows="14"
              :value="ec.messagesJsonText.value"
              @input="(e) => (ec.messagesJsonText.value = (e.target as HTMLTextAreaElement).value)"
            ></textarea>
          </label>
          <p v-if="validationMessage" class="ec-validation ec-validation--error">
            JSON error: {{ validationMessage }}
          </p>
          <p v-else class="ec-validation ec-validation--ok">JSON parses.</p>
        </div>
      </div>

      <!-- Right column: preview + dispatch + ack -->
      <div class="ec-col ec-col--right">
        <div class="ec-card">
          <div class="ec-card-header">
            <span class="ec-label">AgentDispatchRequest preview</span>
            <span class="ec-hint">Exactly what will be POSTed.</span>
          </div>
          <pre v-if="ec.previewText.value" class="ec-preview"><code>{{ ec.previewText.value }}</code></pre>
          <p v-else class="ec-empty-line">Pick a sample and a target agent to preview the request.</p>
        </div>

        <div class="ec-card">
          <div class="ec-row ec-row--space">
            <button
              type="button"
              class="btn btn--primary"
              :disabled="!!dispatchDisabledReason || ec.isDispatching.value"
              @click="ec.dispatch"
            >
              {{ ec.isDispatching.value ? "Dispatching…" : "Dispatch event" }}
            </button>
            <span v-if="dispatchDisabledReason" class="ec-hint ec-hint--warn">
              {{ dispatchDisabledReason }}
            </span>
          </div>
        </div>

        <div v-if="ec.outcome.value" class="ec-card">
          <div class="ec-card-header">
            <span class="ec-label">Dispatch ack</span>
            <span class="ec-hint">{{ formatTimestamp(ec.outcome.value.finishedAt) }}</span>
          </div>
          <div class="ec-ack-row">
            <span :class="['ec-ack-badge', outcomeBadgeClass]">
              {{ ec.outcome.value.status.toUpperCase() }}
            </span>
            <span v-if="ec.outcome.value.httpStatus" class="ec-hint">
              HTTP {{ ec.outcome.value.httpStatus }}
            </span>
          </div>
          <dl class="ec-ack-grid">
            <dt>Target</dt>
            <dd>
              <code>{{ ec.outcome.value.targetPackage }}/{{ ec.outcome.value.targetInstanceId }}</code>
            </dd>
            <dt>routing_key</dt>
            <dd><code>{{ ec.outcome.value.routingKey }}</code></dd>
            <dt>message_type</dt>
            <dd><code>{{ ec.outcome.value.messageType }}</code></dd>
            <dt v-if="ec.outcome.value.contextId">context_id</dt>
            <dd v-if="ec.outcome.value.contextId">
              <code>{{ ec.outcome.value.contextId }}</code>
            </dd>
            <dt v-if="ec.outcome.value.taskId">task_id</dt>
            <dd v-if="ec.outcome.value.taskId">
              <code>{{ ec.outcome.value.taskId }}</code>
            </dd>
            <dt v-if="ec.outcome.value.messageId">message_id</dt>
            <dd v-if="ec.outcome.value.messageId">
              <code>{{ ec.outcome.value.messageId }}</code>
            </dd>
            <dt v-if="ec.outcome.value.detail">detail</dt>
            <dd v-if="ec.outcome.value.detail" class="ec-ack-detail">
              {{ ec.outcome.value.detail }}
            </dd>
          </dl>
          <div v-if="ec.outcome.value.contextId" class="ec-row ec-row--space">
            <button
              type="button"
              class="btn btn--secondary btn--sm"
              :disabled="!ec.selectedAgent.value"
              @click="onOpenContextClick"
            >
              Open resulting context →
            </button>
            <span class="ec-hint">
              Opens this context in the Chat / Provenance view.
            </span>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.event-console {
  padding: 16px 20px 32px;
  max-width: 1200px;
  margin: 0 auto;
}
.ec-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 16px;
}
.ec-title {
  font-size: var(--text-heading);
  font-weight: 600;
  margin: 0 0 4px 0;
  color: var(--text);
}
.ec-subtitle {
  font-size: var(--text-base);
  color: var(--text-secondary);
  margin: 0;
  max-width: 720px;
}
.ec-subtitle code {
  font-family: var(--font-mono);
  font-size: var(--text-sm);
  background: var(--bg-subtle);
  border-radius: var(--radius-sm);
  padding: 1px 6px;
}
.ec-empty {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  padding: 20px;
  color: var(--text-secondary);
  font-size: var(--text-base);
}
.ec-empty code {
  font-family: var(--font-mono);
  font-size: var(--text-sm);
  background: var(--bg-subtle);
  border-radius: var(--radius-sm);
  padding: 1px 6px;
}
.ec-grid {
  display: grid;
  grid-template-columns: minmax(320px, 1fr) minmax(360px, 1.2fr);
  gap: 16px;
}
.ec-col {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.ec-card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.ec-card-header {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
}
.ec-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.ec-label {
  font-size: var(--text-sm);
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-muted);
  font-weight: 600;
}
.ec-input {
  font-family: inherit;
  font-size: var(--text-base);
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--input-bg);
  color: var(--text);
}
.ec-input:focus {
  outline: none;
  border-color: var(--primary);
  box-shadow: 0 0 0 3px var(--primary-subtle);
}
.ec-input--mono {
  font-family: var(--font-mono);
  font-size: var(--text-sm);
}
.ec-textarea {
  resize: vertical;
  min-height: 200px;
  line-height: 1.4;
}
.ec-hint {
  font-size: var(--text-sm);
  color: var(--text-muted);
}
.ec-hint--warn {
  color: var(--color-warning);
}
.ec-sample-note {
  font-size: var(--text-sm);
  color: var(--text-secondary);
  background: var(--bg-subtle);
  border-left: 3px solid var(--color-accent);
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  margin: 0;
}
.ec-scope-row {
  display: flex;
  gap: 12px;
  align-items: center;
}
.ec-radio {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: var(--text-base);
  color: var(--text);
}
.ec-row {
  display: flex;
  gap: 8px;
  align-items: center;
}
.ec-row--space {
  justify-content: space-between;
}
.ec-validation {
  margin: 0;
  font-size: var(--text-sm);
}
.ec-validation--ok {
  color: var(--color-success);
}
.ec-validation--error {
  color: var(--color-error);
}
.ec-preview {
  margin: 0;
  padding: 12px;
  background: var(--bg-subtle);
  border-radius: var(--radius-sm);
  font-family: var(--font-mono);
  font-size: var(--text-sm);
  max-height: 480px;
  overflow: auto;
  line-height: 1.45;
}
.ec-empty-line {
  margin: 0;
  font-size: var(--text-sm);
  color: var(--text-muted);
}
.ec-ack-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.ec-ack-badge {
  display: inline-block;
  padding: 3px 10px;
  font-size: var(--text-sm);
  font-weight: 600;
  border-radius: var(--radius-sm);
  letter-spacing: 0.05em;
  border: 1px solid var(--border);
  background: var(--bg-subtle);
  color: var(--text);
}
.ec-ack-badge--accepted {
  background: var(--color-success-subtle);
  border-color: var(--color-success-border);
  color: var(--color-success);
}
.ec-ack-badge--rejected {
  background: var(--color-warning-subtle);
  border-color: var(--color-warning-border);
  color: var(--color-warning);
}
.ec-ack-badge--error {
  background: var(--color-error-subtle);
  border-color: var(--color-error-border);
  color: var(--color-error);
}
.ec-ack-grid {
  display: grid;
  grid-template-columns: max-content 1fr;
  column-gap: 12px;
  row-gap: 4px;
  margin: 0;
  font-size: var(--text-sm);
}
.ec-ack-grid dt {
  color: var(--text-muted);
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.ec-ack-grid dd {
  margin: 0;
  color: var(--text);
}
.ec-ack-grid code {
  font-family: var(--font-mono);
  font-size: var(--text-sm);
}
.ec-ack-detail {
  word-break: break-word;
  white-space: pre-wrap;
}

@media (max-width: 880px) {
  .ec-grid {
    grid-template-columns: 1fr;
  }
}
</style>

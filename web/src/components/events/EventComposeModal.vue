<script setup lang="ts">
import { computed, ref } from "vue";
import type { AgentDiscoveryEntry } from "../../types/a2a";
import type {
  AgentDeliverableMessageShape,
  DerivedDispatchEnvelope,
  DraftPayloadRecord,
  EventDispatchScope,
  EventDispatchScopeKind,
  EventValidationReport,
} from "../../types/events";
import {
  scopeContextIdFromDraft,
  scopeTaskIdFromDraft,
  shouldOfferApplyObservedScope,
} from "../../events/eventConsoleState";
import { useFocusTrap } from "../../composables/useFocusTrap";
import EventAgentSelector from "./EventAgentSelector.vue";
import EventBatchEditor from "./EventBatchEditor.vue";

const props = defineProps<{
  open: boolean;
  busy: boolean;
  canPublish: boolean;
  publishLabel: string;
  agents: AgentDiscoveryEntry[];
  subscribedAgents: AgentDiscoveryEntry[];
  selectedAgent: AgentDiscoveryEntry | null;
  agentsLoading?: boolean;
  agentAcceptsHostDispatch: boolean;
  shapesForAgent: AgentDeliverableMessageShape[];
  messageShapeId: string;
  selectedMessageShape: AgentDeliverableMessageShape | null;
  derivedEnvelope: DerivedDispatchEnvelope | null;
  observeContextId: string | null;
  observeTaskId: string | null;
  observedContextId: string | null;
  draftScope: EventDispatchScope;
  draftMessages: DraftPayloadRecord[];
  activeMessageIndex: number;
  validationFocusPath: string | null;
  contextualizedValidation: EventValidationReport | null;
  publishPreview: string | null;
}>();

const emit = defineEmits<{
  close: [];
  publish: [];
  validate: [];
  "select-agent": [agent: AgentDiscoveryEntry];
  "select-message-shape": [messageShapeId: string];
  "apply-observed-scope": [];
  "scope-change": [kind: EventDispatchScopeKind];
  "update:scopeContextId": [value: string];
  "update:scopeTaskId": [value: string];
  "update:messages": [messages: DraftPayloadRecord[]];
  "update:activeIndex": [index: number];
  addMessage: [];
  duplicateMessage: [index: number];
  removeMessage: [index: number];
  "apply-sample": [sample: AgentDeliverableMessageShape["samples"][number]];
}>();

const modalRef = ref<HTMLElement | null>(null);
const isVisible = computed(() => props.open);
useFocusTrap(modalRef, isVisible);

const canClose = computed(() => !props.busy);
const titleId = "event-compose-modal-title";

const draftScopeKind = computed(() => props.draftScope.kind);
const scopeContextId = computed(() => scopeContextIdFromDraft(props.draftScope));
const scopeTaskId = computed(() => scopeTaskIdFromDraft(props.draftScope));

const showApplyObservedScope = computed(() =>
  shouldOfferApplyObservedScope(props.observedContextId, props.draftScope),
);

const scopeSegments: { kind: EventDispatchScopeKind; label: string }[] = [
  { kind: "new_context", label: "New context" },
  { kind: "existing_context", label: "Existing context" },
  { kind: "existing_task", label: "Existing task" },
];

const agentsForSelector = computed(() =>
  props.subscribedAgents.length > 0 ? props.subscribedAgents : props.agents,
);

function onOverlayClick(e: MouseEvent): void {
  if (e.target === e.currentTarget && canClose.value) emit("close");
}

function onKeydown(e: KeyboardEvent): void {
  if (e.key === "Escape" && canClose.value) emit("close");
}

function onMessageShapeChange(e: Event): void {
  const id = (e.target as HTMLSelectElement).value;
  if (id) emit("select-message-shape", id);
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="event-compose-overlay"
      role="dialog"
      aria-modal="true"
      :aria-labelledby="titleId"
      @click="onOverlayClick"
      @keydown="onKeydown"
    >
      <div ref="modalRef" class="event-compose-modal">
        <header class="event-compose-modal-header">
          <h2 :id="titleId" class="event-compose-modal-title">Publish event</h2>
          <button
            type="button"
            class="btn btn--sm btn--ghost"
            :disabled="!canClose"
            aria-label="Close"
            @click="emit('close')"
          >
            Close
          </button>
        </header>

        <div class="event-compose-modal-body">
          <section class="compose-section">
            <EventAgentSelector
              :agents="agentsForSelector"
              :subscribed-agents="subscribedAgents"
              :selected="selectedAgent"
              :loading="agentsLoading"
              label="Agent"
              select-id="event-compose-modal-agent"
              @select="emit('select-agent', $event)"
            />
            <p v-if="selectedAgent && !agentAcceptsHostDispatch" class="field-hint field-hint--warn">
              This agent does not accept host dispatch.
            </p>
          </section>

          <template v-if="selectedAgent && agentAcceptsHostDispatch">
            <section class="compose-section">
              <label class="field-label" for="event-compose-source-payload">Source payload</label>
              <select
                id="event-compose-source-payload"
                name="source-payload"
                :value="messageShapeId"
                @change="onMessageShapeChange"
              >
                <option value="">Select source payload…</option>
                <option
                  v-for="shape in shapesForAgent"
                  :key="shape.message_shape_id"
                  :value="shape.message_shape_id"
                  :title="shape.wire_schema_version"
                >
                  {{ shape.display_name }}
                </option>
              </select>
            </section>

            <template v-if="selectedMessageShape">
              <p v-if="observeContextId" class="field-hint observe-hint">
                Observing <code translate="no">{{ observeContextId }}</code>
                <template v-if="observeTaskId">
                  · task <code translate="no">{{ observeTaskId }}</code>
                </template>
              </p>

              <button
                v-if="showApplyObservedScope"
                type="button"
                class="btn btn--sm"
                @click="emit('apply-observed-scope')"
              >
                Target observed run as scope
              </button>

              <div v-if="selectedMessageShape.samples.length" class="sample-row">
                <span class="field-label">Samples</span>
                <button
                  v-for="sample in selectedMessageShape.samples"
                  :key="sample.sample_id"
                  type="button"
                  class="btn btn--sm"
                  @click="emit('apply-sample', sample)"
                >
                  {{ sample.label }}
                </button>
              </div>

              <details v-if="derivedEnvelope" class="envelope-preview">
                <summary>Dispatch envelope</summary>
                <dl class="envelope-dl">
                  <dt>message_type</dt>
                  <dd><code translate="no">{{ derivedEnvelope.messageType }}</code></dd>
                  <dt>routing_key</dt>
                  <dd><code translate="no">{{ derivedEnvelope.routingKey }}</code></dd>
                  <dt>source_kind</dt>
                  <dd><code translate="no">{{ derivedEnvelope.sourceKind }}</code></dd>
                  <dt>source_key</dt>
                  <dd><code translate="no">{{ derivedEnvelope.sourceKey || "—" }}</code></dd>
                </dl>
              </details>

              <section class="compose-section">
                <span class="field-label">Scope</span>
                <div class="scope-segments" role="group" aria-label="Dispatch scope">
                  <button
                    v-for="seg in scopeSegments"
                    :key="seg.kind"
                    type="button"
                    class="scope-segment"
                    :class="{ 'scope-segment--active': draftScopeKind === seg.kind }"
                    :aria-pressed="draftScopeKind === seg.kind"
                    @click="emit('scope-change', seg.kind)"
                  >
                    {{ seg.label }}
                  </button>
                </div>
                <p v-if="draftScopeKind !== 'new_context'" class="field-hint scope-context-hint">
                  Enter ids below, or use <strong>Target observed run</strong> when browsing history.
                </p>
                <div v-if="draftScopeKind !== 'new_context'" class="inline-fields">
                  <input
                    :value="scopeContextId"
                    placeholder="context_id"
                    spellcheck="false"
                    autocomplete="off"
                    @input="emit('update:scopeContextId', ($event.target as HTMLInputElement).value)"
                  />
                  <input
                    v-if="draftScopeKind === 'existing_task'"
                    :value="scopeTaskId"
                    placeholder="task_id"
                    spellcheck="false"
                    autocomplete="off"
                    @input="emit('update:scopeTaskId', ($event.target as HTMLInputElement).value)"
                  />
                </div>
              </section>

              <EventBatchEditor
                :message-shape="selectedMessageShape"
                :messages="draftMessages"
                :active-index="activeMessageIndex"
                :validation-focus-path="validationFocusPath"
                @update:messages="emit('update:messages', $event)"
                @update:active-index="emit('update:activeIndex', $event)"
                @add="emit('addMessage')"
                @duplicate="emit('duplicateMessage', $event)"
                @remove="emit('removeMessage', $event)"
              />

              <div
                v-if="
                  contextualizedValidation?.errors?.length ||
                  contextualizedValidation?.warnings?.length ||
                  publishPreview
                "
                class="validation-panel"
              >
                <h3 v-if="contextualizedValidation?.errors?.length">Fix before publishing</h3>
                <ul
                  v-if="contextualizedValidation?.errors?.length"
                  class="issue-list issue-list--error"
                >
                  <li v-for="(err, i) in contextualizedValidation.errors" :key="i">
                    <code>{{ err.code }}</code> {{ err.message }}
                    <span v-if="err.json_pointer"> ({{ err.json_pointer }})</span>
                  </li>
                </ul>
                <ul v-if="contextualizedValidation?.warnings?.length" class="issue-list">
                  <li v-for="(w, i) in contextualizedValidation.warnings" :key="i">
                    <code>{{ w.code }}</code> {{ w.message }}
                  </li>
                </ul>
                <details v-if="publishPreview" class="preview-details">
                  <summary>Produced event preview (JSON)</summary>
                  <pre class="preview-pre" translate="no">{{ publishPreview }}</pre>
                </details>
              </div>
            </template>
          </template>
        </div>

        <footer class="event-compose-modal-footer">
          <button type="button" class="btn btn--secondary" :disabled="!canClose" @click="emit('close')">
            Cancel
          </button>
          <button
            type="button"
            class="btn btn--sm"
            :disabled="busy || !selectedMessageShape"
            @click="emit('validate')"
          >
            Validate
          </button>
          <button
            type="button"
            class="btn btn--primary"
            :disabled="!canPublish || busy"
            @click="emit('publish')"
          >
            {{ publishLabel }}
          </button>
        </footer>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.event-compose-overlay {
  position: fixed;
  inset: 0;
  z-index: 1100;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 1rem;
  background: color-mix(in srgb, var(--bg) 45%, transparent);
}

.event-compose-modal {
  display: flex;
  flex-direction: column;
  width: min(720px, 92vw);
  max-height: min(85vh, 900px);
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-lg);
  overflow: hidden;
}

.event-compose-modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.75rem 1rem;
  border-bottom: 1px solid var(--border);
  flex-shrink: 0;
}

.event-compose-modal-title {
  margin: 0;
  font-size: 1.125rem;
  font-weight: 600;
  text-wrap: balance;
}

.event-compose-modal-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 1rem;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.event-compose-modal-footer {
  display: flex;
  justify-content: flex-end;
  flex-wrap: wrap;
  gap: 0.5rem;
  padding: 0.75rem 1rem;
  border-top: 1px solid var(--border);
  flex-shrink: 0;
  background: var(--surface);
}

.compose-section {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
}

.field-label {
  font-size: 0.75rem;
  font-weight: 600;
  color: var(--text-secondary);
}

select,
.inline-fields input {
  width: 100%;
  font-size: 0.8125rem;
  padding: 0.4rem 0.55rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-raised);
  color: var(--text);
}

select:focus-visible,
.inline-fields input:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}

.scope-segments {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem;
}

.scope-segment {
  padding: 0.35rem 0.65rem;
  font-size: 0.8125rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--bg-subtle);
  color: var(--text-secondary);
  cursor: pointer;
}

.scope-segment--active {
  border-color: color-mix(in srgb, var(--primary) 45%, var(--border));
  background: var(--primary-subtle);
  color: var(--primary);
  font-weight: 600;
}

.scope-segment:focus-visible {
  outline: 2px solid var(--primary);
  outline-offset: 2px;
}

.inline-fields {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem;
}

.inline-fields input {
  flex: 1;
  min-width: 6rem;
}

.field-hint {
  font-size: 0.75rem;
  margin: 0;
  color: var(--text-muted);
}

.field-hint--warn {
  color: var(--color-error);
}

.observe-hint code {
  font-size: 0.7rem;
}

.envelope-preview {
  margin: 0.25rem 0;
  font-size: 0.8125rem;
}

.envelope-dl {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: 0.25rem 0.75rem;
  margin: 0.35rem 0 0;
}

.envelope-dl dt {
  color: var(--text-muted);
}

.sample-row {
  display: flex;
  flex-wrap: wrap;
  gap: 0.35rem;
  align-items: center;
}

.validation-panel {
  border-top: 1px solid var(--border);
  padding-top: 0.75rem;
}

.validation-panel h3 {
  font-size: 0.875rem;
  margin: 0 0 0.5rem;
}

.issue-list {
  font-size: 0.8125rem;
  margin: 0;
  padding-left: 1.1rem;
}

.issue-list--error {
  color: var(--color-error);
}

.preview-details summary {
  cursor: pointer;
  font-size: 0.8125rem;
  color: var(--text-muted);
}

.preview-pre {
  font-size: 0.7rem;
  max-height: 8rem;
  overflow: auto;
  color: var(--code-text);
  background: var(--code-bg);
  padding: 0.5rem;
  border-radius: var(--radius-sm);
}
</style>

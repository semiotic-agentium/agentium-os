<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import ProvenancePane from "../ProvenancePane.vue";
import ConversationHistorySelector from "../ConversationHistorySelector.vue";
import EventBatchEditor from "./EventBatchEditor.vue";
import EventHistoryPanel from "./EventHistoryPanel.vue";
import EventRunObservePanel from "./EventRunObservePanel.vue";
import {
  buildOperatorPublishTraceMessages,
  resolveDispatchUnitTaskId,
  transcriptHasHostIngress,
} from "../../events/dispatchObserve";
import {
  isEventDispatchInFlight,
  isEventDispatchProvenanceStreaming,
} from "../../events/dispatchPhases";
import { messageShapesForAgent } from "../../events/messageShapes";
import { useEventConsole } from "../../composables/useEventConsole";
import {
  useEventObservation,
  type LoadContextOptions,
} from "../../composables/useEventObservation";
import { useToast } from "../../composables/useToast";
import { parseMermaidBlocks } from "../../utils/parseMermaid";
import { looksLikeMermaidDiagram } from "../../utils/mermaidDiagram";
import type { ChatMessage, ConversationHistoryOption } from "../../types/a2a";
const toast = useToast();

const {
  mode,
  draft,
  selection,
  activeMessageIndex,
  validation,
  validationStale,
  validationFocusPath,
  lastPublishOutcome,
  fleetAcceptsCurrentShape,
  dispatchPhase,
  publishError,
  historyItems,
  filteredHistoryItems,
  selectedContextId,
  historyLoading,
  historyFetchError,
  historyFilterPreview,
  busy,
  agents: deployedAgents,
  subscribedAgents,
  selectedAgent,
  agentAcceptsHostDispatch,
  agentSubscriptions,
  messageShapes: registryShapes,
  shapesForSelectedSubscription,
  contextualizedValidation,
  selectedMessageShape,
  derivedEnvelope,
  activeRunSummary,
  fetchAgents,
  fetchMessageShapes,
  applyRouteFromUrl,
  selectContextFromPicker,
  selectAgent,
  selectSubscriptionEvent,
  applySample,
  addMessage,
  duplicateMessage,
  removeMessage,
  setScope,
  validateDraft,
  publishEvent,
  fetchHistory,
  useContextAsDraftScope,
  observeContextFromDraftOrFlow,
} = useEventConsole();

const observation = useEventObservation();
const provenancePaneOpen = ref(false);
const useOperatorPublishSummary = ref(false);
const provenanceExternalFocus = ref<{ nonce: number; tab: "live" } | undefined>();

const canPublish = computed(
  () =>
    fleetAcceptsCurrentShape.value &&
    Boolean(selectedAgent.value) &&
    Boolean(selectedMessageShape.value) &&
    draft.value.messages.length > 0 &&
    !busy.value,
);

const canPublishNow = computed(
  () => canPublish.value && Boolean(validation.value?.valid) && !validationStale.value,
);

const observeIds = computed(() => observeContextFromDraftOrFlow());

const provenancePaneDiagrams = computed(() => {
  const prov = observation.provenanceDiagram.value.trim();
  if (prov.length > 0 && looksLikeMermaidDiagram(prov)) return [prov];
  for (const m of observation.messages.value) {
    if (m.role !== "agent") continue;
    const blocks = parseMermaidBlocks(m.text);
    const first = blocks[0]?.trim();
    if (first && looksLikeMermaidDiagram(first)) return [first];
  }
  return [];
});

const publishPreview = computed(() => {
  const preview = validation.value?.preview_produced_event;
  return preview ? JSON.stringify(preview, null, 2) : null;
});

const showScopeCard = computed(
  () =>
    Boolean(observeIds.value.contextId) &&
    (Boolean(validation.value?.valid) || Boolean(lastPublishOutcome.value)),
);

const displayMessages = computed((): ChatMessage[] => {
  if (observation.messages.value.length > 0) {
    return observation.messages.value;
  }
  if (useOperatorPublishSummary.value) {
    return buildOperatorPublishTraceMessages({
      agentPackage: draft.value.agent_package,
      agentInstanceId: draft.value.agent_instance_id,
      messageShape: selectedMessageShape.value,
      envelope: derivedEnvelope.value,
      sampleLabel: selectedMessageShape.value?.samples.find(
        (s) => s.sample_id === selection.value.sampleId,
      )?.label,
      outcome: lastPublishOutcome.value,
      publishError: publishError.value,
    });
  }
  return [];
});

const sessionStatusClass = computed(() => {
  const phase = dispatchPhase.value;
  if (phase === "failed") return "session-status--failed";
  if (phase === "live") return "session-status--live";
  if (isEventDispatchInFlight(phase)) {
    return "session-status--active";
  }
  if (validation.value?.valid && !validationStale.value) return "session-status--ready";
  return "";
});

const provenancePaneStreaming = computed(() =>
  isEventDispatchProvenanceStreaming(dispatchPhase.value),
);

function copyObserveContextId(): void {
  const id = observeIds.value.contextId;
  if (!id) return;
  void navigator.clipboard.writeText(id);
  toast.success("Copied context_id");
}

function updateDispatchPhaseFromObservation(): void {
  const state = observation.hydrateState.value;
  if (dispatchPhase.value === "failed" || dispatchPhase.value === "validating") {
    return;
  }
  if (state === "loading" || state === "waiting") {
    dispatchPhase.value = "recording";
    return;
  }
  if (state === "ready") {
    dispatchPhase.value = "live";
    useOperatorPublishSummary.value = false;
    return;
  }
  if (state === "empty" && lastPublishOutcome.value) {
    dispatchPhase.value = transcriptHasHostIngress(observation.messages.value)
      ? "live"
      : "empty";
    useOperatorPublishSummary.value = !transcriptHasHostIngress(observation.messages.value);
    provenanceExternalFocus.value = { nonce: Date.now(), tab: "live" };
    return;
  }
}

function onScopeChange(kind: string): void {
  if (kind === "new_context") {
    setScope({ kind: "new_context" });
    return;
  }
  if (kind === "existing_context") {
    const ctx =
      draft.value.scope.kind === "existing_context"
        ? draft.value.scope.context_id
        : draft.value.scope.kind === "existing_task"
          ? draft.value.scope.context_id
          : "";
    setScope({ kind: "existing_context", context_id: ctx });
    return;
  }
  const ctx =
    draft.value.scope.kind !== "new_context" ? draft.value.scope.context_id : "";
  const task =
    draft.value.scope.kind === "existing_task" ? draft.value.scope.task_id : "";
  setScope({ kind: "existing_task", context_id: ctx, task_id: task });
}

function scopeContextId(): string {
  const s = draft.value.scope;
  return s.kind === "new_context" ? "" : s.context_id;
}

function scopeTaskId(): string {
  return draft.value.scope.kind === "existing_task" ? draft.value.scope.task_id : "";
}

function updateScopeContextId(value: string): void {
  const s = draft.value.scope;
  if (s.kind === "existing_task") {
    setScope({ kind: "existing_task", context_id: value, task_id: s.task_id });
  } else {
    setScope({ kind: "existing_context", context_id: value });
  }
}

function updateScopeTaskId(value: string): void {
  const s = draft.value.scope;
  if (s.kind === "existing_task") {
    setScope({ kind: "existing_task", context_id: s.context_id, task_id: value });
  }
}

function observationLoadOptions(
  extra?: Partial<LoadContextOptions>,
  resolvedTaskId?: string | null,
): LoadContextOptions {
  // Dispatch-unit task scope already isolates the canonical ingress user line; adding
  // agentPackage hides host ingress messages that are not linked to the agent archive.
  const agentPackage =
    resolvedTaskId != null && resolvedTaskId !== ""
      ? null
      : draft.value.agent_package || null;
  return {
    agentPackage,
    ...extra,
  };
}

async function resolveObservationTaskId(
  contextId: string,
  taskId: string | null | undefined,
): Promise<string | null> {
  if (taskId) return taskId;
  return resolveDispatchUnitTaskId(contextId);
}

async function refreshObservation(): Promise<void> {
  const { contextId, taskId } = observeIds.value;
  if (contextId) {
    provenancePaneOpen.value = true;
    const resolvedTask = await resolveObservationTaskId(contextId, taskId);
    await observation.loadContext(
      contextId,
      resolvedTask,
      observationLoadOptions(undefined, resolvedTask),
    );
    updateDispatchPhaseFromObservation();
  } else {
    observation.clear();
    useOperatorPublishSummary.value = false;
  }
}

function openLiveProvenance(): void {
  provenancePaneOpen.value = true;
  provenanceExternalFocus.value = { nonce: Date.now(), tab: "live" };
}

async function onPublish(): Promise<void> {
  await publishEvent();
  if (publishError.value) {
    toast.error("Publish failed");
    useOperatorPublishSummary.value = true;
    dispatchPhase.value = "failed";
    provenancePaneOpen.value = true;
    return;
  }
  if (!lastPublishOutcome.value) {
    toast.error("Publish did not run — select a message type and fix validation errors.");
    return;
  }
  const o = lastPublishOutcome.value;
  const label = `Published ${o.subscribers_accepted}/${o.subscribers_matched}`;
  if (o.failures.length > 0) {
    toast.show(`${label} (${o.failures.length} failed)`, "info");
  } else if (o.subscribers_accepted > 0) {
    toast.success(label);
  } else {
    toast.show("No subscribers accepted the event", "info");
  }
  useOperatorPublishSummary.value = true;
  dispatchPhase.value = "recording";
  provenancePaneOpen.value = true;
  const { contextId, taskId } = observeIds.value;
  if (contextId) {
    void resolveObservationTaskId(contextId, taskId).then((resolvedTask) =>
      observation.loadContext(
        contextId,
        resolvedTask,
        observationLoadOptions(
          {
            preserveMessagesUntilTranscript: true,
          },
          resolvedTask,
        ),
      ),
    );
  }
}

function viewHistoryAfterPublish(): void {
  if (!lastPublishOutcome.value) return;
  mode.value = "history";
  void fetchHistory();
}

function onRoutePopState(): void {
  applyRouteFromUrl();
}

onMounted(async () => {
  await Promise.all([fetchAgents(), fetchMessageShapes()]);
  applyRouteFromUrl();
  if (!draft.value.agent_package) {
    await fetchHistory();
  }
  window.addEventListener("popstate", onRoutePopState);
});

onUnmounted(() => {
  window.removeEventListener("popstate", onRoutePopState);
});

watch(
  () => draft.value.agent_package,
  () => {
    void fetchHistory();
  },
);

watch(
  () => [
    observeIds.value.contextId,
    observeIds.value.taskId,
    draft.value.agent_package,
    lastPublishOutcome.value,
  ],
  () => {
    void refreshObservation();
  },
);

watch(
  () => observation.hydrateState.value,
  () => {
    updateDispatchPhaseFromObservation();
  },
);

watch(lastPublishOutcome, () => {
  if (lastPublishOutcome.value && observeIds.value.contextId) {
    provenancePaneOpen.value = true;
  }
});

watch(
  () => mode.value,
  (m) => {
    if (m === "history") void fetchHistory();
  },
);

async function onSelectHistoryContext(contextId: string): Promise<void> {
  const known = historyItems.value.find((h) => h.contextId === contextId);
  selectContextFromPicker(
    known ?? { contextId, latestTimestampMs: 0, preview: "" },
  );
  await refreshObservation();
}

async function onSelectContextFromPicker(option: ConversationHistoryOption): Promise<void> {
  selectContextFromPicker(option);
  await refreshObservation();
}

function onUseContextAsDraft(): void {
  useContextAsDraftScope();
  toast.show("Bound compose scope to selected context", "info");
}

function openSelectedAgentInChat(): void {
  if (!selectedAgent.value) return;
  const url = new URL(window.location.href);
  url.searchParams.set("view", "chat");
  url.searchParams.set("agentPackage", selectedAgent.value.agent_package);
  url.searchParams.set("agentInstance", selectedAgent.value.agent_instance_id);
  window.location.href = url.toString();
}
</script>

<template>
  <div class="events-layout">
    <div class="events-toolbar">
      <div class="events-toolbar-row">
      <div class="events-mode-tabs" role="tablist">
        <button
          type="button"
          :class="{ active: mode === 'compose' }"
          @click="mode = 'compose'"
        >
          Compose Event
        </button>
        <button
          type="button"
          :class="{ active: mode === 'history' }"
          @click="mode = 'history'"
        >
          History
        </button>
      </div>
        <ConversationHistorySelector
          class="events-context-picker"
          :histories="historyItems"
          :selected-context-id="selectedContextId"
          :loading="historyLoading"
          :disabled="!selectedAgent"
          @select="onSelectContextFromPicker"
          @refresh="fetchHistory()"
        />
      <div v-if="mode === 'compose'" class="events-toolbar-actions">
        <button
          v-if="lastPublishOutcome"
          type="button"
          class="btn btn--sm"
          @click="viewHistoryAfterPublish"
        >
          View history
        </button>
        <button
          type="button"
          class="btn btn--sm"
          :disabled="!canPublish"
          @click="validateDraft()"
        >
          Validate
        </button>
        <button
          type="button"
          class="btn btn--primary btn--sm"
          :disabled="!canPublishNow"
          @click="onPublish()"
        >
          Publish
        </button>
        </div>
      </div>
      <p v-if="historyFetchError" class="events-toolbar-error field-hint field-hint--warn">
        {{ historyFetchError }}
      </p>
    </div>

    <div class="chat-session-zones events-session-zones" aria-label="Operator dispatch session">
      <div class="zone-pair">
        <span class="zone-chip zone-chip--events">Event Intake</span>
        <span class="zone-hint">Message-shaped dispatch</span>
      </div>
      <div class="session-meta" aria-live="polite">
        <template v-if="activeRunSummary.agentLabel">
          <span>{{ activeRunSummary.agentLabel }}</span>
        </template>
        <template v-if="activeRunSummary.messageTypeLabel">
          <span> · {{ activeRunSummary.messageTypeLabel }}</span>
        </template>
        <template v-if="activeRunSummary.contextId">
          <span> · </span>
          <code>{{ activeRunSummary.contextId }}</code>
        </template>
        <template v-if="activeRunSummary.taskId">
          <span> · task </span>
          <code>{{ activeRunSummary.taskId }}</code>
        </template>
        <span
          v-if="activeRunSummary.statusLabel"
          :class="['session-status', sessionStatusClass]"
        >
          · {{ activeRunSummary.statusLabel }}
        </span>
        <template v-if="!activeRunSummary.agentLabel">
          <span>Select agent and message type to bind a run.</span>
        </template>
      </div>
      <div class="zone-pair zone-pair--end">
        <span class="zone-hint">Provenance &amp; transcript</span>
        <span class="zone-chip zone-chip--observe">Observe</span>
      </div>
    </div>

    <div class="events-body app-body">
      <section class="events-intake panel">
        <template v-if="mode === 'compose'">
          <div
            v-if="selectedAgent && !agentAcceptsHostDispatch"
            class="agent-dispatch-banner agent-dispatch-banner--warn"
            role="status"
          >
            <strong>{{ selectedAgent.agent_package }}</strong> is deployed but does not accept
            host event dispatch (no <code>discovery.subscriptions</code> in its manifest and no
            <code>onDispatch</code> handler). Provenance for this agent is produced via
            <strong>Chat</strong>, not the Event Console.
            <button type="button" class="btn btn--sm" @click="openSelectedAgentInChat()">
              Open in Chat
            </button>
          </div>

          <label class="field-label">Agent</label>
          <select
            :value="`${draft.agent_package}/${draft.agent_instance_id}`"
            @change="
              (e) => {
                const v = (e.target as HTMLSelectElement).value;
                const agent = deployedAgents.find(
                  (a) => `${a.agent_package}/${a.agent_instance_id}` === v,
                );
                if (agent) selectAgent(agent);
              }
            "
          >
            <option value="">Select agent…</option>
            <option
              v-for="a in deployedAgents"
              :key="`${a.agent_package}/${a.agent_instance_id}`"
              :value="`${a.agent_package}/${a.agent_instance_id}`"
            >
              {{ a.agent_package }}/{{ a.agent_instance_id }}
              {{
                (a.agent_card.subscriptions?.length ?? 0) > 0
                  ? ""
                  : " (chat only)"
              }}
            </option>
          </select>
          <p v-if="subscribedAgents.length === 0" class="field-hint field-hint--warn">
            No deployed agents declare event subscriptions. Publish an agent with
            <code>discovery.subscriptions</code> (e.g. slack-agent, coordinator-agent, dispatch-echo).
          </p>

          <template v-if="selectedAgent">
            <label class="field-label">Subscription</label>
            <select
              :value="selection.subscriptionIndex"
              @change="
                (e) => {
                  const idx = Number((e.target as HTMLSelectElement).value);
                  const sub = agentSubscriptions[idx];
                  if (!sub) return;
                  const shapes = messageShapesForAgent(registryShapes, [sub]);
                  if (shapes[0]) selectSubscriptionEvent(shapes[0].message_shape_id, idx);
                }
              "
            >
              <option v-for="(sub, i) in agentSubscriptions" :key="i" :value="i">
                {{ sub.schema_versions?.join(", ") || "any schema" }} ·
                {{ sub.source_kinds?.join(", ") || "any kind" }}
              </option>
            </select>
          </template>

          <label class="field-label">Message type</label>
          <select
            :value="selection.messageShapeId"
            @change="
              (e) => {
                const id = (e.target as HTMLSelectElement).value;
                if (id) selectSubscriptionEvent(id, selection.subscriptionIndex);
              }
            "
          >
            <option value="">Select message type…</option>
            <option
              v-for="shape in shapesForSelectedSubscription"
              :key="shape.message_shape_id"
              :value="shape.message_shape_id"
            >
              {{ shape.display_name }}
            </option>
          </select>
          <p
            v-if="selectedAgent && shapesForSelectedSubscription.length === 0"
            class="field-hint field-hint--warn"
          >
            No deliverable message types match this subscription.
          </p>
          <p v-else-if="selectedMessageShape" class="field-hint">
            {{ selectedMessageShape.description }}
            <span class="field-hint-origin">Origin: {{ selectedMessageShape.origin }}</span>
          </p>

          <div v-if="selectedMessageShape?.samples.length" class="sample-row">
            <label class="field-label">Samples</label>
            <button
              v-for="sample in selectedMessageShape.samples"
              :key="sample.sample_id"
              type="button"
              class="btn btn--sm"
              @click="applySample(sample)"
            >
              {{ sample.label }}
            </button>
          </div>

          <details v-if="derivedEnvelope" class="envelope-preview">
            <summary>Dispatch envelope</summary>
            <dl class="envelope-dl">
              <dt>message_type</dt>
              <dd><code>{{ derivedEnvelope.messageType }}</code></dd>
              <dt>routing_key</dt>
              <dd><code>{{ derivedEnvelope.routingKey }}</code></dd>
              <dt>source_kind</dt>
              <dd><code>{{ derivedEnvelope.sourceKind }}</code></dd>
              <dt>source_key</dt>
              <dd><code>{{ derivedEnvelope.sourceKey || "—" }}</code></dd>
              <template v-if="observeIds.contextId">
                <dt>context_id</dt>
                <dd><code>{{ observeIds.contextId }}</code></dd>
              </template>
              <template v-if="observeIds.taskId">
                <dt>task_id</dt>
                <dd><code>{{ observeIds.taskId }}</code></dd>
              </template>
            </dl>
          </details>

          <label class="field-label">Scope</label>
          <select
            :value="draft.scope.kind"
            @change="onScopeChange((($event.target as HTMLSelectElement).value))"
          >
            <option value="new_context">New context</option>
            <option value="existing_context">Continue context</option>
            <option value="existing_task">Continue task</option>
          </select>
          <p v-if="draft.scope.kind !== 'new_context'" class="field-hint scope-context-hint">
            Use the <strong>Context</strong> dropdown in the toolbar to pick a provenance-backed
            run, or enter ids below.
          </p>
          <div v-if="draft.scope.kind !== 'new_context'" class="inline-fields">
            <input
              :value="scopeContextId()"
              placeholder="context_id"
              @input="updateScopeContextId((($event.target as HTMLInputElement).value))"
            />
            <input
              v-if="draft.scope.kind === 'existing_task'"
              :value="scopeTaskId()"
              placeholder="task_id"
              @input="updateScopeTaskId((($event.target as HTMLInputElement).value))"
            />
          </div>

          <EventBatchEditor
            :message-shape="selectedMessageShape ?? null"
            :messages="draft.messages"
            :active-index="activeMessageIndex"
            :validation-focus-path="validationFocusPath"
            @update:messages="(m) => (draft.messages = m)"
            @update:active-index="(i) => (activeMessageIndex = i)"
            @add="addMessage()"
            @duplicate="duplicateMessage"
            @remove="removeMessage"
          />

          <div class="validation-panel">
            <h3>Validation</h3>
            <ul v-if="contextualizedValidation?.errors?.length" class="issue-list issue-list--error">
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
              <pre class="preview-pre">{{ publishPreview }}</pre>
            </details>
          </div>
        </template>

        <EventHistoryPanel
          v-else
          :items="filteredHistoryItems"
          :selected-context-id="selectedContextId"
          :loading="historyLoading"
          :fetch-error="historyFetchError"
          :filter-preview="historyFilterPreview"
          @update:filter-preview="(v) => (historyFilterPreview = v)"
          @refresh="fetchHistory()"
          @select="onSelectHistoryContext"
          @use-as-draft="onUseContextAsDraft"
        />
      </section>

      <section class="events-primary panel">
        <EventRunObservePanel
          :dispatch-phase="dispatchPhase"
          :hydrate-state="observation.hydrateState.value"
          :messages="displayMessages"
          :use-publish-summary="useOperatorPublishSummary"
          :last-publish-outcome="lastPublishOutcome"
          :publish-error="publishError"
          :context-id="observeIds.contextId"
          :task-id="observeIds.taskId"
          :validation-valid="Boolean(validation?.valid)"
          :validation-stale="validationStale"
          :show-scope-card="showScopeCard"
          :busy="busy"
          @refresh="refreshObservation()"
          @open-live="openLiveProvenance()"
          @copy-context-id="copyObserveContextId()"
        />
      </section>

      <ProvenancePane
        :default-open="provenancePaneOpen"
        :context-id="observeIds.contextId ?? undefined"
        :task-id="observeIds.taskId ?? undefined"
        :observe-agent-package="draft.agent_package || undefined"
        :is-streaming="provenancePaneStreaming"
        :diagrams="provenancePaneDiagrams"
        :trace-refresh-tick="observation.traceRefreshGeneration.value"
        :external-tab-focus="provenanceExternalFocus"
      />
    </div>
  </div>
</template>

<style scoped>
.events-layout {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  color: var(--text);
  background: linear-gradient(
    135deg,
    color-mix(in srgb, var(--color-accent) 4%, var(--bg)) 0%,
    var(--bg) 40%
  );
}

.events-toolbar {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  padding: 0.5rem 1rem;
  border-bottom: 1px solid var(--border);
  flex-shrink: 0;
}

.events-toolbar-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  flex-wrap: wrap;
}

.events-context-picker {
  flex: 1;
  min-width: 12rem;
  max-width: 28rem;
}

.events-toolbar-error {
  margin: 0;
}

.scope-context-hint {
  margin-top: 0.25rem;
}

.agent-dispatch-banner {
  padding: 0.65rem 0.75rem;
  border-radius: var(--radius-sm);
  font-size: 0.8125rem;
  line-height: 1.45;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.agent-dispatch-banner--warn {
  border: 1px solid color-mix(in srgb, var(--color-warning, #c90) 45%, var(--border));
  background: color-mix(in srgb, var(--color-warning, #c90) 12%, var(--surface));
}

.agent-dispatch-banner code {
  font-size: 0.75rem;
}

.events-mode-tabs {
  display: flex;
  gap: 0.35rem;
}

.events-toolbar-actions {
  display: flex;
  gap: 0.4rem;
  flex-shrink: 0;
  flex-wrap: wrap;
}

.events-mode-tabs button {
  font-size: 0.8125rem;
  padding: 0.35rem 0.75rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface);
}

.events-mode-tabs button.active {
  border-color: var(--color-accent);
  background: color-mix(in srgb, var(--color-accent) 12%, var(--surface));
}

.events-session-zones .zone-chip--events {
  background: color-mix(in srgb, #6b4c9a 25%, var(--surface));
  border-color: #6b4c9a;
}

.session-status {
  font-weight: 600;
}

.session-status--ready {
  color: var(--color-success, #2a8);
}

.session-status--live,
.session-status--active {
  color: var(--color-accent);
}

.session-status--failed {
  color: var(--color-error);
}

.events-body {
  flex: 1;
  min-height: 0;
}

.events-intake.panel {
  flex: 0 0 min(420px, 40%);
  max-width: 420px;
  overflow-y: auto;
  padding: 1rem;
  display: flex;
  flex-direction: column;
  gap: 0.65rem;
  border-right: 1px solid var(--border);
  background: var(--surface);
  min-width: 0;
}

.events-primary.panel {
  flex: 1;
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 0.75rem 1rem;
  background: color-mix(in srgb, var(--color-accent) 3%, var(--bg));
}

.events-body :deep(.provenance-pane) {
  flex: 1;
  min-width: 0;
  min-height: 0;
}

.field-label {
  font-size: 0.75rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  color: var(--text-muted);
}

select,
.inline-fields input {
  width: 100%;
  font-size: 0.8125rem;
  padding: 0.35rem 0.5rem;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-raised);
  color: var(--text);
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
  margin: 0.15rem 0 0.5rem;
  color: var(--text-muted);
}

.field-hint--warn {
  color: var(--color-danger, #c44);
}

.field-hint-origin {
  display: block;
  margin-top: 0.15rem;
  font-family: var(--font-mono, ui-monospace, monospace);
  font-size: 0.6875rem;
}

.envelope-preview {
  margin: 0.5rem 0;
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

.envelope-dl code {
  font-size: 0.75rem;
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
}

.validation-panel h3 {
  font-size: 0.875rem;
  margin: 0 0 0.5rem;
}
</style>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import ProvenancePane from "../ProvenancePane.vue";
import TranscriptView from "../TranscriptView.vue";
import EventComposeModal from "./EventComposeModal.vue";
import EventRunHeader from "./EventRunHeader.vue";
import EventRunStatusStrip from "./EventRunStatusStrip.vue";
import {
  buildEventConsoleLocalTranscript,
  localTranscriptMatchesScope,
  transcriptHasIngressUserRows,
} from "../../events/dispatchObserve";
import {
  isEventDispatchInFlight,
} from "../../events/dispatchPhases";
import { deriveEventRunStatus } from "../../operator/runStatus";
import { messageShapesForSubscription } from "../../events/messageShapes";
import { useEventConsole } from "../../composables/useEventConsole";
import {
  useEventObservation,
  type LoadContextOptions,
} from "../../composables/useEventObservation";
import { useToast } from "../../composables/useToast";
import type { EventRunMeta } from "../../events/eventTranscriptModel";
import {
  changeDraftScopeKind,
  isEventDispatchScopeKind,
  publishTargetsNewSession,
  updateDraftScopeContextId,
  updateDraftScopeTaskId,
} from "../../events/eventConsoleState";
import { parseMermaidBlocks } from "../../utils/parseMermaid";
import { looksLikeMermaidDiagram } from "../../utils/mermaidDiagram";
import type { ConversationHistoryOption } from "../../types/a2a";
import type { DraftPayloadRecord, EventDispatchScopeKind } from "../../types/events";
import {
  buildObservationRefreshLoadKey,
  buildObservationScopeKey,
  buildObserveScopeWatchKey,
  shouldPreserveTranscriptOnScopeChange,
  shouldSkipObservationRefresh,
} from "../../events/eventConsoleObservation";

const toast = useToast();

const {
  draft,
  selection,
  activeMessageIndex,
  validation,
  validationStale,
  validationFocusPath,
  lastPublishOutcome,
  lastPublishedScope,
  fleetAcceptsCurrentShape,
  dispatchPhase,
  publishError,
  historyItems,
  observedContextId,
  observedScopeIds,
  historyLoading,
  historyFetchError,
  busy,
  agents: deployedAgents,
  subscribedAgents,
  selectedAgent,
  agentAcceptsHostDispatch,
  agentSubscriptions,
  messageShapes: registryShapes,
  availableMessageShapes,
  contextualizedValidation,
  selectedMessageShape,
  derivedEnvelope,
  fetchAgents,
  fetchMessageShapes,
  applyRouteFromUrl,
  selectAgent,
  selectContextFromPicker,
  selectSubscriptionEvent,
  applySample,
  addMessage,
  duplicateMessage,
  removeMessage,
  setScope,
  applyObservedContextToDraftScope,
  beginNewEvent,
  beginPublishSession,
  validateDraft,
  publishEvent,
  fetchHistory,
} = useEventConsole();

const observation = useEventObservation();
const composeModalOpen = ref(false);
let lastObservationLoadKey = "";
let observationRefreshInFlight: Promise<void> | null = null;
let observationRefreshInFlightKey = "";
const transcriptViewRef = ref<InstanceType<typeof TranscriptView> | null>(null);
const runHeaderRef = ref<InstanceType<typeof EventRunHeader> | null>(null);
const provenancePreferOpen = ref(false);
const provenanceExternalFocus = ref<{ nonce: number; tab: "live" } | undefined>();

const canPublish = computed(
  () =>
    fleetAcceptsCurrentShape.value &&
    Boolean(selectedAgent.value) &&
    Boolean(selectedMessageShape.value) &&
    draft.value.messages.length > 0 &&
    !busy.value,
);

const publishLabel = computed(() => {
  if (!busy.value) return "Publish event";
  if (dispatchPhase.value === "validating") return "Validating…";
  if (dispatchPhase.value === "publishing") return "Publishing…";
  return "Working…";
});

const observeIds = observedScopeIds;

const provenanceDefaultOpen = computed(() => Boolean(observeIds.value.contextId));

const historyRunsHint = computed(() => {
  if (observedContextId.value) return null;
  if (historyLoading.value) return null;
  const n = historyItems.value.length;
  if (n === 0) return null;
  const runLabel = n === 1 ? "run" : "runs";
  return `${n} recent ${runLabel} — pick one or publish a new event.`;
});

const agentsLoading = ref(false);

function onSelectMessageShape(messageShapeId: string): void {
  const subs = agentSubscriptions.value;
  for (let i = 0; i < subs.length; i++) {
    const shapes = messageShapesForSubscription(registryShapes.value, subs, i);
    if (shapes.some((s) => s.message_shape_id === messageShapeId)) {
      selectSubscriptionEvent(messageShapeId, i);
      return;
    }
  }
  selectSubscriptionEvent(messageShapeId, selection.value.subscriptionIndex);
}

async function loadAgentsAndShapes(): Promise<void> {
  agentsLoading.value = true;
  try {
    await Promise.all([fetchAgents(), fetchMessageShapes()]);
  } finally {
    agentsLoading.value = false;
  }
}

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

const localTranscriptRows = computed(() => {
  if (
    !lastPublishOutcome.value ||
    !localTranscriptMatchesScope(lastPublishedScope.value, observeIds.value.contextId)
  ) {
    return [];
  }
  return buildEventConsoleLocalTranscript({
    previewProducedEvent: validation.value?.preview_produced_event ?? null,
    outcome: lastPublishOutcome.value,
    publishError: publishError.value,
    agentPackage: draft.value.agent_package,
    agentInstanceId: draft.value.agent_instance_id,
    messageShape: selectedMessageShape.value ?? undefined,
    envelope: derivedEnvelope.value,
  });
});

watch(
  localTranscriptRows,
  (rows) => {
    observation.setLocalOverlay(rows);
  },
  { immediate: true },
);

const transcriptShowsDispatchFailures = computed(() =>
  observation.transcriptMessages.value.some((m) =>
    (m.contentBlocks ?? []).some(
      (b) =>
        b.type === "operational" &&
        (b.kind === "dispatch_rejected" || b.kind === "dispatch_transport_error"),
    ),
  ),
);

const waitingForIngress = computed(() => {
  if (!lastPublishOutcome.value) return false;
  if ((lastPublishOutcome.value.subscribers_accepted ?? 0) === 0) return false;
  if (transcriptHasIngressUserRows(observation.transcriptMessages.value)) return false;
  const hydrate = observation.hydrateState.value;
  if (hydrate === "empty" || hydrate === "error") return false;
  return (
    hydrate === "waiting" ||
    hydrate === "loading" ||
    isEventDispatchInFlight(dispatchPhase.value)
  );
});

const hasPublishedRun = computed(
  () =>
    Boolean(lastPublishOutcome.value) &&
    observation.transcriptMessages.value.length === 0 &&
    (observation.hydrateState.value === "empty" ||
      observation.hydrateState.value === "waiting"),
);

function publishObservationActive(): boolean {
  return (
    Boolean(lastPublishOutcome.value) &&
    localTranscriptMatchesScope(lastPublishedScope.value, observeIds.value.contextId)
  );
}

/** Avoid empty/skeleton flicker while optimistic ingress rows are on screen. */
const transcriptHydrateState = computed(() => {
  const raw = observation.hydrateState.value;
  if (
    observation.transcriptMessages.value.length > 0 &&
    (raw === "waiting" || raw === "empty" || raw === "loading")
  ) {
    return "ready";
  }
  return raw;
});

const runStatus = computed(() =>
  deriveEventRunStatus({
    dispatchPhase: dispatchPhase.value,
    hydrateState: transcriptHydrateState.value,
    lastPublishOutcome: lastPublishOutcome.value,
    publishError: publishError.value,
    waitingForIngress: waitingForIngress.value,
    transcriptMessages: observation.transcriptMessages.value,
    contextId: observeIds.value.contextId,
    observeOnly: !publishObservationActive(),
  }),
);

const transcriptMessages = observation.transcriptMessages;
const traceRefreshTick = computed(() => observation.traceRefreshGeneration.value);

const eventRunMeta = computed((): EventRunMeta => ({
  dispatchPhase: dispatchPhase.value,
  hydrateState: transcriptHydrateState.value,
  lastPublishOutcome: lastPublishOutcome.value,
  publishError: publishError.value,
  waitingForIngress: waitingForIngress.value,
  hasPublishedRun: hasPublishedRun.value,
}));

function updateDispatchPhaseFromObservation(): void {
  const state = transcriptHydrateState.value;
  if (dispatchPhase.value === "failed" || dispatchPhase.value === "validating") {
    return;
  }
  if (state === "loading" || state === "waiting") {
    dispatchPhase.value = "recording";
    return;
  }
  if (state === "ready") {
    dispatchPhase.value = "live";
    return;
  }
  if (state === "empty" && lastPublishOutcome.value) {
    const hasIngress = transcriptHasIngressUserRows(observation.transcriptMessages.value);
    dispatchPhase.value = hasIngress || localTranscriptRows.value.length > 0 ? "live" : "empty";
    if (hasIngress) {
      provenanceExternalFocus.value = { nonce: Date.now(), tab: "live" };
    }
    return;
  }
}

function onScopeChange(kind: EventDispatchScopeKind | string): void {
  if (!isEventDispatchScopeKind(kind)) return;
  setScope(changeDraftScopeKind(draft.value.scope, kind));
}

function onUpdateScopeContextId(value: string): void {
  setScope(updateDraftScopeContextId(draft.value.scope, value));
}

function onUpdateScopeTaskId(value: string): void {
  setScope(updateDraftScopeTaskId(draft.value.scope, value));
}

function observationLoadOptions(
  extra?: Partial<LoadContextOptions>,
): LoadContextOptions {
  return {
    agentPackage: draft.value.agent_package || null,
    ingressMode: observedContextId.value ? "explicit_restore" : "evented",
    ...extra,
  };
}

async function refreshObservation(options?: { preserveTranscript?: boolean }): Promise<void> {
  const { contextId, taskId } = observeIds.value;
  if (!contextId) {
    lastObservationLoadKey = "";
    observation.clear();
    return;
  }

  const preserve = options?.preserveTranscript ?? publishObservationActive();
  const resolvedTask = taskId?.trim() ? taskId : null;
  const loadKey = buildObservationRefreshLoadKey(contextId, resolvedTask, preserve);

  if (observationRefreshInFlightKey === loadKey && observationRefreshInFlight) {
    return observationRefreshInFlight;
  }

  const scopeKey = buildObservationScopeKey(
    contextId,
    resolvedTask,
    draft.value.agent_package,
  );
  observationRefreshInFlightKey = loadKey;
  observationRefreshInFlight = (async () => {
    if (
      shouldSkipObservationRefresh(
        scopeKey,
        lastObservationLoadKey,
        observation.messages.value.length,
        preserve,
      )
    ) {
      observation.bumpTraceRefresh();
      updateDispatchPhaseFromObservation();
      return;
    }

    lastObservationLoadKey = scopeKey;
    provenancePreferOpen.value = true;
    await observation.loadContext(
      contextId,
      resolvedTask,
      observationLoadOptions(
        preserve ? { preserveMessagesUntilTranscript: true } : undefined,
      ),
    );
    updateDispatchPhaseFromObservation();
  })();

  try {
    await observationRefreshInFlight;
  } finally {
    if (observationRefreshInFlightKey === loadKey) {
      observationRefreshInFlight = null;
      observationRefreshInFlightKey = "";
    }
  }
}

async function onValidate(): Promise<void> {
  const report = await validateDraft();
  if (!report.valid) {
    toast.error(report.errors[0]?.message ?? "Validation failed");
  }
}

async function onPublish(): Promise<void> {
  if (
    validationStale.value ||
    !validation.value?.valid ||
    !validation.value.preview_produced_event
  ) {
    const report = await validateDraft();
    if (!report.valid || !report.preview_produced_event) {
      toast.error(publishError.value ?? "Fix validation errors before publishing.");
      return;
    }
  }

  const startingNewSession = publishTargetsNewSession(
    draft.value.scope,
    observedContextId.value,
  );
  beginPublishSession();
  if (startingNewSession) {
    lastObservationLoadKey = "";
    observation.clear();
  }

  composeModalOpen.value = false;
  provenancePreferOpen.value = true;

  const result = await publishEvent();
  if (result === "validation_failed") {
    toast.error(publishError.value ?? "Fix validation errors before publishing.");
    composeModalOpen.value = true;
    return;
  }
  if (result === "publish_failed") {
    toast.error(publishError.value ?? "Publish failed");
    composeModalOpen.value = true;
    return;
  }

  const { contextId } = observeIds.value;
  if (!contextId) return;

  await refreshObservation({ preserveTranscript: !startingNewSession });
  updateDispatchPhaseFromObservation();
  requestAnimationFrame(() => {
    transcriptViewRef.value?.getScrollContainer()?.scrollIntoView({ behavior: "smooth", block: "start" });
  });
}

function onNewEvent(): void {
  beginNewEvent();
  lastObservationLoadKey = "";
  observation.clear();
  composeModalOpen.value = true;
}

function openComposeFromQuery(): void {
  const params = new URLSearchParams(window.location.search);
  if (params.get("compose") === "1") {
    composeModalOpen.value = true;
    params.delete("compose");
    const url = new URL(window.location.href);
    url.search = params.toString();
    window.history.replaceState(window.history.state, "", url.toString());
  }
}

onMounted(async () => {
  if (deployedAgents.value.length === 0) {
    await loadAgentsAndShapes();
  }
  applyRouteFromUrl();
  openComposeFromQuery();
  if (!observeIds.value.contextId && !draft.value.agent_package) {
    await fetchHistory();
  }
});

watch(
  () => draft.value.agent_package,
  () => {
    void fetchHistory();
  },
);

watch(
  () =>
    buildObserveScopeWatchKey(
      observeIds.value.contextId,
      observeIds.value.taskId,
      draft.value.agent_package,
    ),
  (scopeKey, prevScopeKey) => {
    if (!scopeKey || scopeKey === ":") {
      void refreshObservation();
      return;
    }
    const preserve = shouldPreserveTranscriptOnScopeChange(
      prevScopeKey,
      scopeKey,
      publishObservationActive(),
    );
    void refreshObservation({ preserveTranscript: preserve });
  },
  { immediate: true },
);

watch(
  () => observation.hydrateState.value,
  () => {
    updateDispatchPhaseFromObservation();
  },
);

function onSelectContextFromPicker(option: ConversationHistoryOption): void {
  selectContextFromPicker(option);
  provenancePreferOpen.value = true;
}

function focusEventRunFromTranscript(): void {
  runHeaderRef.value?.focusEventRunSelect();
}

</script>

<template>
  <div class="events-layout">
    <EventRunHeader
      ref="runHeaderRef"
      :agents="deployedAgents"
      :subscribed-agents="subscribedAgents"
      :selected-agent="selectedAgent ?? null"
      :agents-loading="agentsLoading"
      :histories="historyItems"
      :selected-context-id="observedContextId"
      :history-loading="historyLoading"
      :history-fetch-error="historyFetchError"
      :history-runs-hint="historyRunsHint"
      @select-agent="selectAgent"
      @select-context="onSelectContextFromPicker"
      @refresh-history="fetchHistory()"
      @new-event="onNewEvent()"
    />

    <EventRunStatusStrip
      :status="runStatus"
      :context-id="observeIds.contextId"
      :last-publish-outcome="lastPublishOutcome"
      :publish-error="publishError"
      :transcript-shows-dispatch-failures="transcriptShowsDispatchFailures"
    />

    <div class="events-body app-body">
      <section
        class="events-observe panel"
        role="region"
        aria-labelledby="event-console-transcript-heading"
      >
        <TranscriptView
          ref="transcriptViewRef"
          variant="event"
          :messages="transcriptMessages"
          :hydrate-state="transcriptHydrateState"
          :is-streaming="runStatus.active"
          :selected-context-id="observeIds.contextId"
          :waiting-for-ingress="waitingForIngress"
          :has-published-run="hasPublishedRun"
          :event-run-meta="eventRunMeta"
          @compose-event="composeModalOpen = true"
          @focus-event-run="focusEventRunFromTranscript"
        />
      </section>

      <ProvenancePane
        surface="event"
        :default-open="provenanceDefaultOpen"
        :prefer-open="provenancePreferOpen"
        :context-id="observeIds.contextId ?? undefined"
        :task-id="observeIds.taskId ?? undefined"
        :selected-agent-package="draft.agent_package || undefined"
        :run-status="runStatus"
        :diagrams="provenancePaneDiagrams"
        :trace-refresh-tick="traceRefreshTick"
        :external-tab-focus="provenanceExternalFocus"
      />
    </div>

    <EventComposeModal
      :open="composeModalOpen"
      :busy="busy"
      :can-publish="canPublish"
      :publish-label="publishLabel"
      :agents="deployedAgents"
      :subscribed-agents="subscribedAgents"
      :selected-agent="selectedAgent ?? null"
      :agents-loading="agentsLoading"
      :agent-accepts-host-dispatch="agentAcceptsHostDispatch"
      :shapes-for-agent="availableMessageShapes"
      :message-shape-id="selection.messageShapeId"
      :selected-message-shape="selectedMessageShape ?? null"
      :derived-envelope="derivedEnvelope"
      :observe-context-id="observeIds.contextId"
      :observe-task-id="observeIds.taskId"
      :observed-context-id="observedContextId"
      :draft-scope="draft.scope"
      :draft-messages="draft.messages"
      :active-message-index="activeMessageIndex"
      :validation-focus-path="validationFocusPath"
      :contextualized-validation="contextualizedValidation"
      :publish-preview="publishPreview"
      @close="composeModalOpen = false"
      @publish="onPublish()"
      @validate="onValidate()"
      @select-message-shape="onSelectMessageShape"
      @apply-observed-scope="applyObservedContextToDraftScope()"
      @apply-sample="applySample"
      @scope-change="onScopeChange"
      @update:scope-context-id="onUpdateScopeContextId"
      @update:scope-task-id="onUpdateScopeTaskId"
      @update:messages="(m: DraftPayloadRecord[]) => (draft.messages = m)"
      @update:active-index="(i) => (activeMessageIndex = i)"
      @add-message="addMessage()"
      @duplicate-message="duplicateMessage"
      @remove-message="removeMessage"
    />
  </div>
</template>

<style scoped>
.events-layout {
  position: relative;
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  color: var(--text);
  background: var(--bg);
}

.events-body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: row;
  overflow: hidden;
}

.events-observe.panel {
  flex: 1 1 0;
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  padding: 0;
  background: var(--bg);
  border-right: 1px solid var(--border);
}

.events-observe.panel :deep(.message-row--ingress-wire) {
  width: 100%;
}

.events-body :deep(.provenance-pane.open) {
  flex: 0 1 min(480px, 38vw);
  max-width: min(480px, 38vw);
}

.events-body :deep(.provenance-pane:not(.open)) {
  flex: 0 0 auto;
}
</style>

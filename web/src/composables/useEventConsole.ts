// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { computed, ref, watch } from "vue";
import { instanceFetch } from "./instanceApi";
import type { AgentDiscoveryEntry, ContextPickerPage, ConversationHistoryOption } from "../types/a2a";
import {
  readEventConsoleRouteFromUrl,
  writeEventConsoleRouteToUrl,
} from "../events/operatorRoute";
import type {
  AgentDeliverableMessageShape,
  DerivedDispatchEnvelope,
  EventConsoleSelection,
  EventDispatchPhase,
  EventDispatchScope,
  EventObservationState,
  EventPayloadDraft,
  EventPublishResponse,
  EventValidationReport,
  MessageShapeSample,
  ResolvedObservationIds,
} from "../types/events";
import { resolveDispatchUnitTaskId, scopeFromRecord, type PublishedScope } from "../events/dispatchObserve";
import {
  buildObservationScopeResolveInput,
  createInitialObservation,
  parsePreviewProducedEvent,
  pickerOptionFromContextId,
  composeScopeForObservedRun,
  publishedScopeFromPreview,
  resolveObservedScopeIds,
} from "../events/eventConsoleState";
import {
  autofillPayload,
  deriveDispatchEnvelope,
  findMessageShape,
  findSample,
  firstDeliverableShape,
  messageShapesForAgent,
  messageShapesForSubscription,
  payloadFromShapeSelection,
  subscriptionMatchesShape,
} from "../events/messageShapes";
import { defaultPayloadFromSchema } from "../events/schemaForm";

const OPERATOR_ORIGIN = "operator-event-console";

function cloneJson<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function emptyDraft(): EventPayloadDraft {
  return {
    agent_package: "",
    agent_instance_id: "",
    messages: [],
    scope: { kind: "new_context" },
    message_id: "",
    metadata: { origin: OPERATOR_ORIGIN },
  };
}

function emptySelection(): EventConsoleSelection {
  return {
    agentPackage: "",
    agentInstanceId: "",
    subscriptionIndex: 0,
    messageShapeId: "",
  };
}

function normalizeEpochMs(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const n = Number(value);
    if (Number.isFinite(n)) return n;
  }
  return 0;
}

function normalizePreview(value: unknown): string {
  if (typeof value !== "string") return "Untitled conversation";
  const compact = value.replace(/\s+/g, " ").trim();
  return compact.length > 0 ? compact : "Untitled conversation";
}

const CONTEXT_LIST_TIMEOUT_MS = 20_000;
const RECENT_DISPATCH_CONTEXTS_KEY = "eventConsole.recentDispatchContexts";

type RecentDispatchContextRow = {
  agentPackage: string;
  contextId: string;
  preview: string;
  latestTimestampMs: number;
};

function readRecentDispatchContexts(agentPackage: string): ConversationHistoryOption[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.sessionStorage.getItem(RECENT_DISPATCH_CONTEXTS_KEY);
    if (!raw) return [];
    const rows = JSON.parse(raw) as RecentDispatchContextRow[];
    if (!Array.isArray(rows)) return [];
    return rows
      .filter((row) => row.agentPackage === agentPackage && row.contextId)
      .map((row) => ({
        contextId: row.contextId,
        latestTimestampMs: row.latestTimestampMs,
        preview: normalizePreview(row.preview),
      }));
  } catch {
    return [];
  }
}

function rememberRecentDispatchContext(
  agentPackage: string,
  contextId: string,
  preview: string,
): void {
  if (typeof window === "undefined") return;
  const row: RecentDispatchContextRow = {
    agentPackage,
    contextId,
    preview: normalizePreview(preview),
    latestTimestampMs: Date.now(),
  };
  try {
    const raw = window.sessionStorage.getItem(RECENT_DISPATCH_CONTEXTS_KEY);
    const existing = raw ? (JSON.parse(raw) as RecentDispatchContextRow[]) : [];
    const rows = Array.isArray(existing) ? existing : [];
    const next = [
      row,
      ...rows.filter((r) => !(r.agentPackage === agentPackage && r.contextId === contextId)),
    ].slice(0, 40);
    window.sessionStorage.setItem(RECENT_DISPATCH_CONTEXTS_KEY, JSON.stringify(next));
  } catch {
    // sessionStorage may be unavailable; ignore
  }
}

/** Merge server picker rows with session-local dispatch contexts (newest first). */
export function mergeContextPickerItems(
  server: ConversationHistoryOption[],
  recent: ConversationHistoryOption[],
): ConversationHistoryOption[] {
  const seen = new Set<string>();
  const merged: ConversationHistoryOption[] = [];
  for (const item of [...recent, ...server]) {
    if (seen.has(item.contextId)) continue;
    seen.add(item.contextId);
    merged.push(item);
  }
  return merged.sort((a, b) => b.latestTimestampMs - a.latestTimestampMs);
}

export {
  readEventConsoleRouteFromUrl as readEventConsoleRoute,
  writeEventConsoleRouteToUrl as writeEventConsoleRoute,
} from "../events/operatorRoute";

export function resolveAgentFromRoute(
  agents: AgentDiscoveryEntry[],
  agentPackage: string | null,
  agentInstance: string | null,
): AgentDiscoveryEntry | undefined {
  if (!agentPackage) return undefined;
  return agents.find(
    (a) =>
      a.agent_package === agentPackage &&
      (!agentInstance || a.agent_instance_id === agentInstance),
  );
}

/** Agents that declare `discovery.subscriptions` (eligible for host dispatch). */
export function resolveSubscribedAgentFromRoute(
  agents: AgentDiscoveryEntry[],
  agentPackage: string | null,
  agentInstance: string | null,
): AgentDiscoveryEntry | undefined {
  const match = resolveAgentFromRoute(agents, agentPackage, agentInstance);
  if (!match || (match.agent_card.subscriptions?.length ?? 0) === 0) {
    return undefined;
  }
  return match;
}

function draftFingerprint(
  draft: EventPayloadDraft,
  envelope: DerivedDispatchEnvelope | null,
  selection: EventConsoleSelection,
): string {
  return JSON.stringify({
    agent_package: draft.agent_package,
    agent_instance_id: draft.agent_instance_id,
    messages: draft.messages,
    scope: draft.scope,
    message_id: draft.message_id,
    metadata: draft.metadata,
    message_shape_id: selection.messageShapeId,
    sample_id: selection.sampleId,
    envelope,
  });
}

export type EventConsoleApi = ReturnType<typeof createEventConsoleState>;

let sharedEventConsole: EventConsoleApi | null = null;

export function useEventConsole(): EventConsoleApi {
  if (!sharedEventConsole) {
    sharedEventConsole = createEventConsoleState();
  }
  return sharedEventConsole;
}

function createEventConsoleState() {
  const agents = ref<AgentDiscoveryEntry[]>([]);
  const messageShapes = ref<AgentDeliverableMessageShape[]>([]);
  const draft = ref<EventPayloadDraft>(emptyDraft());
  const selection = ref<EventConsoleSelection>(emptySelection());
  const activeMessageIndex = ref(0);
  const validation = ref<EventValidationReport | null>(null);
  const validatedFingerprint = ref<string | null>(null);
  const lastPublishOutcome = ref<EventPublishResponse | null>(null);
  const publishError = ref<string | null>(null);
  const historyItems = ref<ConversationHistoryOption[]>([]);
  const observation = ref<EventObservationState>(createInitialObservation());
  const historyLoading = ref(false);
  const historyFetchError = ref<string | null>(null);
  let historyFetchAbort: AbortController | null = null;
  const busy = ref(false);
  const historyFilterPreview = ref("");
  const lastPublishedScope = ref<PublishedScope | null>(null);
  const dispatchPhase = ref<EventDispatchPhase>("idle");

  const subscribedAgents = computed(() =>
    agents.value.filter((a) => (a.agent_card.subscriptions?.length ?? 0) > 0),
  );

  const selectedAgent = computed(() =>
    agents.value.find(
      (a) =>
        a.agent_package === draft.value.agent_package &&
        a.agent_instance_id === draft.value.agent_instance_id,
    ),
  );

  const agentAcceptsHostDispatch = computed(
    () => (selectedAgent.value?.agent_card.subscriptions?.length ?? 0) > 0,
  );

  const fleetAcceptsCurrentShape = computed(() => {
    const shape = selectedMessageShape.value;
    if (!shape) return false;
    return subscribedAgents.value.some((agent) =>
      (agent.agent_card.subscriptions ?? []).some((sub) =>
        subscriptionMatchesShape(sub, shape),
      ),
    );
  });

  const agentSubscriptions = computed(
    () => selectedAgent.value?.agent_card.subscriptions ?? [],
  );

  const availableMessageShapes = computed(() =>
    messageShapesForAgent(messageShapes.value, agentSubscriptions.value),
  );

  const shapesForSelectedSubscription = computed(() =>
    messageShapesForSubscription(
      messageShapes.value,
      agentSubscriptions.value,
      selection.value.subscriptionIndex,
    ),
  );

  const selectedMessageShape = computed(() =>
    findMessageShape(messageShapes.value, selection.value.messageShapeId),
  );

  const selectedSample = computed(() => {
    const shape = selectedMessageShape.value;
    if (!shape) return undefined;
    return findSample(shape, selection.value.sampleId);
  });

  const derivedEnvelope = computed((): DerivedDispatchEnvelope | null => {
    const shape = selectedMessageShape.value;
    if (!shape) return null;
    return deriveDispatchEnvelope(shape, selectedSample.value);
  });

  const currentFingerprint = computed(() =>
    draftFingerprint(draft.value, derivedEnvelope.value, selection.value),
  );

  const validationStale = computed(
    () =>
      Boolean(validation.value?.valid) &&
      validatedFingerprint.value !== null &&
      validatedFingerprint.value !== currentFingerprint.value,
  );

  const filteredHistoryItems = computed(() => {
    const q = historyFilterPreview.value.trim().toLowerCase();
    if (!q) return historyItems.value;
    return historyItems.value.filter(
      (item) =>
        item.contextId.toLowerCase().includes(q) ||
        item.preview.toLowerCase().includes(q),
    );
  });

  const observedContextId = computed(() => observation.value.contextId);

  const scopeResolveInput = computed(() =>
    buildObservationScopeResolveInput({
      lastPublishedScope: lastPublishedScope.value,
      draft: draft.value,
      observation: observation.value,
      validation: validation.value,
    }),
  );

  const observedScopeIds = computed((): ResolvedObservationIds =>
    resolveObservedScopeIds(scopeResolveInput.value),
  );

  async function fetchAgents(): Promise<void> {
    const res = await instanceFetch("/agents");
    if (!res.ok) {
      agents.value = [];
      return;
    }
    agents.value = (await res.json()) as AgentDiscoveryEntry[];
  }

  async function fetchMessageShapes(): Promise<void> {
    const res = await instanceFetch("/message-shapes");
    if (!res.ok) {
      messageShapes.value = [];
      return;
    }
    const body = (await res.json()) as { items: AgentDeliverableMessageShape[] };
    messageShapes.value = body.items ?? [];
  }

  function syncEventConsoleRoute(): void {
    writeEventConsoleRouteToUrl({
      agentPackage: draft.value.agent_package,
      agentInstance: draft.value.agent_instance_id,
      contextId: observation.value.contextId,
    });
  }

  /** Compose modal: New context — transcript resets on publish, not before. */
  function prepareComposeNewContext(): void {
    draft.value.scope = { kind: "new_context" };
    validation.value = null;
    validatedFingerprint.value = null;
    publishError.value = null;
  }

  /** Compose modal: continue the run currently observed in the transcript. */
  function prepareComposeContinueRun(): void {
    const ctx = observation.value.contextId;
    if (ctx) {
      draft.value.scope = composeScopeForObservedRun(ctx);
    } else {
      draft.value.scope = { kind: "new_context" };
    }
    validation.value = null;
    validatedFingerprint.value = null;
    publishError.value = null;
  }

  /** Apply agent/context from the URL (initial load or browser back/forward only). */
  function applyRouteFromUrl(): void {
    const { agentPackage, agentInstance, contextId } = readEventConsoleRouteFromUrl();
    if (agentPackage) {
      const match = resolveAgentFromRoute(agents.value, agentPackage, agentInstance);
      if (match) {
        const alreadySelected =
          draft.value.agent_package === match.agent_package &&
          draft.value.agent_instance_id === match.agent_instance_id;
        if (!alreadySelected) {
          selectAgent(match, {
            syncRoute: false,
            keepObservation: Boolean(contextId),
          });
        }
      }
    } else if (!draft.value.agent_package && subscribedAgents.value[0]) {
      selectAgent(subscribedAgents.value[0], { syncRoute: true });
    }
    if (contextId && observation.value.contextId !== contextId) {
      selectContextFromPicker(pickerOptionFromContextId(contextId), {
        syncRoute: false,
      });
    }
  }

  function selectAgent(
    agent: AgentDiscoveryEntry,
    options?: { syncRoute?: boolean; keepObservation?: boolean },
  ): void {
    draft.value.agent_package = agent.agent_package;
    draft.value.agent_instance_id = agent.agent_instance_id;
    draft.value.messages = [];
    const subs = agent.agent_card.subscriptions ?? [];
    const first = firstDeliverableShape(messageShapes.value, subs, 0);
    if (first) {
      selectSubscriptionEvent(first.message_shape_id, 0);
    } else {
      selection.value = {
        agentPackage: agent.agent_package,
        agentInstanceId: agent.agent_instance_id,
        subscriptionIndex: 0,
        messageShapeId: "",
      };
      validation.value = null;
      validatedFingerprint.value = null;
    }
    lastPublishOutcome.value = null;
    lastPublishedScope.value = null;
    if (!options?.keepObservation) {
      observation.value = createInitialObservation();
    }
    publishError.value = null;
    if (options?.syncRoute !== false) {
      syncEventConsoleRoute();
    }
    void fetchHistory();
  }

  function selectSubscriptionEvent(messageShapeId: string, subscriptionIndex: number): void {
    const shape = findMessageShape(messageShapes.value, messageShapeId);
    if (!shape) return;
    selection.value = {
      ...selection.value,
      subscriptionIndex,
      messageShapeId,
      sampleId: shape.samples[0]?.sample_id,
    };
    const { payload } = payloadFromShapeSelection(shape, selection.value.sampleId);
    draft.value.messages = [payload];
    activeMessageIndex.value = 0;
    validation.value = null;
    validatedFingerprint.value = null;
  }

  function applySample(sample: MessageShapeSample): void {
    const shape = selectedMessageShape.value;
    if (!shape) return;
    selection.value = { ...selection.value, sampleId: sample.sample_id };
    const envelope = deriveDispatchEnvelope(shape, sample);
    const base = cloneJson(sample.payload) as Record<string, unknown>;
    draft.value.messages = [autofillPayload(shape, envelope, base)];
    activeMessageIndex.value = 0;
    validation.value = null;
    validatedFingerprint.value = null;
  }

  function syncPayloadAutofill(): void {
    const shape = selectedMessageShape.value;
    const envelope = derivedEnvelope.value;
    if (!shape || !envelope || draft.value.messages.length === 0) return;
    const idx = activeMessageIndex.value;
    const current = draft.value.messages[idx];
    if (!current || typeof current !== "object" || Array.isArray(current)) return;
    const next = [...draft.value.messages];
    next[idx] = autofillPayload(shape, envelope, {
      ...(current as Record<string, unknown>),
    });
    draft.value.messages = next;
  }

  function addMessage(): void {
    const shape = selectedMessageShape.value;
    const base = shape ? defaultPayloadFromSchema(shape.payload_schema) : {};
    const envelope = derivedEnvelope.value;
    const payload =
      shape && envelope ? autofillPayload(shape, envelope, base) : base;
    draft.value.messages.push(payload);
    activeMessageIndex.value = draft.value.messages.length - 1;
    validation.value = null;
    validatedFingerprint.value = null;
  }

  function duplicateMessage(index: number): void {
    const src = draft.value.messages[index];
    if (!src) return;
    draft.value.messages.splice(index + 1, 0, cloneJson(src));
    activeMessageIndex.value = index + 1;
    validation.value = null;
    validatedFingerprint.value = null;
  }

  function removeMessage(index: number): void {
    draft.value.messages.splice(index, 1);
    activeMessageIndex.value = Math.max(0, activeMessageIndex.value - 1);
    validation.value = null;
    validatedFingerprint.value = null;
  }

  function setScope(scope: EventDispatchScope): void {
    draft.value.scope = scope;
    validation.value = null;
    validatedFingerprint.value = null;
  }

  function buildValidateBody() {
    const envelope = derivedEnvelope.value;
    if (!envelope) {
      throw new Error("Select a message type before validating");
    }
    syncPayloadAutofill();
    return {
      agent_package: draft.value.agent_package,
      agent_instance_id: draft.value.agent_instance_id,
      routing_key: envelope.routingKey,
      message_type: envelope.messageType,
      source_kind: envelope.sourceKind,
      source_key: envelope.sourceKey || undefined,
      messages: draft.value.messages,
      scope: draft.value.scope,
      message_id: draft.value.message_id || undefined,
      metadata: draft.value.metadata,
    };
  }

  const validationFocusPath = computed(() => {
    const err = validation.value?.errors[0];
    return err?.json_pointer ?? null;
  });

  function contextualizeIssue(message: string): string {
    const shape = selectedMessageShape.value;
    if (!shape) return message;
    return `${shape.display_name}: ${message}`;
  }

  const contextualizedValidation = computed(() => {
    const report = validation.value;
    if (!report) return null;
    return {
      ...report,
      errors: report.errors.map((e) => ({
        ...e,
        message: contextualizeIssue(e.message),
      })),
      warnings: report.warnings.map((w) => ({
        ...w,
        message: contextualizeIssue(w.message),
      })),
    };
  });

  function syncScopeFromPreview(): void {
    const preview = parsePreviewProducedEvent(validation.value?.preview_produced_event);
    const scope = scopeFromRecord(preview as Record<string, unknown> | null);
    if (scope) {
      lastPublishedScope.value = scope;
    }
  }

  async function validateDraft(): Promise<EventValidationReport> {
    busy.value = true;
    dispatchPhase.value = "validating";
    validation.value = null;
    try {
      if (!derivedEnvelope.value) {
        const report: EventValidationReport = {
          valid: false,
          matched_subscription: false,
          errors: [
            {
              code: "no_message_type",
              message: "Select a message type before validating.",
            },
          ],
          warnings: [],
        };
        validation.value = report;
        validatedFingerprint.value = null;
        dispatchPhase.value = "failed";
        return report;
      }
      const res = await instanceFetch("/event-dispatch/validate", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(buildValidateBody()),
      });
      const raw = (await res.json()) as Partial<EventValidationReport>;
      const report: EventValidationReport = {
        valid: Boolean(raw.valid),
        matched_subscription: Boolean(raw.matched_subscription),
        errors: raw.errors ?? [],
        warnings: raw.warnings ?? [],
        preview_produced_event: raw.preview_produced_event,
      };
      validation.value = report;
      if (report.valid) {
        syncScopeFromPreview();
        validatedFingerprint.value = currentFingerprint.value;
        dispatchPhase.value = "idle";
      } else {
        validatedFingerprint.value = null;
        dispatchPhase.value = "failed";
      }
      return report;
    } finally {
      busy.value = false;
    }
  }

  async function publishEvent(): Promise<"published" | "validation_failed" | "publish_failed"> {
    publishError.value = null;
    let previewRecord: Record<string, unknown> | undefined;
    const existingPreview = parsePreviewProducedEvent(
      validation.value?.preview_produced_event,
    );
    if (existingPreview) {
      previewRecord = existingPreview as Record<string, unknown>;
    }
    if (validationStale.value || !validation.value?.valid || !previewRecord) {
      const report = await validateDraft();
      const validated = parsePreviewProducedEvent(report.preview_produced_event);
      if (!report.valid || !validated) {
        const first = report.errors[0];
        publishError.value =
          first?.message ?? "Validation failed — fix the draft before publishing.";
        dispatchPhase.value = "failed";
        return "validation_failed";
      }
      previewRecord = validated as Record<string, unknown>;
    }

    busy.value = true;
    lastPublishOutcome.value = null;
    dispatchPhase.value = "publishing";
    try {
      syncScopeFromPreview();
      const res = await instanceFetch("/events/publish", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(previewRecord),
      });
      if (!res.ok) {
        publishError.value = await res.text();
        dispatchPhase.value = "failed";
        return "publish_failed";
      }
      const outcome = (await res.json()) as EventPublishResponse;
      lastPublishOutcome.value = outcome;
      const previewParsed = parsePreviewProducedEvent(previewRecord);
      const scope =
        (previewParsed
          ? publishedScopeFromPreview(
              previewParsed,
              draft.value.agent_package,
              draft.value.agent_instance_id,
            )
          : null) ??
        (outcome.context_id ? scopeFromRecord({ context_id: outcome.context_id }) : null);
      if (scope) {
        const unitTaskId = await resolveDispatchUnitTaskId(scope.contextId);
        lastPublishedScope.value = {
          ...scope,
          taskId: unitTaskId ?? scope.taskId ?? null,
        };
        observation.value = {
          contextId: scope.contextId,
          source: "publish",
          taskId: unitTaskId ?? scope.taskId ?? null,
        };
        const label =
          selectedMessageShape.value?.display_name ?? "Operator publish";
        rememberRecentDispatchContext(
          draft.value.agent_package,
          scope.contextId,
          label,
        );
        syncEventConsoleRoute();
      }
      dispatchPhase.value = "recording";
      void fetchHistory();
      return "published";
    } finally {
      busy.value = false;
    }
  }

  async function fetchHistory(): Promise<void> {
    if (!draft.value.agent_package) {
      historyItems.value = [];
      historyFetchError.value = null;
      historyLoading.value = false;
      return;
    }
    historyFetchAbort?.abort();
    const controller = new AbortController();
    historyFetchAbort = controller;
    historyLoading.value = true;
    historyFetchError.value = null;
    const timer = setTimeout(() => controller.abort(), CONTEXT_LIST_TIMEOUT_MS);
    try {
      const params = new URLSearchParams();
      params.set("limit", "100");
      params.set("eventOnly", "true");
      // Do not pass agentPackage here: scoped Message ops are too slow on large graphs
      // and the picker times out. Transcript/provenance reads still filter by agent.
      const res = await instanceFetch(`/contexts?${params.toString()}`, {
        signal: controller.signal,
      });
      if (!res.ok) {
        historyItems.value = [];
        historyFetchError.value = `Could not load contexts (${res.status}).`;
        return;
      }
      const payload = (await res.json()) as ContextPickerPage;
      const items = Array.isArray(payload.items) ? payload.items : [];
      const server = items.map((item) => ({
        contextId: item.contextId,
        latestTimestampMs: normalizeEpochMs(item.latestTimestampMs),
        preview: normalizePreview(item.preview),
      }));
      const recent = readRecentDispatchContexts(draft.value.agent_package);
      historyItems.value = mergeContextPickerItems(server, recent);
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") {
        if (historyFetchAbort !== controller) {
          return;
        }
        historyItems.value = [];
        historyFetchError.value = "Context list timed out — try Refresh.";
      } else if (historyFetchAbort === controller) {
        historyItems.value = [];
        historyFetchError.value = "Could not load contexts.";
      }
    } finally {
      clearTimeout(timer);
      if (historyFetchAbort === controller) {
        historyLoading.value = false;
        historyFetchAbort = null;
      }
    }
  }

  function selectContextFromPicker(
    option: ConversationHistoryOption,
    options?: { syncRoute?: boolean },
  ): void {
    observation.value = {
      contextId: option.contextId,
      source: "picker",
      taskId: null,
    };
    if (options?.syncRoute !== false) {
      syncEventConsoleRoute();
    }
  }

  /** Copy the observed event run into compose draft scope (explicit, not on picker change). */
  function applyObservedContextToDraftScope(): void {
    const ctx = observation.value.contextId;
    if (!ctx) return;
    draft.value.scope = { kind: "existing_context", context_id: ctx };
    observation.value = { ...observation.value, source: "draft" };
    validation.value = null;
    validatedFingerprint.value = null;
    lastPublishOutcome.value = null;
    publishError.value = null;
  }

  watch(shapesForSelectedSubscription, (shapes) => {
    const current = selection.value.messageShapeId;
    if (!current) {
      if (shapes[0]) {
        selectSubscriptionEvent(shapes[0].message_shape_id, selection.value.subscriptionIndex);
      }
      return;
    }
    if (!shapes.some((s) => s.message_shape_id === current) && shapes[0]) {
      selectSubscriptionEvent(shapes[0].message_shape_id, selection.value.subscriptionIndex);
    }
  });

  return {
    agents,
    messageShapes,
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
    filteredHistoryItems,
    observation,
    observedContextId,
    observedScopeIds,
    historyLoading,
    historyFetchError,
    historyFilterPreview,
    busy,
    subscribedAgents,
    selectedAgent,
    agentAcceptsHostDispatch,
    agentSubscriptions,
    availableMessageShapes,
    shapesForSelectedSubscription,
    contextualizedValidation,
    selectedMessageShape,
    selectedSample,
    derivedEnvelope,
    fetchAgents,
    fetchMessageShapes,
    applyRouteFromUrl,
    prepareComposeNewContext,
    prepareComposeContinueRun,
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
    selectContextFromPicker,
    applyObservedContextToDraftScope,
  };
}

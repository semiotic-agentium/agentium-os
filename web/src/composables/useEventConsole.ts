import { computed, ref, watch } from "vue";
import type { AgentDiscoveryEntry, ContextPickerPage, ConversationHistoryOption } from "../types/a2a";
import type {
  AgentDeliverableMessageShape,
  AgentDispatchAck,
  DerivedDispatchEnvelope,
  EventConsoleSelection,
  EventDispatchPhase,
  EventDispatchScope,
  EventPayloadDraft,
  EventValidationReport,
  MessageShapeSample,
} from "../types/events";
import {
  resolveObservationScope,
  scopeFromAck,
  scopeFromRecord,
  type DispatchedScope,
} from "../events/dispatchObserve";
import {
  autofillPayload,
  deriveDispatchEnvelope,
  findMessageShape,
  findSample,
  firstDeliverableShape,
  messageShapesForAgent,
  messageShapesForSubscription,
  payloadFromShapeSelection,
} from "../events/messageShapes";
import { defaultPayloadFromSchema } from "../events/schemaForm";

const OPERATOR_ORIGIN = "operator-eval-console";

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

export function readEventConsoleRoute(): {
  agentPackage: string | null;
  agentInstance: string | null;
  contextId: string | null;
} {
  const params = new URLSearchParams(window.location.search);
  return {
    agentPackage: params.get("agentPackage"),
    agentInstance: params.get("agentInstance"),
    contextId: params.get("contextId"),
  };
}

/** Keep the events deep link in sync when the operator changes agent or context in the UI. */
export function writeEventConsoleRoute(patch: {
  agentPackage?: string;
  agentInstance?: string;
  contextId?: string | null;
}): void {
  if (typeof window === "undefined") return;
  const url = new URL(window.location.href);
  if (patch.agentPackage !== undefined) {
    if (patch.agentPackage) {
      url.searchParams.set("agentPackage", patch.agentPackage);
    } else {
      url.searchParams.delete("agentPackage");
    }
  }
  if (patch.agentInstance !== undefined) {
    if (patch.agentInstance) {
      url.searchParams.set("agentInstance", patch.agentInstance);
    } else {
      url.searchParams.delete("agentInstance");
    }
  }
  if (patch.contextId !== undefined) {
    if (patch.contextId) {
      url.searchParams.set("contextId", patch.contextId);
    } else {
      url.searchParams.delete("contextId");
    }
  }
  window.history.replaceState(window.history.state, "", url.toString());
}

/** @deprecated Use readEventConsoleRoute */
export function readEventConsoleAgentRoute(): {
  agentPackage: string | null;
  agentInstance: string | null;
} {
  const route = readEventConsoleRoute();
  return { agentPackage: route.agentPackage, agentInstance: route.agentInstance };
}

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

export function useEventConsole() {
  const mode = ref<"compose" | "history">("compose");
  const agents = ref<AgentDiscoveryEntry[]>([]);
  const messageShapes = ref<AgentDeliverableMessageShape[]>([]);
  const draft = ref<EventPayloadDraft>(emptyDraft());
  const selection = ref<EventConsoleSelection>(emptySelection());
  const activeMessageIndex = ref(0);
  const validation = ref<EventValidationReport | null>(null);
  const validatedFingerprint = ref<string | null>(null);
  const lastAck = ref<AgentDispatchAck | null>(null);
  const dispatchError = ref<string | null>(null);
  const historyItems = ref<ConversationHistoryOption[]>([]);
  const selectedContextId = ref<string | null>(null);
  const historyLoading = ref(false);
  const historyFetchError = ref<string | null>(null);
  let historyFetchAbort: AbortController | null = null;
  const busy = ref(false);
  const historyFilterPreview = ref("");
  const lastDispatchedScope = ref<DispatchedScope | null>(null);
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

  const activeRunSummary = computed(() => {
    const agent = selectedAgent.value;
    const shape = selectedMessageShape.value;
    const scope = resolveObservationScope({
      lastDispatchedScope: lastDispatchedScope.value,
      draftScope: draft.value.scope,
      selectedContextId: selectedContextId.value,
      previewRequest: validation.value?.preview_request as Record<string, unknown> | undefined,
      currentAgent: agent
        ? {
            agentPackage: agent.agent_package,
            agentInstanceId: agent.agent_instance_id,
          }
        : null,
      mode: mode.value,
    });
    let statusLabel = "";
    if (dispatchError.value) statusLabel = "Failed";
    else if (lastAck.value) {
      statusLabel = lastAck.value.accepted ? "Accepted" : "Rejected";
    } else if (validation.value?.valid && !validationStale.value) {
      statusLabel = "Validated";
    }
    return {
      agentLabel: agent ? `${agent.agent_package}/${agent.agent_instance_id}` : null,
      messageTypeLabel: shape?.display_name ?? null,
      contextId: scope?.contextId ?? null,
      taskId: scope?.taskId ?? null,
      statusLabel,
      phase: dispatchPhase.value,
    };
  });

  async function fetchAgents(): Promise<void> {
    const res = await fetch("/agents");
    if (!res.ok) {
      agents.value = [];
      return;
    }
    agents.value = (await res.json()) as AgentDiscoveryEntry[];
  }

  async function fetchMessageShapes(): Promise<void> {
    const res = await fetch("/message-shapes");
    if (!res.ok) {
      messageShapes.value = [];
      return;
    }
    const body = (await res.json()) as { items: AgentDeliverableMessageShape[] };
    messageShapes.value = body.items ?? [];
  }

  function syncEventConsoleRoute(): void {
    writeEventConsoleRoute({
      agentPackage: draft.value.agent_package,
      agentInstance: draft.value.agent_instance_id,
      contextId: selectedContextId.value,
    });
  }

  /** Apply agent/context from the URL (initial load or browser back/forward only). */
  function applyRouteFromUrl(): void {
    const { agentPackage, agentInstance, contextId } = readEventConsoleRoute();
    if (agentPackage) {
      const match = resolveAgentFromRoute(agents.value, agentPackage, agentInstance);
      if (match) {
        const alreadySelected =
          draft.value.agent_package === match.agent_package &&
          draft.value.agent_instance_id === match.agent_instance_id;
        if (!alreadySelected) {
          selectAgent(match, { syncRoute: false });
        }
      }
    }
    if (contextId && selectedContextId.value !== contextId) {
      selectContextFromPicker(
        { contextId, latestTimestampMs: 0, preview: "" },
        { syncRoute: false },
      );
    }
  }

  function selectAgent(
    agent: AgentDiscoveryEntry,
    options?: { syncRoute?: boolean },
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
    lastAck.value = null;
    lastDispatchedScope.value = null;
    selectedContextId.value = null;
    dispatchError.value = null;
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
    const preview = validation.value?.preview_request as Record<string, unknown> | undefined;
    const scope = scopeFromRecord(preview);
    if (scope) {
      lastDispatchedScope.value = scope;
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
      const res = await fetch("/event-dispatch/validate", {
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
        preview_request: raw.preview_request,
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

  async function dispatchEvent(): Promise<void> {
    let preview = validation.value?.preview_request as Record<string, unknown> | undefined;
    if (validationStale.value || !validation.value?.valid || !preview) {
      const report = await validateDraft();
      if (!report.valid || !report.preview_request) {
        const first = report.errors[0];
        dispatchError.value =
          first?.message ?? "Validation failed — fix the draft before dispatching.";
        dispatchPhase.value = "failed";
        return;
      }
      preview = report.preview_request as Record<string, unknown>;
    }

    busy.value = true;
    dispatchError.value = null;
    lastAck.value = null;
    dispatchPhase.value = "dispatching";
    try {
      syncScopeFromPreview();
      const url = `/agents/${draft.value.agent_package}/${draft.value.agent_instance_id}/dispatch`;
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(preview),
      });
      if (!res.ok) {
        dispatchError.value = await res.text();
        dispatchPhase.value = "failed";
        return;
      }
      const ack = (await res.json()) as AgentDispatchAck;
      lastAck.value = ack;
      const ackScope = scopeFromAck(ack);
      if (ackScope) {
        lastDispatchedScope.value = {
          ...ackScope,
          agentPackage: draft.value.agent_package,
          agentInstanceId: draft.value.agent_instance_id,
        };
        selectedContextId.value = ackScope.contextId;
        const preview =
          selectedMessageShape.value?.display_name ??
          ack.summary ??
          "Operator dispatch";
        rememberRecentDispatchContext(
          draft.value.agent_package,
          ackScope.contextId,
          preview,
        );
        syncEventConsoleRoute();
      }
      dispatchPhase.value = "recording";
      await fetchHistory();
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
      // Do not pass agentPackage here: scoped Message ops are too slow on large graphs
      // and the picker times out. Transcript/provenance reads still filter by agent.
      const res = await fetch(`/contexts?${params.toString()}`, {
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

  function selectHistoryContext(contextId: string): void {
    selectedContextId.value = contextId;
  }

  function selectContextFromPicker(
    option: ConversationHistoryOption,
    options?: { syncRoute?: boolean },
  ): void {
    selectHistoryContext(option.contextId);
    const scope = draft.value.scope;
    if (scope.kind === "existing_task") {
      setScope({
        kind: "existing_task",
        context_id: option.contextId,
        task_id: scope.task_id,
      });
    } else {
      setScope({ kind: "existing_context", context_id: option.contextId });
    }
    validation.value = null;
    validatedFingerprint.value = null;
    if (options?.syncRoute !== false) {
      syncEventConsoleRoute();
    }
  }

  function useContextAsDraftScope(): void {
    const ctx = selectedContextId.value;
    if (!ctx) return;
    draft.value.scope = { kind: "existing_context", context_id: ctx };
    mode.value = "compose";
    validation.value = null;
    validatedFingerprint.value = null;
    lastAck.value = null;
    dispatchError.value = null;
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

  function observeContextFromDraftOrFlow(): {
    contextId: string | null;
    taskId: string | null;
  } {
    const preview = validation.value?.preview_request as Record<string, unknown> | undefined;
    const resolved = resolveObservationScope({
      lastDispatchedScope: lastDispatchedScope.value,
      draftScope: draft.value.scope,
      selectedContextId: selectedContextId.value,
      previewRequest: preview,
      currentAgent:
        draft.value.agent_package && draft.value.agent_instance_id
          ? {
              agentPackage: draft.value.agent_package,
              agentInstanceId: draft.value.agent_instance_id,
            }
          : null,
      mode: mode.value,
    });
    if (!resolved) {
      return { contextId: null, taskId: null };
    }
    return {
      contextId: resolved.contextId,
      taskId: resolved.taskId ?? null,
    };
  }

  return {
    mode,
    agents,
    messageShapes,
    draft,
    selection,
    activeMessageIndex,
    validation,
    validationStale,
    validationFocusPath,
    lastAck,
    lastDispatchedScope,
    dispatchPhase,
    dispatchError,
    historyItems,
    filteredHistoryItems,
    selectedContextId,
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
    activeRunSummary,
    fetchAgents,
    fetchMessageShapes,
    applyRouteFromUrl,
    selectAgent,
    selectSubscriptionEvent,
    applySample,
    addMessage,
    duplicateMessage,
    removeMessage,
    setScope,
    validateDraft,
    dispatchEvent,
    fetchHistory,
    selectHistoryContext,
    selectContextFromPicker,
    useContextAsDraftScope,
    observeContextFromDraftOrFlow,
  };
}

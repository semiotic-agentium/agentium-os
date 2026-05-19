import { computed, reactive, ref, type Ref } from "vue";
import type { AgentDiscoveryEntry } from "../types/a2a";
import { filterEventCapableAgents } from "./agentFilter";
import {
  buildDispatchRequest,
  buildDispatchRequestPreview,
  previewJson,
  SCOPE_EXISTING_CONTEXT,
  SCOPE_NEW_CONTEXT,
  type EventDispatchScope,
} from "./dispatchRequest";
import {
  EVENT_SAMPLES,
  findSampleById,
  type AgentDispatchRequestBody,
  type EventSample,
} from "./sampleCatalog";

/** Last dispatch outcome for the result panel. */
export interface DispatchOutcome {
  status: "accepted" | "rejected" | "error";
  httpStatus?: number;
  detail?: string;
  targetPackage: string;
  targetInstanceId: string;
  routingKey: string;
  messageType: string;
  contextId?: string;
  taskId?: string;
  messageId?: string;
  /** Wall-clock timestamp the dispatch returned (ms). */
  finishedAt: number;
}

export interface UseEventConsoleArgs {
  /** All agents from GET /agents — filtered to event-capable agents internally. */
  agents: Ref<ReadonlyArray<AgentDiscoveryEntry>>;
}

export function useEventConsole(args: UseEventConsoleArgs) {
  const defaultSample = EVENT_SAMPLES[0] ?? null;

  const selectedAgentKey = ref<string | null>(null);
  const selectedSampleId = ref<string>(defaultSample?.id ?? "");
  const messagesJsonText = ref<string>(
    defaultSample ? previewJson(defaultSample.messages) : "[]",
  );
  const scope = reactive<{ kind: EventDispatchScope["kind"] }>({ kind: SCOPE_NEW_CONTEXT });
  const continueContextId = ref<string>("");
  const operatorNote = ref<string>("");

  const isDispatching = ref(false);
  const outcome = ref<DispatchOutcome | null>(null);

  function agentKey(a: AgentDiscoveryEntry): string {
    return `${a.agent_package}/${a.agent_instance_id}`;
  }

  const eventCapableAgents = computed(() => filterEventCapableAgents(args.agents.value));

  const selectedAgent = computed<AgentDiscoveryEntry | null>(() => {
    const key = selectedAgentKey.value;
    if (!key) return null;
    return eventCapableAgents.value.find((a) => agentKey(a) === key) ?? null;
  });

  const selectedSample = computed<EventSample | null>(() =>
    findSampleById(selectedSampleId.value),
  );

  /** Parsed messages — Error.message exposes the JSON parse error for the validation strip. */
  const parsedMessages = computed<{ ok: true; value: unknown[] } | { ok: false; error: string }>(
    () => {
      const text = messagesJsonText.value.trim();
      if (!text) return { ok: true, value: [] };
      let parsed: unknown;
      try {
        parsed = JSON.parse(text);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        return { ok: false, error: msg };
      }
      if (!Array.isArray(parsed)) {
        return { ok: false, error: "messages JSON must be a top-level array" };
      }
      return { ok: true, value: parsed };
    },
  );

  /** Concrete scope object used by buildDispatchRequest. */
  const effectiveScope = computed<EventDispatchScope>(() => {
    if (scope.kind === SCOPE_EXISTING_CONTEXT) {
      const id = continueContextId.value.trim();
      if (id) return { kind: SCOPE_EXISTING_CONTEXT, contextId: id };
    }
    return { kind: SCOPE_NEW_CONTEXT };
  });

  /**
   * Preview body for the operator. Uses sentinel placeholders for host-minted
   * fields so the preview is stable across keystrokes and cannot diverge from
   * what `dispatch()` will actually send.
   */
  const previewText = computed<string>(() => {
    const sample = selectedSample.value;
    if (!sample) return "";
    const parsed = parsedMessages.value;
    if (!parsed.ok) return "";
    const body = buildDispatchRequestPreview({
      sample,
      messages: parsed.value,
      scope: effectiveScope.value,
      note: operatorNote.value,
    });
    return previewJson(body);
  });

  /** Operator-facing message when {@link previewText} is empty. */
  const previewPlaceholder = computed<string | null>(() => {
    if (!selectedSample.value) return "Pick a sample to preview the request.";
    if (!parsedMessages.value.ok) return "Fix the JSON above to see the preview.";
    return null;
  });

  function canDispatch(): boolean {
    if (isDispatching.value) return false;
    if (!selectedAgent.value) return false;
    if (!selectedSample.value) return false;
    if (!parsedMessages.value.ok) return false;
    if (scope.kind === SCOPE_EXISTING_CONTEXT && !continueContextId.value.trim()) {
      return false;
    }
    return true;
  }

  /**
   * Change which sample's routing_key / message_type is active. Does not
   * touch the editor body — operator edits are preserved so an incident
   * payload can be re-routed under a different sample's wrapper. Use
   * {@link loadSampleIntoEditor} to explicitly reset the editor.
   */
  function selectSample(id: string): void {
    selectedSampleId.value = id;
  }

  /** Replace editor JSON with the canonical payload of the named (or current) sample. */
  function loadSampleIntoEditor(id?: string): void {
    const target = id ?? selectedSampleId.value;
    selectedSampleId.value = target;
    const sample = findSampleById(target);
    messagesJsonText.value = sample ? previewJson(sample.messages) : "[]";
  }

  function selectAgent(key: string): void {
    selectedAgentKey.value = key;
  }

  function outcomeFor(
    agent: AgentDiscoveryEntry,
    body: AgentDispatchRequestBody,
    overrides: Pick<DispatchOutcome, "status" | "httpStatus" | "detail">,
  ): DispatchOutcome {
    return {
      ...overrides,
      targetPackage: agent.agent_package,
      targetInstanceId: agent.agent_instance_id,
      routingKey: body.routing_key,
      messageType: body.message_type,
      contextId: body.context_id,
      // Reserved: AgentDispatchAckDto does not yet carry task_id; mirrors the
      // request shape so future ack extensions populate this without UI churn.
      taskId: body.task_id,
      messageId: body.message_id,
      finishedAt: Date.now(),
    };
  }

  async function dispatch(): Promise<DispatchOutcome | null> {
    const agent = selectedAgent.value;
    const sample = selectedSample.value;
    const parsed = parsedMessages.value;
    if (!agent || !sample || !parsed.ok) return null;
    isDispatching.value = true;
    try {
      const body = buildDispatchRequest({
        sample,
        messages: parsed.value,
        scope: effectiveScope.value,
        note: operatorNote.value,
      });
      const url = `/agents/${encodeURIComponent(agent.agent_package)}/${encodeURIComponent(
        agent.agent_instance_id,
      )}/dispatch`;
      let res: Response;
      try {
        res = await fetch(url, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });
      } catch (e) {
        const detail = e instanceof Error ? e.message : String(e);
        const result = outcomeFor(agent, body, { status: "error", detail });
        outcome.value = result;
        return result;
      }

      const bodyText = await res.text();
      if (!res.ok) {
        const result = outcomeFor(agent, body, {
          status: "error",
          httpStatus: res.status,
          detail: bodyText || res.statusText,
        });
        outcome.value = result;
        return result;
      }

      let parsedAck: { accepted?: boolean; detail?: string } = {};
      try {
        parsedAck = bodyText ? JSON.parse(bodyText) : {};
      } catch {
        parsedAck = {};
      }
      const result = outcomeFor(agent, body, {
        status: parsedAck.accepted === true ? "accepted" : "rejected",
        httpStatus: res.status,
        detail: parsedAck.detail,
      });
      outcome.value = result;
      return result;
    } finally {
      isDispatching.value = false;
    }
  }

  return {
    // Selection state
    selectedAgentKey,
    selectedSampleId,
    messagesJsonText,
    scope,
    continueContextId,
    operatorNote,

    // Derived
    eventCapableAgents,
    selectedAgent,
    selectedSample,
    parsedMessages,
    previewText,
    previewPlaceholder,
    canDispatch,
    agentKey,

    // Actions
    selectAgent,
    selectSample,
    loadSampleIntoEditor,
    dispatch,

    // Result
    isDispatching,
    outcome,
  };
}

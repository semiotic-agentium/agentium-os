import { ref, computed, type Ref } from "vue";
import {
  applyConversationHistoryDelta,
  applyConversationHistoryPage,
  liveAgentBubbleBlocksHistoryReplace,
  provenanceSnapshotLagsLiveChat,
  shouldDeferProvenanceHistoryRebuild,
} from "../chat/conversationHistoryHydration";
import { normalizeEpochMs, normalizePreview } from "../chat/chatTime";
import {
  ensureContentBlocks,
  partsToContentBlocks,
  pushStructuredBlocks,
  pushTextBlock,
} from "../chat/chatMessageBlocks";
import { appendExecutionErrorCard, parseExecutionErrorText } from "../chat/executionErrorCard";
import {
  deriveToolStatus,
  detectToolAppendMode,
  getOrCreateToolBlockForAppend,
} from "../chat/toolBlocks";
import {
  pushToolEventsDeduped,
  withSessionStepDetailEvents,
} from "../chat/toolNotificationEvents";
import { isSyntheticInputRequiredPrompt } from "../chat/inputRequiredUi";
import { shouldSuppressAgentTranscriptText } from "../chat/workflowUiFilters";
import type {
  AgentDiscoveryEntry,
  A2aMessage,
  ChatMessage,
  ConversationHistoryPage,
  ContextPickerPage,
  ContextMetricsResponse,
  JSONRPCResponse,
  ChunkPayload,
  ToolCompletion,
  ToolEvent,
  WorkflowProgressState,
  ConversationHistoryOption,
  HistoryHydrateState,
  LlmPromptOperation,
} from "../types/a2a";

let counter = 0;
function nextId(prefix: string): string {
  return `${prefix}-${Date.now()}-${++counter}`;
}

/** Extract `data:` payloads from one SSE event block (between blank-line separators). */
function sseEventDataPayload(block: string): string | null {
  const lines = block.split("\n");
  const parts: string[] = [];
  for (const line of lines) {
    const t = line.trimEnd();
    if (t.startsWith("data:")) {
      parts.push(t.slice(5).trimStart());
    }
  }
  if (parts.length === 0) return null;
  return parts.join("\n");
}

/**
 * Parse a full `text/event-stream` body into JSON-RPC objects (aligned with
 * `baml_rt_core::parse_a2a_sse_json_rpc_chunks`). We use `response.text()` rather than
 * ReadableStream parsing so dev proxies (Vite) and gzip cannot break SSE framing.
 */
function parseA2aSseJsonRpcBody(body: string): JSONRPCResponse[] {
  const normalized = body.replace(/\r\n/g, "\n");
  const out: JSONRPCResponse[] = [];
  for (const rawEvent of normalized.split("\n\n")) {
    const trimmed = rawEvent.trim();
    if (!trimmed) continue;
    const payload = sseEventDataPayload(trimmed);
    if (!payload?.trim()) continue;
    out.push(JSON.parse(payload) as JSONRPCResponse);
  }
  return out;
}

function sortLlmPromptOperations(a: LlmPromptOperation, b: LlmPromptOperation): number {
  return a.eventOrder - b.eventOrder || a.activityAnchor.localeCompare(b.activityAnchor);
}

function mergeLlmPromptOperations(
  prev: LlmPromptOperation[],
  incoming: LlmPromptOperation[] | undefined,
): LlmPromptOperation[] {
  if (!incoming?.length) return prev;
  const m = new Map(prev.map((o) => [o.activityAnchor, o]));
  for (const o of incoming) {
    m.set(o.activityAnchor, o);
  }
  return [...m.values()].sort(sortLlmPromptOperations);
}

function updateMessage(
  messages: Ref<ChatMessage[]>,
  id: string,
  updater: (msg: ChatMessage) => void,
): void {
  const idx = messages.value.findIndex((m) => m.id === id);
  if (idx !== -1) {
    updater(messages.value[idx]!);
  }
}

function tryApplyStructuredMessage(msg: ChatMessage, wire: A2aMessage): boolean {
  const parts = wire.parts;
  if (!parts?.length) return false;
  const structBlocks = partsToContentBlocks(parts);
  const hasMetadata = wire.metadata && Object.keys(wire.metadata).length > 0;
  const useStructured =
    structBlocks.length > 1 ||
    structBlocks.some((b) => b.type === "data") ||
    parts.length > 1 ||
    hasMetadata;
  if (!useStructured || structBlocks.length === 0) return false;
  ensureContentBlocks(msg);
  pushStructuredBlocks(msg, structBlocks);
  if (wire.metadata && typeof wire.metadata === "object") {
    msg.metadata = { ...(msg.metadata ?? {}), ...wire.metadata };
  }
  return true;
}

export function useA2aClient() {
  const agents: Ref<AgentDiscoveryEntry[]> = ref([]);
  const selectedAgent: Ref<AgentDiscoveryEntry | null> = ref(null);
  const messages: Ref<ChatMessage[]> = ref([]);
  const isLoading = ref(false);

  // Multi-turn conversation state
  const _contextId = ref<string | undefined>();
  const _taskId = ref<string | null>(null);

  // Context metrics (fetched after each response)
  const contextMetrics = ref<ContextMetricsResponse | null>(null);
  /** From conversation-history SSE/HTTP: temporal tail + per-LLM prompt JSON sizes */
  const llmPromptOperations = ref<LlmPromptOperation[]>([]);
  const promptContextBytesSessionCurrent = ref<number | null>(null);
  const conversationHistoryOptions = ref<ConversationHistoryOption[]>([]);
  const selectedHistoryContextId = ref<string | null>(null);
  const historyLoading = ref(false);
  /** Primary transcript restore: loading / error / skipped vs ready (see ChatWindow empty states). */
  const historyHydrateState = ref<HistoryHydrateState>("idle");

  // Workflow progress tracker (parsed from coordinator SSE progress messages)
  const workflowProgress = ref<WorkflowProgressState>({
    phase: "idle",
    nodes: [],
    completedNodes: [],
  });

  // Stream cancellation
  let _abortController: AbortController | null = null;
  let _historyStream: EventSource | null = null;
  let _historyStreamKey = "";
  let _historyVersion = "";

  // Provenance diagram source (raw mermaid text fetched after each response)
  const provenanceDiagram = ref<string>("");
  /** Monotonic id so slower diagram HTTP responses cannot overwrite newer ones */
  let diagramFetchSeq = 0;

  function replaceLlmPromptStateFromPage(page: ConversationHistoryPage): void {
    llmPromptOperations.value = [...(page.llmPromptOperations ?? [])].sort(sortLlmPromptOperations);
    promptContextBytesSessionCurrent.value = page.promptContextBytesSessionCurrent ?? null;
  }

  function extendLlmPromptStateFromPage(page: ConversationHistoryPage): void {
    llmPromptOperations.value = mergeLlmPromptOperations(llmPromptOperations.value, page.llmPromptOperations);
    if (page.promptContextBytesSessionCurrent != null) {
      promptContextBytesSessionCurrent.value = page.promptContextBytesSessionCurrent;
    }
  }

  /** Incremented (debounced) when SSE implies new provenance rows; ProvenancePane watches this */
  const traceRefreshGeneration = ref(0);
  let traceRefreshDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  /** Dedupe task state bumps within a single stream */
  let lastSeenTaskState: string | undefined;
  const TRACE_REFRESH_DEBOUNCE_MS = 300;

  /** One-shot retry when hydration is skipped because provenance lags behind the live stream. */
  let pendingHydrateRetryTimer: ReturnType<typeof setTimeout> | null = null;

  function scheduleTraceRefreshBump(): void {
    if (!_contextId.value) return;
    if (traceRefreshDebounceTimer !== null) clearTimeout(traceRefreshDebounceTimer);
    traceRefreshDebounceTimer = setTimeout(() => {
      traceRefreshDebounceTimer = null;
      traceRefreshGeneration.value += 1;
      // Edge-trigger Mermaid refresh from evented provenance updates (no periodic UI polling).
      void fetchProvenanceDiagram();
    }, TRACE_REFRESH_DEBOUNCE_MS);
  }

  function closeConversationHistoryStream(): void {
    if (_historyStream) {
      _historyStream.close();
      _historyStream = null;
    }
    _historyStreamKey = "";
  }

  function ensureConversationHistoryStream(): void {
    if (!_contextId.value) return;
    const key = _contextId.value;
    if (_historyStreamKey === key && _historyStream) return;

    closeConversationHistoryStream();
    const params = new URLSearchParams();
    params.set("limit", "500");
    const url = `/contexts/${_contextId.value}/conversation-history/stream?${params.toString()}`;
    const stream = new EventSource(url);

    stream.addEventListener("snapshot", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        if (page.version === _historyVersion) return;
        if (shouldDeferProvenanceHistoryRebuild(messages.value, page)) return;
        applyConversationHistoryPage(messages, page);
        replaceLlmPromptStateFromPage(page);
        _historyVersion = page.version;
        selectedHistoryContextId.value = page.contextId;
        historyHydrateState.value = "ready";
      } catch {
        // Ignore malformed stream event payloads.
      }
    });
    stream.addEventListener("delta", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        if (page.version === _historyVersion) return;
        if (shouldDeferProvenanceHistoryRebuild(messages.value, page)) return;
        applyConversationHistoryDelta(messages, page);
        extendLlmPromptStateFromPage(page);
        _historyVersion = page.version;
        selectedHistoryContextId.value = page.contextId;
        historyHydrateState.value = "ready";
      } catch {
        // Ignore malformed stream event payloads.
      }
    });
    stream.addEventListener("done", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        _historyVersion = page.version;
      } catch {
        // Ignore malformed stream event payloads.
      } finally {
        stream.close();
        if (_historyStream === stream) {
          _historyStream = null;
          _historyStreamKey = "";
        }
      }
    });
    stream.onerror = () => {
      // Keep the current transcript; caller can re-open on next state transition.
      if (_historyStream === stream) {
        stream.close();
        _historyStream = null;
        _historyStreamKey = "";
      }
    };

    _historyStream = stream;
    _historyStreamKey = key;
  }

  async function fetchAgents(): Promise<void> {
    try {
      const res = await fetch("/agents");
      if (!res.ok) {
        agents.value = [];
        selectedAgent.value = null;
        await fetchConversationHistoryOptions();
        return;
      }
      const data: unknown = await res.json();
      if (!Array.isArray(data)) {
        agents.value = [];
        selectedAgent.value = null;
        await fetchConversationHistoryOptions();
        return;
      }
      agents.value = data as AgentDiscoveryEntry[];
      if (agents.value.length > 0 && !selectedAgent.value) {
        selectedAgent.value = agents.value[0] ?? null;
      }
      await fetchConversationHistoryOptions();
    } catch {
      agents.value = [];
      selectedAgent.value = null;
      conversationHistoryOptions.value = [];
    }
  }

  async function fetchConversationHistoryOptions(): Promise<void> {
    if (!selectedAgent.value) {
      conversationHistoryOptions.value = [];
      selectedHistoryContextId.value = null;
      return;
    }
    historyLoading.value = true;
    try {
      const params = new URLSearchParams();
      params.set("limit", "100");
      const response = await fetch(`/contexts?${params.toString()}`);
      if (!response.ok) {
        conversationHistoryOptions.value = [];
        return;
      }
      const payload = (await response.json()) as ContextPickerPage;
      const items = Array.isArray(payload.items) ? payload.items : [];
      conversationHistoryOptions.value = items
        .map((item) => ({
          contextId: item.contextId,
          latestTimestampMs: normalizeEpochMs(item.latestTimestampMs),
          preview: normalizePreview(item.preview),
        }))
        .sort((a, b) => b.latestTimestampMs - a.latestTimestampMs);
    } catch {
      conversationHistoryOptions.value = [];
    } finally {
      historyLoading.value = false;
    }
  }

  function selectAgent(agent: AgentDiscoveryEntry): void {
    selectedAgent.value = agent;
    historyHydrateState.value = "idle";
    messages.value = [];
    _contextId.value = undefined;
    _taskId.value = null;
    provenanceDiagram.value = "";
    contextMetrics.value = null;
    workflowProgress.value = { phase: "idle", nodes: [], completedNodes: [] };
    lastSeenTaskState = undefined;
    if (traceRefreshDebounceTimer !== null) {
      clearTimeout(traceRefreshDebounceTimer);
      traceRefreshDebounceTimer = null;
    }
    closeConversationHistoryStream();
    _historyVersion = "";
    llmPromptOperations.value = [];
    promptContextBytesSessionCurrent.value = null;
    conversationHistoryOptions.value = [];
    selectedHistoryContextId.value = null;
    void fetchConversationHistoryOptions();
  }

  function updateWorkflowPhase(text: string): void {
    if (/discovering available/i.test(text)) {
      workflowProgress.value = {
        phase: "discovery",
        nodes: [],
        completedNodes: workflowProgress.value.completedNodes,
        pipelineActive: true,
      };
    } else if (/planning workflow/i.test(text)) {
      const iterMatch = text.match(/iteration\s+(\d+)/i);
      workflowProgress.value = {
        phase: "planning",
        iteration: iterMatch ? parseInt(iterMatch[1]!, 10) : undefined,
        nodes: [],
        completedNodes: workflowProgress.value.completedNodes,
        pipelineActive: true,
      };
    } else if (/executing\s+\d+\s+workflow\s+node/i.test(text)) {
      const nodeListMatch = text.match(/node\(s\):\s*(.+)/i);
      const nodeNames = nodeListMatch
        ? nodeListMatch[1]!
            .split(",")
            .map((n) => n.trim())
            .filter((n) => n.length > 0)
        : [];
      const prev = workflowProgress.value;
      workflowProgress.value = {
        phase: "execution",
        nodes: nodeNames.map((name) => ({
          name,
          status: prev.completedNodes.includes(name)
            ? ("completed" as const)
            : ("running" as const),
        })),
        completedNodes: prev.completedNodes,
      };
    } else if (/synthesiz/i.test(text) || /compiling final/i.test(text)) {
      workflowProgress.value = {
        phase: "synthesis",
        nodes: [],
        completedNodes: workflowProgress.value.completedNodes,
      };
    }
  }

  function markWorkflowNodeCompleted(toolName: string): void {
    const wp = workflowProgress.value;
    if (wp.phase !== "execution") return;
    const node = wp.nodes.find((n) => toolName.toLowerCase().includes(n.name.toLowerCase()));
    if (node && node.status !== "completed") {
      node.status = "completed";
      if (!wp.completedNodes.includes(node.name)) {
        wp.completedNodes.push(node.name);
      }
    }
  }

  async function sendMessage(text: string): Promise<void> {
    if (!selectedAgent.value || !text.trim()) return;

    if (pendingHydrateRetryTimer !== null) {
      clearTimeout(pendingHydrateRetryTimer);
      pendingHydrateRetryTimer = null;
    }

    // Clear input-required state on the last agent message when user replies (resume turn)
    const lastAgent = [...messages.value].reverse().find((m) => m.role === "agent");
    if (lastAgent?.awaitingInput) {
      lastAgent.awaitingInput = false;
      lastAgent.inputRequiredPrompt = undefined;
    }

    const agent = selectedAgent.value;
    const url = `/agents/${agent.agent_package}/${agent.agent_instance_id}/a2a`;

    // Add user message
    messages.value.push({
      id: nextId("user-msg"),
      role: "user",
      speakerKind: "human",
      text: text.trim(),
      timestamp: new Date(),
    });

    // Build JSON-RPC request
    const message: Record<string, unknown> = {
      messageId: nextId("ui-msg"),
      role: "user",
      parts: [{ text: text.trim() }],
    };
    if (!_contextId.value) {
      _contextId.value = nextId("ctx");
      selectedHistoryContextId.value = _contextId.value;
    }
    if (_contextId.value) message.contextId = _contextId.value;
    if (_taskId.value) message.taskId = _taskId.value;
    ensureConversationHistoryStream();

    const request = {
      jsonrpc: "2.0",
      id: nextId("corr"),
      method: "message.sendStream",
      params: { message },
    };

    isLoading.value = true;
    workflowProgress.value = { phase: "idle", nodes: [], completedNodes: [] };
    lastSeenTaskState = undefined;

    // Placeholder for streaming agent response (contentBlocks updated incrementally from stream)
    const agentMsgId = nextId("agent-msg");
    messages.value.push({
      id: agentMsgId,
      role: "agent",
      text: "",
      timestamp: new Date(),
      isStreaming: true,
      contentBlocks: [],
    });

    const controller = new AbortController();
    _abortController = controller;

    try {
      const response = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
        signal: controller.signal,
      });

      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }

      const ct = response.headers.get("content-type") ?? "";
      const bodyText = await response.text();
      if (bodyText.trimStart().startsWith("<")) {
        throw new Error(
          `Unexpected HTML from /a2a — proxy or runner URL misconfigured (content-type: ${ct || "missing"})`,
        );
      }
      if (!ct.toLowerCase().includes("text/event-stream") && bodyText.trim().length > 0) {
        console.warn(`POST /a2a unexpected Content-Type: ${ct || "(missing)"}; parsing as SSE anyway`);
      }
      const events = parseA2aSseJsonRpcBody(bodyText);
      if (events.length === 0 && bodyText.trim().length > 0) {
        throw new Error(
          `No JSON-RPC events parsed from /a2a body (content-type: ${ct || "missing"})`,
        );
      }
      for (const event of events) {
        processEvent(event, agentMsgId);
      }
      updateMessage(messages, agentMsgId, (msg) => {
        msg.isStreaming = false;
      });
      const awaitingResume =
        messages.value.find((m) => m.id === agentMsgId)?.awaitingInput === true;
      await Promise.all([
        awaitingResume
          ? Promise.resolve()
          : hydrateMessagesFromConversationHistory(undefined, { quiet: true }),
        fetchProvenanceDiagram(),
        fetchContextMetrics(),
        fetchConversationHistoryOptions(),
      ]);
    } catch (err) {
      // AbortError is expected when the user cancels — cancelStream() already handled state
      if (err instanceof DOMException && err.name === "AbortError") return;
      updateMessage(messages, agentMsgId, (msg) => {
        msg.text = `Error: ${err}`;
        msg.isStreaming = false;
      });
    } finally {
      _abortController = null;
      isLoading.value = false;
    }
  }

  function processEvent(event: JSONRPCResponse, agentMsgId: string): void {
    if (event.error) {
      updateMessage(messages, agentMsgId, (msg) => {
        msg.text = `Error: ${event.error!.message}`;
        msg.isStreaming = false;
      });
      return;
    }

    const result = event.result;
    if (!result) return;

    const chunk: ChunkPayload = result.chunk;
    if (!chunk) return;

    // Track multi-turn state (task and statusUpdate both carry contextId/taskId)
    const ctx =
      chunk.task?.contextId ??
      chunk.statusUpdate?.status_update?.contextId ??
      chunk.statusUpdate?.statusUpdate?.contextId;
    const tid =
      chunk.task?.id ??
      chunk.statusUpdate?.taskId ??
      chunk.statusUpdate?.status_update?.taskId ??
      chunk.statusUpdate?.statusUpdate?.taskId;
    if (ctx) {
      _contextId.value = ctx;
      selectedHistoryContextId.value = ctx;
    }
    if (tid) _taskId.value = tid;
    if (ctx || tid) {
      ensureConversationHistoryStream();
    }

    // Track whether this chunk implies new provenance/planning material.
    let sawProvenanceMutation = false;

    // Shape: toolStreamChunk chunks split into two kinds.
    // - Phase (status): toolStreamChunk true, no tool payload — statusUpdate.status_update.status.message only (e.g. "Calling model: unknown (PhaseName)", "Invoking tool: X"). One block per "segment" (new segment after each message).
    // - Tool: toolStreamChunk true, has tool payload — toolName and/or events and/or completion. One block per tool invocation (new block when previous for same tool is DONE).
    // Backend may send tool data at chunk top level (chunk.toolName/events/completion) or inside chunk.task.
    const toolChunk = result.toolStreamChunk
      ? (chunk as ChunkPayload & {
          toolName?: string;
          events?: ToolEvent[];
          completion?: ToolCompletion;
          task?: { toolName?: string; events?: ToolEvent[]; completion?: ToolCompletion };
        })
      : null;
    const toolName = toolChunk?.toolName ?? toolChunk?.task?.toolName;
    const toolEvents =
      toolChunk?.events ??
      toolChunk?.task?.events ??
      (toolChunk &&
      typeof (toolChunk as { chunk?: unknown }).chunk === "object" &&
      (toolChunk as { chunk?: { events?: ToolEvent[] } }).chunk &&
      "events" in (toolChunk as { chunk: object }).chunk
        ? (toolChunk as { chunk: { events?: ToolEvent[] } }).chunk.events
        : undefined) ??
      [];
    const toolCompletion =
      toolChunk?.completion ??
      toolChunk?.task?.completion ??
      (toolChunk &&
      typeof (toolChunk as { chunk?: unknown }).chunk === "object" &&
      (toolChunk as { chunk?: object }).chunk &&
      "completion" in (toolChunk as { chunk: object }).chunk
        ? (toolChunk as { chunk: { completion?: ToolCompletion } }).chunk.completion
        : undefined);
    if (toolChunk && (!!toolName || toolEvents.length > 0 || !!toolCompletion)) {
      const baseName = toolName ?? "tool";
      const events = withSessionStepDetailEvents(toolEvents);
      const completion = toolCompletion;
      const appendMode = detectToolAppendMode(events, completion);

      updateMessage(messages, agentMsgId, (msg) => {
        ensureContentBlocks(msg);
        const block = getOrCreateToolBlockForAppend(msg, baseName, appendMode);
        if (events.length) pushToolEventsDeduped(block, events);
        if (completion) block.completion = completion;
        block.status = deriveToolStatus(block);
      });
      if (completion === "DONE" && baseName) {
        markWorkflowNodeCompleted(baseName);
        scheduleTraceRefreshBump();
      }
      sawProvenanceMutation = true;
    } else if (result.toolStreamChunk && toolChunk) {
      // Relay may still mark toolStreamChunk while attaching real assistant prose on chunk.message.
      // Without this branch, that prose was dropped because phase chunks never reached the handler below.
      let appliedAssistantFromWire = false;
      updateMessage(messages, agentMsgId, (msg) => {
        if (chunk.message && tryApplyStructuredMessage(msg, chunk.message)) {
          appliedAssistantFromWire = true;
          return;
        }
        if (
          chunk.task?.status?.message &&
          tryApplyStructuredMessage(msg, chunk.task.status.message)
        ) {
          appliedAssistantFromWire = true;
        }
      });
      if (!appliedAssistantFromWire) {
        const direct =
          extractText(chunk.message) ?? extractText(chunk.task?.status?.message);
        if (direct?.trim()) {
          const dtrim = direct.trim();
          if (
            !shouldSuppressAgentTranscriptText(dtrim) &&
            !parseExecutionErrorText(dtrim)
          ) {
            updateMessage(messages, agentMsgId, (msg) => {
              if (msg.contentBlocks) {
                pushTextBlock(msg, direct);
              } else {
                msg.text = msg.text ? `${msg.text}\n\n${direct}` : direct;
              }
            });
            sawProvenanceMutation = true;
          }
        }
      }
      // Phase chunk: statusUpdate drives WorkflowProgress only (not duplicate tool chatter).
      const statusText =
        extractTextFromStatusUpdate(chunk) ??
        extractText(chunk.statusUpdate?.status?.message) ??
        extractText(
          (chunk.statusUpdate?.statusUpdate ?? chunk.statusUpdate?.status_update)?.message,
        );
      const trimmed = statusText?.trim();
      // Skip "Invoking tool: X" in phase text — tool cards already render invocation details.
      if (trimmed && !trimmed.match(/^Invoking tool: /)) {
        updateWorkflowPhase(trimmed);
        sawProvenanceMutation = true;
      }
    } else {
      // Structured multi-part assistant messages (text + media_type/data + metadata.citations)
      let appliedStructured = false;
      updateMessage(messages, agentMsgId, (msg) => {
        if (chunk.message && tryApplyStructuredMessage(msg, chunk.message)) {
          appliedStructured = true;
          return;
        }
        if (
          chunk.task?.status?.message &&
          tryApplyStructuredMessage(msg, chunk.task.status.message)
        ) {
          appliedStructured = true;
        }
      });

      if (!appliedStructured) {
        const text =
          extractText(chunk.message) ??
          extractText(chunk.task?.status?.message) ??
          extractText(chunk.statusUpdate?.status?.message) ??
          extractTextFromStatusUpdate(chunk);

        if (text) {
          const trimmed = text.trim();
          if (trimmed && shouldSuppressAgentTranscriptText(trimmed)) {
            updateWorkflowPhase(trimmed);
            sawProvenanceMutation = true;
          } else if (trimmed && parseExecutionErrorText(trimmed)) {
            updateMessage(messages, agentMsgId, (msg) => {
              appendExecutionErrorCard(msg, text);
            });
            sawProvenanceMutation = true;
          } else {
            updateMessage(messages, agentMsgId, (msg) => {
              if (msg.contentBlocks) {
                pushTextBlock(msg, text);
              } else {
                msg.text = msg.text ? `${msg.text}\n\n${text}` : text;
              }
            });
            sawProvenanceMutation = true;
          }
        }
      }
    }

    // Check terminal state (state can be in task.status or nested statusUpdate.status_update.status)
    const state = getStateFromChunk(chunk);

    if (state && state !== lastSeenTaskState) {
      lastSeenTaskState = state;
      scheduleTraceRefreshBump();
      sawProvenanceMutation = true;
    }

    // Record state transitions for the task timeline
    if (state) {
      updateMessage(messages, agentMsgId, (msg) => {
        if (!msg.stateTransitions) msg.stateTransitions = [];
        // Avoid duplicate consecutive states
        const last = msg.stateTransitions[msg.stateTransitions.length - 1];
        if (!last || last.state !== state) {
          msg.stateTransitions.push({ state, timestamp: new Date() });
        }
      });
    }

    if (result.final) {
      scheduleTraceRefreshBump();
      sawProvenanceMutation = true;
    }

    // Keep planning/provenance panes live while tool chunks are flowing, not only on restore.
    if (_contextId.value && (sawProvenanceMutation || result.toolStreamChunk)) {
      scheduleTraceRefreshBump();
    }

    if (
      state === "TASK_STATE_COMPLETED" ||
      state === "TASK_STATE_FAILED" ||
      state === "TASK_STATE_CANCELED" ||
      result.final
    ) {
      updateMessage(messages, agentMsgId, (msg) => {
        msg.isStreaming = false;
      });
      workflowProgress.value = { phase: "idle", nodes: [], completedNodes: [] };
    }
    // Input required: stream suspended (no final); agent waiting for user reply.
    if (state === "TASK_STATE_INPUT_REQUIRED") {
      updateMessage(messages, agentMsgId, (msg) => {
        msg.isStreaming = false;
        msg.awaitingInput = true;
        const prompt =
          extractTextFromStatusUpdate(chunk) ??
          extractText(chunk.statusUpdate?.status?.message) ??
          extractText(chunk.task?.status?.message);
        const trimmed = prompt?.trim() ?? "";
        const synth = isSyntheticInputRequiredPrompt(trimmed);
        msg.inputRequiredPrompt = synth ? undefined : trimmed;
        // Real awaitInput(prompt) becomes placeholder + may show in-bubble; wire shim copy is not transcript.
        if (!synth && trimmed.length > 0 && !msg.text?.trim()) {
          if (msg.contentBlocks?.length) {
            ensureContentBlocks(msg);
            pushTextBlock(msg, trimmed);
          } else {
            msg.text = trimmed;
          }
        }
      });
    }
  }

  function extractTextFromStatusUpdate(chunk: ChunkPayload): string | undefined {
    const su = chunk.statusUpdate;
    if (!su) return undefined;
    // Flat shape from runtime shim (emitInputRequired): statusUpdate.status.message only.
    const flat = extractText(su.status?.message);
    if (flat?.trim()) return flat;
    const inner = su.statusUpdate ?? su.status_update;
    // Relay: nested status_update.status.message or inner.message
    const nested = inner as { message?: A2aMessage; status?: { message?: A2aMessage } } | undefined;
    return extractText(nested?.status?.message) ?? extractText(nested?.message);
  }

  /** State can be in task.status or statusUpdate.status or nested statusUpdate.status_update.status */
  function getStateFromChunk(chunk: ChunkPayload): string | undefined {
    const t = chunk.task?.status?.state;
    if (t) return t;
    const su = chunk.statusUpdate;
    const flat = su?.status?.state;
    if (flat) return flat;
    const inner = su?.statusUpdate ?? su?.status_update;
    return (inner as { status?: { state?: string } } | undefined)?.status?.state;
  }

  function extractText(
    message: { parts?: { text?: string }[] } | undefined | null,
  ): string | undefined {
    const parts = message?.parts;
    if (!parts?.length) return undefined;
    const texts: string[] = [];
    for (const p of parts) {
      if (typeof p?.text === "string" && p.text.length > 0) {
        texts.push(p.text);
      }
    }
    if (texts.length === 0) return undefined;
    return texts.join("\n\n");
  }

  async function fetchProvenanceDiagram(): Promise<void> {
    if (!_contextId.value) return;
    const seq = ++diagramFetchSeq;
    try {
      // Canonical API route is /contexts/{context_id}/mermaid.
      // Keep a legacy fallback while old backends/links still exist.
      let res = await fetch(`/contexts/${_contextId.value}/mermaid`);
      if (!res.ok && res.status === 404) {
        res = await fetch(`/mermaid/context/${_contextId.value}`);
      }
      if (res.ok) {
        const text = await res.text();
        if (seq !== diagramFetchSeq) return;
        provenanceDiagram.value = text;
      }
    } catch {
      // provenance endpoint not available; leave existing diagram
    }
  }

  async function fetchContextMetrics(): Promise<void> {
    if (!_contextId.value) return;
    try {
      const res = await fetch(`/contexts/${_contextId.value}/metrics`);
      if (res.ok) {
        contextMetrics.value = await res.json();
      }
    } catch {
      // metrics endpoint not available; leave existing data
    }
  }

  async function hydrateMessagesFromConversationHistory(
    contextId: string = _contextId.value ?? "",
    options: { allowRetry?: boolean; quiet?: boolean } = {},
  ): Promise<void> {
    if (!contextId) {
      historyHydrateState.value = "idle";
      return;
    }
    if (!options.quiet) {
      historyHydrateState.value = "loading";
    }
    try {
      const params = new URLSearchParams();
      params.set("limit", "500");
      const response = await fetch(
        `/contexts/${contextId}/conversation-history?${params.toString()}`,
      );
      if (!response.ok) {
        if (!options.quiet) {
          historyHydrateState.value = "error";
        }
        return;
      }

      const page = (await response.json()) as ConversationHistoryPage;
      if (!Array.isArray(page.items)) {
        if (!options.quiet) {
          historyHydrateState.value = "error";
        }
        return;
      }
      _taskId.value = page.taskId ?? null;
      if (liveAgentBubbleBlocksHistoryReplace(messages.value)) {
        if (!options.quiet) {
          historyHydrateState.value = "skipped";
        }
        if (options.allowRetry !== false && pendingHydrateRetryTimer === null) {
          pendingHydrateRetryTimer = setTimeout(() => {
            pendingHydrateRetryTimer = null;
            void hydrateMessagesFromConversationHistory(contextId, { allowRetry: false });
          }, 400);
        }
        return;
      }
      if (provenanceSnapshotLagsLiveChat(messages.value, page)) {
        if (!options.quiet) {
          historyHydrateState.value = "skipped";
        }
        if (options.allowRetry !== false && pendingHydrateRetryTimer === null) {
          pendingHydrateRetryTimer = setTimeout(() => {
            pendingHydrateRetryTimer = null;
            void hydrateMessagesFromConversationHistory(contextId, { allowRetry: false });
          }, 400);
        }
        return;
      }
      applyConversationHistoryPage(messages, page);
      replaceLlmPromptStateFromPage(page);
      _historyVersion = page.version;
      selectedHistoryContextId.value = page.contextId;
      historyHydrateState.value = "ready";
    } catch {
      if (!options.quiet) {
        historyHydrateState.value = "error";
      }
    }
  }

  async function loadConversationHistoryContext(
    contextId: string,
  ): Promise<void> {
    if (pendingHydrateRetryTimer !== null) {
      clearTimeout(pendingHydrateRetryTimer);
      pendingHydrateRetryTimer = null;
    }
    closeConversationHistoryStream();
    _historyVersion = "";
    llmPromptOperations.value = [];
    promptContextBytesSessionCurrent.value = null;
    _contextId.value = contextId;
    _taskId.value = null;
    workflowProgress.value = { phase: "idle", nodes: [], completedNodes: [] };
    lastSeenTaskState = undefined;
    isLoading.value = false;
    selectedHistoryContextId.value = contextId;
    messages.value = [];
    historyHydrateState.value = "loading";
    await Promise.all([
      hydrateMessagesFromConversationHistory(contextId),
      fetchProvenanceDiagram(),
      fetchContextMetrics(),
    ]);
    ensureConversationHistoryStream();
  }

  const awaitingInput = computed(() => {
    const last = [...messages.value].reverse().find((m) => m.role === "agent");
    return last?.awaitingInput ?? false;
  });
  const inputRequiredPrompt = computed(() => {
    const last = [...messages.value].reverse().find((m) => m.role === "agent");
    return last?.inputRequiredPrompt ?? "";
  });

  function cancelStream(): void {
    if (pendingHydrateRetryTimer !== null) {
      clearTimeout(pendingHydrateRetryTimer);
      pendingHydrateRetryTimer = null;
    }
    if (_abortController) {
      _abortController.abort();
      _abortController = null;
    }
    closeConversationHistoryStream();
    isLoading.value = false;
    const streamingMsg = messages.value.find((m) => m.isStreaming);
    if (streamingMsg) {
      streamingMsg.isStreaming = false;
      if (streamingMsg.contentBlocks?.length) {
        const lastText = [...streamingMsg.contentBlocks]
          .reverse()
          .find((b) => b.type === "text") as { type: "text"; text: string } | undefined;
        if (lastText) {
          lastText.text = `${lastText.text ?? ""} _(cancelled)_`;
        } else {
          streamingMsg.contentBlocks.push({ type: "text", text: "_(cancelled)_" });
        }
      } else {
        streamingMsg.text = `${streamingMsg.text ?? ""} _(cancelled)_`;
      }
    }
  }

  return {
    agents,
    selectedAgent,
    messages,
    isLoading,
    provenanceDiagram,
    traceRefreshGeneration,
    contextMetrics,
    llmPromptOperations,
    promptContextBytesSessionCurrent,
    conversationHistoryOptions,
    selectedHistoryContextId,
    historyLoading,
    historyHydrateState,
    workflowProgress,
    contextId: computed(() => _contextId.value),
    taskId: computed(() => _taskId.value),
    awaitingInput,
    inputRequiredPrompt,
    fetchAgents,
    selectAgent,
    fetchConversationHistoryOptions,
    loadConversationHistoryContext,
    sendMessage,
    cancelStream,
  };
}

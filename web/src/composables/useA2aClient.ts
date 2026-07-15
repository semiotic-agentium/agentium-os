// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import { ref, computed, type Ref } from "vue";

/** Per-request page size for GET/stream conversation-history (must match server default chunking). */
const CONVERSATION_HISTORY_PAGE_SIZE = 50;
import { applyConversationHistoryIngress } from "../chat/conversationHistorySync";
import { normalizeEpochMs, normalizePreview } from "../chat/chatTime";
import {
  ensureContentBlocks,
  partsToContentBlocks,
  pushStructuredBlocks,
  pushTextBlock,
} from "../chat/chatMessageBlocks";
import { appendExecutionErrorCard, parseExecutionErrorText } from "../chat/executionErrorCard";
import { readA2aSseJsonRpcStream } from "../chat/a2aSse";
import { collectChunkAssistantPlainText, extractWireMessageText } from "../chat/a2aStreamAssistantText";
import { digestA2aProcessEvent } from "../chat/a2aStreamChunkDigest";
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
import {
  extractMessageMetadata,
  isGateAuthorizationMetadata,
  isGateAuthorizationPrompt,
  parseGateAuthorizationPrompt,
  type GateAuthorizationSummary,
} from "../chat/gateAuthorizationUi";
import {
  fetchContextMermaidDiagram,
  scheduleContextMermaidDiagram,
} from "../utils/mermaidDiagram";
import {
  bumpTraceRefreshOnHistoryIngress,
  useTraceRefreshGeneration,
} from "./useTraceRefreshGeneration";
import { shouldSuppressAgentTranscriptText } from "../chat/workflowUiFilters";
import type {
  AgentDiscoveryEntry,
  A2aMessage,
  ChatMessage,
  ConversationHistoryItem,
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
  if (idx === -1) {
    if (typeof window !== "undefined" && window.location.hostname === "localhost") {
      console.warn(
        "[transcript] updateMessage skipped: unknown message id (orphan streaming row?)",
        id,
      );
    }
    return;
  }
  updater(messages.value[idx]!);
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
  const clientId = nextId("client");
  const agents: Ref<AgentDiscoveryEntry[]> = ref([]);
  const selectedAgent: Ref<AgentDiscoveryEntry | null> = ref(null);
  const messages: Ref<ChatMessage[]> = ref([]);
  const isLoading = ref(false);

  // Multi-turn conversation state
  const _contextId = ref<string | undefined>();
  const _taskId = ref<string | null>(null);

  // Context metrics (fetched after each response)
  const contextMetrics = ref<ContextMetricsResponse | null>(null);
  /** From conversation-history SSE/HTTP: temporal tail + per-LLM prompt telemetry */
  const llmPromptOperations = ref<LlmPromptOperation[]>([]);
  const promptMessageCharsSessionCurrent = ref<number | null>(null);
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

  // Provenance diagram source (raw mermaid text fetched after each response)
  const provenanceDiagram = ref<string>("");
  /** Monotonic id so slower diagram HTTP responses cannot overwrite newer ones */
  let diagramFetchSeq = 0;

  const traceRefresh = useTraceRefreshGeneration({
    when: () => !!_contextId.value,
    onBump: () => {
      void fetchProvenanceDiagram();
    },
  });

  function bumpTraceRefresh(force = false): void {
    traceRefresh.bumpTraceRefresh(force);
  }

  function replaceLlmPromptStateFromPage(page: ConversationHistoryPage): void {
    llmPromptOperations.value = [...(page.llmPromptOperations ?? [])].sort(sortLlmPromptOperations);
    promptMessageCharsSessionCurrent.value = page.promptMessageCharsSessionCurrent ?? null;
  }

  function extendLlmPromptStateFromPage(page: ConversationHistoryPage): void {
    llmPromptOperations.value = mergeLlmPromptOperations(llmPromptOperations.value, page.llmPromptOperations);
    if (page.promptMessageCharsSessionCurrent != null) {
      promptMessageCharsSessionCurrent.value = page.promptMessageCharsSessionCurrent;
    }
  }

  const sseConversationHistoryIngressDeps = {
    messages,
    getHistoryVersion: traceRefresh.getHistoryVersion,
    setHistoryVersion: traceRefresh.setHistoryVersion,
    setHydrateState: (s: HistoryHydrateState) => {
      historyHydrateState.value = s;
    },
    setSelectedContextId: (id: string) => {
      selectedHistoryContextId.value = id;
    },
    setTaskId: (id: string | null) => {
      _taskId.value = id;
    },
    replaceLlmFromPage: replaceLlmPromptStateFromPage,
    extendLlmFromPage: extendLlmPromptStateFromPage,
    deferFullSnapshotWhileA2aInFlight: () => isLoading.value,
  };

  let lastSeenTaskState: string | undefined;

  /** One-shot retry when hydration is skipped because provenance lags behind the live stream. */
  let pendingHydrateRetryTimer: ReturnType<typeof setTimeout> | null = null;

  const traceTranscript = (...args: unknown[]) => {
    if (typeof window !== "undefined" && window.location.hostname === "localhost") {
      console.debug(
        "[transcript]",
        ...args.map((arg) =>
          typeof arg === "string"
            ? `[${clientId}] ${arg}`
            : JSON.stringify({
                clientId,
                payload: arg,
              }),
        ),
      );
    }
  };

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
    params.set("limit", String(CONVERSATION_HISTORY_PAGE_SIZE));
    const encodedContextId = encodeURIComponent(_contextId.value);
    const url = `/contexts/${encodedContextId}/conversation-history/stream?${params.toString()}`;
    const stream = new EventSource(url);

    stream.addEventListener("snapshot", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        const effect = applyConversationHistoryIngress(sseConversationHistoryIngressDeps, {
          kind: "full",
          mode: "evented",
          page,
        });
        bumpTraceRefreshOnHistoryIngress(traceRefresh, effect);
      } catch {
        // Ignore malformed stream event payloads.
      }
    });
    stream.addEventListener("delta", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        const effect = applyConversationHistoryIngress(sseConversationHistoryIngressDeps, {
          kind: "delta",
          mode: "evented",
          page,
        });
        bumpTraceRefreshOnHistoryIngress(traceRefresh, effect);
      } catch {
        // Ignore malformed stream event payloads.
      }
    });
    stream.addEventListener("done", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        traceRefresh.setHistoryVersion(page.version);
      } catch {
        // Ignore malformed stream event payloads.
      } finally {
        bumpTraceRefresh();
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
      params.set("chatOnly", "true");
      params.set("agentPackage", selectedAgent.value.agent_package);
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
    traceTranscript("selectAgent.clear", {
      agent: `${agent.agent_package}/${agent.agent_instance_id}`,
      priorMessages: messages.value.length,
      priorContextId: _contextId.value ?? null,
    });
    selectedAgent.value = agent;
    historyHydrateState.value = "idle";
    messages.value = [];
    _contextId.value = undefined;
    _taskId.value = null;
    provenanceDiagram.value = "";
    contextMetrics.value = null;
    workflowProgress.value = { phase: "idle", nodes: [], completedNodes: [] };
    lastSeenTaskState = undefined;
    closeConversationHistoryStream();
    traceRefresh.resetHistoryVersion();
    llmPromptOperations.value = [];
    promptMessageCharsSessionCurrent.value = null;
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

    const lastAgent = [...messages.value].reverse().find((m) => m.role === "agent");
    // Clear input-required UI on the last agent bubble once the user sends a resume reply.
    if (lastAgent?.awaitingInput) {
      lastAgent.awaitingInput = false;
      lastAgent.inputRequiredPrompt = undefined;
      lastAgent.gateAuthorization = false;
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

    // Open provenance SSE only after the streaming row exists; otherwise the initial `snapshot`
    // can full-replace [user] and orphan `agentMsgId` before POST /a2a chunks arrive.
    ensureConversationHistoryStream();

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
      if (!response.body) {
        throw new Error(
          `POST /a2a returned no response body; expected streaming SSE (content-type: ${ct || "missing"})`,
        );
      }
      if (!ct.toLowerCase().includes("text/event-stream")) {
        console.warn(`POST /a2a unexpected Content-Type: ${ct || "(missing)"}; streaming parse anyway`);
      }
      const eventCount = await readA2aSseJsonRpcStream(response.body, (event) => {
        processEvent(event, agentMsgId);
      });
      if (eventCount === 0) {
        throw new Error(
          `No JSON-RPC events parsed from /a2a body (content-type: ${ct || "missing"})`,
        );
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
        fetchProvenanceDiagram({ force: true }),
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

    // Tool stream payloads:
    // - Relay/async path sets `result.toolStreamChunk` (__toolStreamChunk → toolStreamChunk).
    // - JS `__chat_yield` chunks often carry `task.toolName` / `task.events` without that flag.
    // We must handle both; otherwise tool/delegation turns show nothing until the terminal chunk.
    //
    // Phase-only relay chunks: toolStreamChunk true, no toolName/events/completion — workflow banner only.
    type ToolishChunk = ChunkPayload & {
      toolName?: string;
      events?: ToolEvent[];
      completion?: ToolCompletion;
      task?: { toolName?: string; events?: ToolEvent[]; completion?: ToolCompletion };
      chunk?: { events?: ToolEvent[]; completion?: ToolCompletion };
    };
    const ext = chunk as ToolishChunk;
    const nestedChunk = ext.chunk;
    const toolName = ext.toolName ?? ext.task?.toolName;
    const toolEvents =
      ext.events ??
      ext.task?.events ??
      (typeof nestedChunk === "object" &&
      nestedChunk !== null &&
      "events" in nestedChunk
        ? nestedChunk.events
        : undefined) ??
      [];
    const toolCompletion =
      ext.completion ??
      ext.task?.completion ??
      (typeof nestedChunk === "object" &&
      nestedChunk !== null &&
      "completion" in nestedChunk
        ? nestedChunk.completion
        : undefined);

    const hasToolStreamPayload =
      !!toolName || toolEvents.length > 0 || !!toolCompletion;

    if (hasToolStreamPayload) {
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
        bumpTraceRefresh();
      }
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
        const direct = collectChunkAssistantPlainText(chunk);
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
          }
        }
      }
      sawProvenanceMutation = true;
    } else if (result.toolStreamChunk) {
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
        const direct = collectChunkAssistantPlainText(chunk);
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
          collectChunkAssistantPlainText(chunk) ??
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
      bumpTraceRefresh();
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
      bumpTraceRefresh();
      sawProvenanceMutation = true;
    }

    // Keep planning/provenance panes live while tool chunks are flowing, not only on restore.
    if (_contextId.value && (sawProvenanceMutation || result.toolStreamChunk)) {
      bumpTraceRefresh();
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
        const statusMessage =
          chunk.statusUpdate?.status?.message ?? chunk.task?.status?.message;
        const meta = extractMessageMetadata(statusMessage);
        const prompt =
          collectChunkAssistantPlainText(chunk) ??
          extractTextFromStatusUpdate(chunk) ??
          extractText(chunk.statusUpdate?.status?.message) ??
          extractText(chunk.task?.status?.message);
        const trimmed = prompt?.trim() ?? "";
        msg.gateAuthorization =
          isGateAuthorizationMetadata(meta) || isGateAuthorizationPrompt(trimmed);
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

    if (typeof window !== "undefined" && window.location.hostname === "localhost") {
      traceTranscript("processEvent.digest", digestA2aProcessEvent(chunk, result));
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
    return extractWireMessageText(message as A2aMessage | undefined);
  }

  async function fetchProvenanceDiagram(options?: { force?: boolean }): Promise<void> {
    if (!_contextId.value) return;
    const seq = ++diagramFetchSeq;
    const text = options?.force
      ? await fetchContextMermaidDiagram(_contextId.value, { force: true })
      : await scheduleContextMermaidDiagram(_contextId.value);
    if (seq !== diagramFetchSeq) return;
    provenanceDiagram.value = text;
  }

  async function fetchContextMetrics(): Promise<void> {
    if (!_contextId.value) return;
    try {
      const res = await fetch(`/contexts/${encodeURIComponent(_contextId.value)}/metrics`);
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
      const PAGE_LIMIT = CONVERSATION_HISTORY_PAGE_SIZE;
      const allItems: ConversationHistoryItem[] = [];
      const mergedPromptOps: NonNullable<
        ConversationHistoryPage["llmPromptOperations"]
      > = [];
      let cursor: string | undefined;
      let lastPage: ConversationHistoryPage | null = null;
      let maxEventOrder = 0;
      let fetchCount = 0;

      for (;;) {
        const params = new URLSearchParams();
        params.set("limit", String(PAGE_LIMIT));
        if (cursor) params.set("cursor", cursor);
        const encodedContextId = encodeURIComponent(contextId);
        const response = await fetch(
          `/contexts/${encodedContextId}/conversation-history?${params.toString()}`,
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
        fetchCount += 1;
        allItems.push(...page.items);
        maxEventOrder = Math.max(maxEventOrder, page.maxEventOrder);
        if (page.llmPromptOperations?.length) {
          mergedPromptOps.push(...page.llmPromptOperations);
        }
        lastPage = page;
        if (!page.nextCursor) break;
        cursor = page.nextCursor;
      }

      if (!lastPage) {
        if (!options.quiet) {
          historyHydrateState.value = "error";
        }
        return;
      }

      const page: ConversationHistoryPage = {
        ...lastPage,
        items: allItems,
        maxEventOrder,
        llmPromptOperations:
          mergedPromptOps.length > 0 ? mergedPromptOps : lastPage.llmPromptOperations,
        nextCursor: null,
        version:
          fetchCount <= 1
            ? lastPage.version
            : `merged:${allItems.length}:${maxEventOrder}:${lastPage.version}`,
      };

      const hydrateIngressDeps = {
        messages,
        getHistoryVersion: traceRefresh.getHistoryVersion,
        setHistoryVersion: traceRefresh.setHistoryVersion,
        setHydrateState: (s: HistoryHydrateState) => {
          historyHydrateState.value = s;
        },
        setSelectedContextId: (id: string) => {
          selectedHistoryContextId.value = id;
        },
        setTaskId: (id: string | null) => {
          _taskId.value = id;
        },
        replaceLlmFromPage: replaceLlmPromptStateFromPage,
        extendLlmFromPage: extendLlmPromptStateFromPage,
        scheduleHydrateRetry:
          options.allowRetry !== false
            ? () => {
                if (pendingHydrateRetryTimer !== null) return;
                pendingHydrateRetryTimer = setTimeout(() => {
                  pendingHydrateRetryTimer = null;
                  void hydrateMessagesFromConversationHistory(contextId, { allowRetry: false });
                }, 400);
              }
            : undefined,
      };

      applyConversationHistoryIngress(hydrateIngressDeps, {
        kind: "full",
        mode: options.quiet ? "background" : "explicit_restore",
        page,
        allowRetry: options.allowRetry,
        respectDuplicateVersion: false,
        syncTaskIdFromPageBeforeDefer: true,
      });
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
    traceRefresh.resetHistoryVersion();
    llmPromptOperations.value = [];
    promptMessageCharsSessionCurrent.value = null;
    _contextId.value = contextId;
    _taskId.value = null;
    workflowProgress.value = { phase: "idle", nodes: [], completedNodes: [] };
    lastSeenTaskState = undefined;
    isLoading.value = false;
    selectedHistoryContextId.value = contextId;
    traceTranscript("loadContext.clear", {
      contextId,
      priorMessages: messages.value.length,
      selectedAgent: selectedAgent.value
        ? `${selectedAgent.value.agent_package}/${selectedAgent.value.agent_instance_id}`
        : null,
    });
    messages.value = [];
    historyHydrateState.value = "loading";
    await Promise.all([
      hydrateMessagesFromConversationHistory(contextId),
      fetchProvenanceDiagram({ force: true }),
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
  const gateAuthorizationPending = computed(() => {
    const last = [...messages.value].reverse().find((m) => m.role === "agent");
    return (last?.awaitingInput && last?.gateAuthorization) ?? false;
  });
  const gateAuthorizationSummary = computed((): GateAuthorizationSummary | null => {
    if (!gateAuthorizationPending.value) return null;
    const prompt = inputRequiredPrompt.value.trim();
    if (!prompt) return null;
    return parseGateAuthorizationPrompt(prompt);
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
    clientId,
    agents,
    selectedAgent,
    messages,
    isLoading,
    provenanceDiagram,
    traceRefreshGeneration: traceRefresh.traceRefreshGeneration,
    contextMetrics,
    llmPromptOperations,
    promptMessageCharsSessionCurrent,
    conversationHistoryOptions,
    selectedHistoryContextId,
    historyLoading,
    historyHydrateState,
    workflowProgress,
    contextId: computed(() => _contextId.value),
    taskId: computed(() => _taskId.value),
    awaitingInput,
    inputRequiredPrompt,
    gateAuthorizationPending,
    gateAuthorizationSummary,
    fetchAgents,
    selectAgent,
    fetchConversationHistoryOptions,
    loadConversationHistoryContext,
    sendMessage,
    cancelStream,
  };
}

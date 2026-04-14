import { ref, computed, type Ref } from "vue";
import type {
  AgentDiscoveryEntry,
  A2aMessage,
  ChatMessage,
  ContentBlock,
  ConversationHistoryItem,
  ConversationHistoryOption,
  ConversationHistoryPage,
  ContextMetricsResponse,
  DataContentBlock,
  JSONRPCResponse,
  ChunkPayload,
  Part,
  TextContentBlock,
  ToolNotificationBlock,
  ToolCompletion,
  ToolEvent,
  WorkflowProgressState,
} from "../types/a2a";

interface ProvenanceMessageRow {
  context_id?: string;
  task_id?: string | null;
  timestamp_ms?: number;
  message_text?: string;
  agent_package?: string;
  a2a_role?: string;
}

let counter = 0;
function nextId(prefix: string): string {
  return `${prefix}-${Date.now()}-${++counter}`;
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

/** Format status/phase text for display: surface phase or tool name, avoid "Calling model: unknown (X)". */
function formatStatusPhaseText(raw: string): string {
  const phaseMatch = raw.match(/Calling model: unknown \((.+)\)/);
  const toolMatch = raw.match(/Invoking tool: (.+)/);
  return phaseMatch ? phaseMatch[1]! : toolMatch ? `Tool: ${toolMatch[1]}` : raw;
}

function deriveToolStatus(block: ToolNotificationBlock): string {
  if (block.completion === "DONE") return "Done";
  if (block.completion === "INPUT_REQUIRED") return "Input required";
  if (block.completion === "INTERRUPTED") return "Interrupted";
  const last = block.events[block.events.length - 1];
  if (!last) return "Running";
  switch (last.kind) {
    case "assistant_thinking":
      return "Thinking…";
    case "assistant_tool_use":
      return "Using tool";
    case "assistant_text":
      return "Writing…";
    case "terminal_result":
      return "Complete";
    case "system_notice": {
      const phase = last.subtype ? formatStatusPhaseText(last.subtype) : "System";
      const model = typeof last.model === "string" ? last.model : undefined;
      return model ? `${phase} · ${model}` : phase;
    }
    default:
      return "Running";
  }
}

function ensureContentBlocks(msg: ChatMessage): void {
  if (msg.contentBlocks) return;
  msg.contentBlocks = [];
  if (msg.text) {
    msg.contentBlocks.push({ type: "text", text: msg.text });
  }
}

function findOrCreateToolBlock(msg: ChatMessage, toolName: string): ToolNotificationBlock {
  const blocks = msg.contentBlocks!;
  const existing = blocks.find(
    (b): b is ToolNotificationBlock => b.type === "tool" && b.toolName === toolName,
  );
  if (existing) return existing;
  const block: ToolNotificationBlock = {
    type: "tool",
    toolName,
    status: "Running",
    events: [],
  };
  blocks.push(block);
  return block;
}

/** Blocks for a given tool (baseName or "baseName 2", "baseName 3", …). */
function isToolBlockForBase(b: ContentBlock, baseName: string): b is ToolNotificationBlock {
  return (
    b.type === "tool" &&
    (b.toolName === baseName ||
      b.toolName === `${baseName} 2` ||
      b.toolName.startsWith(`${baseName} `))
  );
}

/** When the last block for this tool is already DONE, start a new block (new invocation); otherwise append. */
function getOrCreateToolBlockForAppend(
  msg: ChatMessage,
  baseName: string,
  _completion: ToolCompletion | undefined,
): ToolNotificationBlock {
  const blocks = msg.contentBlocks!;
  const toolBlocks = blocks.filter((b): b is ToolNotificationBlock =>
    isToolBlockForBase(b, baseName),
  );
  const lastTool = toolBlocks[toolBlocks.length - 1];
  const needNewBlock = lastTool?.completion === "DONE";
  if (needNewBlock) {
    const name = toolBlocks.length === 1 ? `${baseName} 2` : `${baseName} ${toolBlocks.length + 1}`;
    return findOrCreateToolBlock(msg, name);
  }
  return lastTool ?? findOrCreateToolBlock(msg, baseName);
}

/** Label for phase/status blocks (no tool payload: statusUpdate.message only). Distinct from tool blocks (toolName + events/completion). */
const PHASE_BLOCK_LABEL = "Status";

/** Phase blocks have toolName "Status" or "Status 2", "Status 3", …. */
function isPhaseBlock(b: ContentBlock): b is ToolNotificationBlock {
  return (
    b.type === "tool" &&
    (b.toolName === PHASE_BLOCK_LABEL || b.toolName.startsWith(`${PHASE_BLOCK_LABEL} `))
  );
}

/** When the last block is text, start a new phase block below it; otherwise append to the current one. */
function getOrCreatePhaseBlockForAppend(msg: ChatMessage): ToolNotificationBlock {
  const blocks = msg.contentBlocks!;
  const phaseBlocks = blocks.filter(isPhaseBlock);
  const lastBlock = blocks[blocks.length - 1];
  const needNewBlock = lastBlock?.type === "text";
  if (needNewBlock) {
    const name =
      phaseBlocks.length === 0
        ? PHASE_BLOCK_LABEL
        : `${PHASE_BLOCK_LABEL} ${phaseBlocks.length + 1}`;
    return findOrCreateToolBlock(msg, name);
  }
  const lastPhase = phaseBlocks[phaseBlocks.length - 1];
  return lastPhase ?? findOrCreateToolBlock(msg, PHASE_BLOCK_LABEL);
}

/** Push a new text block so each message/chunk is its own area (no concatenation). */
function pushTextBlock(msg: ChatMessage, text: string): void {
  const blocks = msg.contentBlocks!;
  blocks.push({ type: "text", text });
  syncMsgTextFromTextBlocks(msg);
}

/** Map A2A wire parts to UI blocks (prose + optional structured data parts). */
function partsToContentBlocks(parts: Part[]): Array<TextContentBlock | DataContentBlock> {
  const out: Array<TextContentBlock | DataContentBlock> = [];
  for (const p of parts) {
    const rawText = p.text;
    if (typeof rawText === "string" && rawText.trim() !== "") {
      out.push({ type: "text", text: rawText });
    }
    const mediaHint = p.media_type ?? p.mediaType;
    const hasData = p.data !== undefined;
    if (hasData || mediaHint) {
      out.push({
        type: "data",
        mediaType: mediaHint ?? "application/octet-stream",
        data: hasData ? p.data : null,
      });
    }
  }
  return out;
}

function syncMsgTextFromTextBlocks(msg: ChatMessage): void {
  const blocks = msg.contentBlocks ?? [];
  msg.text = blocks
    .filter((b): b is TextContentBlock => b.type === "text")
    .map((b) => b.text)
    .join("\n\n");
}

/** Append structured part blocks (text + data) in order; keeps msg.text as joined text blocks only. */
function pushStructuredBlocks(
  msg: ChatMessage,
  blocks: Array<TextContentBlock | DataContentBlock>,
): void {
  const arr = msg.contentBlocks!;
  for (const b of blocks) {
    arr.push(b);
  }
  syncMsgTextFromTextBlocks(msg);
}

function statusFromFsmPhase(fsmPhase: string): string {
  const phase = fsmPhase.toLowerCase();
  if (phase.includes("complete") || phase.includes("done") || phase.includes("finish")) return "Done";
  if (phase.includes("error") || phase.includes("fail") || phase.includes("abort")) return "Interrupted";
  return "Running";
}

function completionFromStatus(status: string): ToolCompletion | undefined {
  if (status === "Done") return "DONE";
  if (status === "Interrupted") return "INTERRUPTED";
  return undefined;
}

function stableJsonSignature(value: unknown): string {
  const visit = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(visit);
    if (v && typeof v === "object") {
      const obj = v as Record<string, unknown>;
      return Object.keys(obj)
        .sort()
        .reduce<Record<string, unknown>>((acc, key) => {
          acc[key] = visit(obj[key]);
          return acc;
        }, {});
    }
    return v;
  };
  return JSON.stringify(visit(value));
}

function applyConversationHistoryPage(messages: Ref<ChatMessage[]>, page: ConversationHistoryPage): void {
  const sorted = [...page.items].sort((a, b) => a.timestampMs - b.timestampMs);
  const rebuilt: ChatMessage[] = [];
  let turnOrdinal = 0;
  let activeAgentMsg: ChatMessage | null = null;
  // Per-turn dedupe: repeated send_done payload snapshots for same tool.
  let sendDonePayloadSignaturesByTool = new Map<string, Set<string>>();

  const ensureAgentMsg = (item: ConversationHistoryItem): ChatMessage => {
    if (activeAgentMsg) return activeAgentMsg;
    const msg: ChatMessage = {
      id: `prov-agent-${turnOrdinal}-${item.activityAnchor}`,
      role: "agent",
      text: "",
      timestamp: new Date(normalizeEpochMs(item.timestampMs)),
      contentBlocks: [],
    };
    rebuilt.push(msg);
    activeAgentMsg = msg;
    return msg;
  };

  for (const item of sorted) {
    const isUser = item.role.toLowerCase() === "user";
    const ts = new Date(normalizeEpochMs(item.timestampMs));
    const content = item.content;

    if (isUser) {
      turnOrdinal += 1;
      activeAgentMsg = null;
      sendDonePayloadSignaturesByTool = new Map<string, Set<string>>();
      const text = content.type === "message" ? content.text : "";
      rebuilt.push({
        id: `prov-user-${item.activityAnchor}`,
        role: "user",
        text,
        timestamp: ts,
      });
      continue;
    }

    if (
      content.type === "session_step" &&
      content.op.kind === "send_done" &&
      content.send_done_replay_payload !== undefined
    ) {
      const signature = stableJsonSignature(content.send_done_replay_payload);
      const toolName = content.tool_name;
      const seen = sendDonePayloadSignaturesByTool.get(toolName) ?? new Set<string>();
      if (seen.has(signature)) {
        // Collapse duplicate send_done steps with equivalent replay payload.
        continue;
      }
      seen.add(signature);
      sendDonePayloadSignaturesByTool.set(toolName, seen);
    }

    const msg = ensureAgentMsg(item);
    ensureContentBlocks(msg);

    switch (content.type) {
      case "message": {
        if (content.text) {
          pushTextBlock(msg, content.text);
        }
        if (Array.isArray(content.citations) && content.citations.length > 0) {
          const prev = Array.isArray(msg.metadata?.citations)
            ? (msg.metadata!.citations as unknown[]).filter((x): x is string => typeof x === "string")
            : [];
          const merged = [...new Set([...prev, ...content.citations])];
          msg.metadata = { ...(msg.metadata ?? {}), citations: merged };
        }
        break;
      }
      case "tool_call": {
        const status = statusFromFsmPhase(content.fsm_phase);
        const completion = completionFromStatus(status);
        const block = getOrCreateToolBlockForAppend(msg, content.tool_name, completion);
        block.events.push({
          kind: "assistant_tool_use",
          name: content.tool_name,
          input: content.args,
        });
        block.events.push({
          kind: "system_notice",
          subtype: `FSM phase: ${content.fsm_phase}`,
          text: `FSM phase: ${content.fsm_phase}`,
        });
        if (completion) block.completion = completion;
        block.status = deriveToolStatus(block);
        break;
      }
      case "tool_result": {
        const status = statusFromFsmPhase(content.fsm_phase);
        const completion = completionFromStatus(status) ?? "DONE";
        const block = getOrCreateToolBlockForAppend(msg, content.tool_name, completion);
        block.events.push({
          kind: "system_notice",
          subtype: `FSM phase: ${content.fsm_phase}`,
          text: `FSM phase: ${content.fsm_phase}`,
        });
        block.events.push({
          kind: "terminal_result",
          subtype: "success",
          result:
            typeof content.outcome === "string" ? content.outcome : JSON.stringify(content.outcome),
        });
        block.completion = completion;
        block.status = deriveToolStatus(block);
        break;
      }
      case "session_step": {
        const stepKind = content.op.kind;
        const done = stepKind === "send_done" || stepKind === "finish";
        const completion = done ? "DONE" : undefined;
        const block = getOrCreateToolBlockForAppend(msg, content.tool_name, completion);
        block.events.push({
          kind: "system_notice",
          subtype: `Session step: ${stepKind}`,
          text: `Session step: ${stepKind}`,
        });
        if (Array.isArray(content.read_replay_lines) && content.read_replay_lines.length > 0) {
          block.events.push({
            kind: "assistant_text",
            text: content.read_replay_lines.join("\n"),
          });
        }
        if (completion) block.completion = completion;
        block.status = deriveToolStatus(block);
        break;
      }
    }
  }

  messages.value = rebuilt;
}

function normalizePreview(text: string | undefined): string {
  if (!text) return "Untitled conversation";
  const singleLine = text.replace(/\s+/g, " ").trim();
  if (singleLine.length === 0) return "Untitled conversation";
  return singleLine.length > 80 ? `${singleLine.slice(0, 77)}...` : singleLine;
}

function normalizeEpochMs(raw: number | string | undefined): number {
  const numeric = typeof raw === "string" ? Number(raw) : raw;
  if (!Number.isFinite(numeric) || !numeric || numeric <= 0) return 0;
  // Some provenance reads surface nanoseconds; convert to milliseconds for UI dates.
  if (numeric > 10_000_000_000_000) return Math.floor(numeric / 1_000_000);
  return numeric;
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
  const conversationHistoryOptions = ref<ConversationHistoryOption[]>([]);
  const selectedHistoryContextId = ref<string | null>(null);
  const historyLoading = ref(false);

  // Workflow progress tracker (parsed from coordinator SSE progress messages)
  const workflowProgress = ref<WorkflowProgressState>({
    phase: "idle",
    nodes: [],
    completedNodes: [],
  });

  // Stream cancellation
  let _abortController: AbortController | null = null;
  let _streamReader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let _historyStream: EventSource | null = null;
  let _historyStreamKey = "";

  // Provenance diagram source (raw mermaid text fetched after each response)
  const provenanceDiagram = ref<string>("");
  /** Throttle diagram refetch during stream (ms); updated on each fetch */
  let lastDiagramFetchAt = 0;
  /** Provenance diagram refetch during SSE — keep low churn vs trace Mermaid cost */
  const diagramThrottleMs = 2000;
  /** Monotonic id so slower diagram HTTP responses cannot overwrite newer ones */
  let diagramFetchSeq = 0;

  /** Incremented (debounced) when SSE implies new provenance rows; ProvenancePane watches this */
  const traceRefreshGeneration = ref(0);
  let traceRefreshDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  /** Dedupe task state bumps within a single stream */
  let lastSeenTaskState: string | undefined;
  const TRACE_REFRESH_DEBOUNCE_MS = 300;

  function scheduleTraceRefreshBump(): void {
    if (!_contextId.value) return;
    if (traceRefreshDebounceTimer !== null) clearTimeout(traceRefreshDebounceTimer);
    traceRefreshDebounceTimer = setTimeout(() => {
      traceRefreshDebounceTimer = null;
      traceRefreshGeneration.value += 1;
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
        applyConversationHistoryPage(messages, page);
        selectedHistoryContextId.value = page.contextId;
      } catch {
        // Ignore malformed stream event payloads.
      }
    });
    stream.addEventListener("done", (ev) => {
      try {
        const page = JSON.parse((ev as MessageEvent<string>).data) as ConversationHistoryPage;
        applyConversationHistoryPage(messages, page);
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
    const res = await fetch("/agents");
    agents.value = await res.json();
    if (agents.value.length > 0 && !selectedAgent.value) {
      selectedAgent.value = agents.value[0] ?? null;
    }
    await fetchConversationHistoryOptions();
  }

  async function fetchConversationHistoryOptions(): Promise<void> {
    if (!selectedAgent.value) {
      conversationHistoryOptions.value = [];
      selectedHistoryContextId.value = null;
      return;
    }
    historyLoading.value = true;
    try {
      const response = await fetch(
        "/provenance/messages?pageSize=200&sortBy=timestamp_ms&sortDir=desc&outcome=both",
      );
      if (!response.ok) {
        conversationHistoryOptions.value = [];
        return;
      }
      const payload = (await response.json()) as { rows?: ProvenanceMessageRow[] };
      const rows = Array.isArray(payload.rows) ? payload.rows : [];
      const byContext = new Map<
        string,
        {
          contextId: string;
          taskId: string | null;
          latestTimestampMs: number;
          latestPreview: string;
          firstUserTimestampMs: number;
          firstUserMessage: string;
        }
      >();
      const selectedPackage = selectedAgent.value.agent_package;

      for (const row of rows) {
        if (row.agent_package && row.agent_package !== selectedPackage) continue;
        const contextId = row.context_id;
        if (!contextId) continue;
        const ts = normalizeEpochMs(row.timestamp_ms);
        const existing = byContext.get(contextId) ?? {
          contextId,
          taskId: null,
          latestTimestampMs: 0,
          latestPreview: "Untitled conversation",
          firstUserTimestampMs: Number.MAX_SAFE_INTEGER,
          firstUserMessage: "",
        };

        if (ts >= existing.latestTimestampMs) {
          existing.latestTimestampMs = ts;
          existing.latestPreview = normalizePreview(row.message_text);
        }

        const role = row.a2a_role?.toUpperCase() ?? "";
        if (
          role === "ROLE_USER" &&
          typeof row.message_text === "string" &&
          row.message_text.trim().length > 0 &&
          ts > 0 &&
          ts <= existing.firstUserTimestampMs
        ) {
          existing.firstUserTimestampMs = ts;
          existing.firstUserMessage = normalizePreview(row.message_text);
        }

        byContext.set(contextId, existing);
      }

      conversationHistoryOptions.value = [...byContext.values()]
        .map<ConversationHistoryOption>((ctx) => ({
          contextId: ctx.contextId,
          taskId: ctx.taskId,
          latestTimestampMs: ctx.latestTimestampMs,
          preview: ctx.firstUserMessage || ctx.latestPreview,
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

    // Clear input-required state on the last agent message when user replies (resume turn)
    const lastAgent = [...messages.value].reverse().find((m) => m.role === "agent");
    if (lastAgent?.awaitingInput) {
      lastAgent.awaitingInput = false;
      lastAgent.inputRequiredPrompt = undefined;
    }

    const agent = selectedAgent.value;
    const url = `/agents/${agent.agent_package}/${agent.agent_instance_id}/a2a/sse`;

    // Add user message
    messages.value.push({
      id: nextId("user-msg"),
      role: "user",
      text: text.trim(),
      timestamp: new Date(),
    });

    // Build JSON-RPC request
    const message: Record<string, unknown> = {
      messageId: nextId("ui-msg"),
      role: "user",
      parts: [{ text: text.trim() }],
    };
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

    const controller = new AbortController();
    _abortController = controller;

    try {
      const response = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
        signal: controller.signal,
      });

      if (!response.ok || !response.body) {
        throw new Error(`HTTP ${response.status}`);
      }

      await readSSEStream(response.body, agentMsgId);
      await Promise.all([
        hydrateMessagesFromConversationHistory(),
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
      _streamReader = null;
      isLoading.value = false;
    }
  }

  async function readSSEStream(
    body: ReadableStream<Uint8Array>,
    agentMsgId: string,
  ): Promise<void> {
    const reader = body.getReader();
    _streamReader = reader;
    const decoder = new TextDecoder();
    let buffer = "";
    /** Macrotask yield every N SSE data events so the stream loop is not one timer per line */
    let sseYieldCounter = 0;

    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";

      for (const line of lines) {
        if (line.startsWith("data:")) {
          const jsonStr = line.slice(5).trim();
          if (!jsonStr) continue;
          try {
            const event: JSONRPCResponse = JSON.parse(jsonStr);
            processEvent(event, agentMsgId);
            // Yield occasionally so the UI can paint; avoid setTimeout per line on chatty streams
            sseYieldCounter++;
            if ((sseYieldCounter & 3) === 0) {
              await new Promise((resolve) => setTimeout(resolve, 0));
            }
            // Refresh provenance diagram (throttled); do not await — keeps SSE ahead of HTTP latency
            if (_contextId.value && Date.now() - lastDiagramFetchAt >= diagramThrottleMs) {
              lastDiagramFetchAt = Date.now();
              void fetchProvenanceDiagram();
            }
          } catch {
            // skip malformed events
          }
        }
      }
    }

    // Mark streaming complete
    updateMessage(messages, agentMsgId, (msg) => {
      msg.isStreaming = false;
    });
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
      const events = toolEvents;
      const completion = toolCompletion;

      updateMessage(messages, agentMsgId, (msg) => {
        ensureContentBlocks(msg);
        const block = getOrCreateToolBlockForAppend(msg, baseName, completion);
        if (events.length) block.events.push(...events);
        if (completion) block.completion = completion;
        block.status = deriveToolStatus(block);
      });
      if (completion === "DONE" && baseName) {
        markWorkflowNodeCompleted(baseName);
        scheduleTraceRefreshBump();
      }
    } else if (result.toolStreamChunk && toolChunk) {
      // Phase chunk: toolStreamChunk true but no tool payload — statusUpdate.message only
      const statusText =
        extractTextFromStatusUpdate(chunk) ??
        extractText(chunk.statusUpdate?.status?.message) ??
        extractText(
          (chunk.statusUpdate?.statusUpdate ?? chunk.statusUpdate?.status_update)?.message,
        );
      const trimmed = statusText?.trim();
      // Skip "Invoking tool: X" in the phase block — the tool has its own block; keeps order correct (phase after tool)
      if (trimmed && !trimmed.match(/^Invoking tool: /)) {
        updateWorkflowPhase(trimmed);
        updateMessage(messages, agentMsgId, (msg) => {
          ensureContentBlocks(msg);
          const block = getOrCreatePhaseBlockForAppend(msg);
          block.events.push({ kind: "system_notice", subtype: trimmed, text: trimmed });
          block.status = deriveToolStatus(block);
        });
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
          updateMessage(messages, agentMsgId, (msg) => {
            if (msg.contentBlocks) {
              pushTextBlock(msg, text);
            } else {
              msg.text = msg.text ? `${msg.text}\n\n${text}` : text;
            }
          });
        }
      }
    }

    // Check terminal state (state can be in task.status or nested statusUpdate.status_update.status)
    const state = getStateFromChunk(chunk);

    if (state && state !== lastSeenTaskState) {
      lastSeenTaskState = state;
      scheduleTraceRefreshBump();
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
          extractTextFromStatusUpdate(chunk) ?? extractText(chunk.statusUpdate?.status?.message);
        if (prompt?.trim()) msg.inputRequiredPrompt = prompt.trim();
      });
    }
  }

  function extractTextFromStatusUpdate(chunk: ChunkPayload): string | undefined {
    const su = chunk.statusUpdate;
    const inner = su?.statusUpdate ?? su?.status_update;
    // Relay sends status_update.status.message; flat shape uses inner.message
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
    return message?.parts?.[0]?.text ?? undefined;
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
  ): Promise<void> {
    if (!contextId) return;
    try {
      const params = new URLSearchParams();
      params.set("limit", "500");
      const response = await fetch(
        `/contexts/${contextId}/conversation-history?${params.toString()}`,
      );
      if (!response.ok) return;

      const page = (await response.json()) as ConversationHistoryPage;
      if (!Array.isArray(page.items)) return;
      applyConversationHistoryPage(messages, page);
      selectedHistoryContextId.value = page.contextId;
    } catch {
      // conversation-history endpoint not available; keep stream-derived chat
    }
  }

  async function loadConversationHistoryContext(
    contextId: string,
  ): Promise<void> {
    closeConversationHistoryStream();
    _contextId.value = contextId;
    _taskId.value = null;
    workflowProgress.value = { phase: "idle", nodes: [], completedNodes: [] };
    lastSeenTaskState = undefined;
    isLoading.value = false;
    selectedHistoryContextId.value = contextId;
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
    if (_streamReader) {
      _streamReader.cancel().catch(() => {});
      _streamReader = null;
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
    conversationHistoryOptions,
    selectedHistoryContextId,
    historyLoading,
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

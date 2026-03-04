import { ref, computed, type Ref } from "vue";
import type {
  AgentDiscoveryEntry,
  ChatMessage,
  ContentBlock,
  ContextMetricsResponse,
  JSONRPCResponse,
  ChunkPayload,
  ToolCompletion,
  ToolEvent,
  ToolNotificationBlock,
} from "../types/a2a";

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
    (b.toolName === baseName || b.toolName === `${baseName} 2` || b.toolName.startsWith(`${baseName} `))
  );
}

/** When the last block for this tool is already DONE, start a new block (new invocation); otherwise append. */
function getOrCreateToolBlockForAppend(
  msg: ChatMessage,
  baseName: string,
  _completion: ToolCompletion | undefined,
): ToolNotificationBlock {
  const blocks = msg.contentBlocks!;
  const toolBlocks = blocks.filter((b): b is ToolNotificationBlock => isToolBlockForBase(b, baseName));
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
  return b.type === "tool" && (b.toolName === PHASE_BLOCK_LABEL || b.toolName.startsWith(`${PHASE_BLOCK_LABEL} `));
}

/** When the last block is text, start a new phase block below it; otherwise append to the current one. */
function getOrCreatePhaseBlockForAppend(msg: ChatMessage): ToolNotificationBlock {
  const blocks = msg.contentBlocks!;
  const phaseBlocks = blocks.filter(isPhaseBlock);
  const lastBlock = blocks[blocks.length - 1];
  const needNewBlock = lastBlock?.type === "text";
  if (needNewBlock) {
    const name = phaseBlocks.length === 0 ? PHASE_BLOCK_LABEL : `${PHASE_BLOCK_LABEL} ${phaseBlocks.length + 1}`;
    return findOrCreateToolBlock(msg, name);
  }
  const lastPhase = phaseBlocks[phaseBlocks.length - 1];
  return lastPhase ?? findOrCreateToolBlock(msg, PHASE_BLOCK_LABEL);
}

/** Push a new text block so each message/chunk is its own area (no concatenation). */
function pushTextBlock(msg: ChatMessage, text: string): void {
  const blocks = msg.contentBlocks!;
  blocks.push({ type: "text", text });
  // Keep msg.text in sync for backward compat / fallback
  msg.text = blocks
    .filter((b) => b.type === "text")
    .map((b) => (b as { text: string }).text)
    .join("\n\n");
}

export function useA2aClient() {
  const agents: Ref<AgentDiscoveryEntry[]> = ref([]);
  const selectedAgent: Ref<AgentDiscoveryEntry | null> = ref(null);
  const messages: Ref<ChatMessage[]> = ref([]);
  const isLoading = ref(false);

  // Multi-turn conversation state
  const _contextId = ref<string | undefined>();
  let taskId: string | undefined;

  // Context metrics (fetched after each response)
  const contextMetrics = ref<ContextMetricsResponse | null>(null);

  // Provenance diagram source (raw mermaid text fetched after each response)
  const provenanceDiagram = ref<string>("");
  /** Throttle diagram refetch during stream (ms); updated on each fetch */
  let lastDiagramFetchAt = 0;
  const diagramThrottleMs = 500;

  async function fetchAgents(): Promise<void> {
    const res = await fetch("/agents");
    agents.value = await res.json();
    if (agents.value.length > 0 && !selectedAgent.value) {
      selectedAgent.value = agents.value[0] ?? null;
    }
  }

  function selectAgent(agent: AgentDiscoveryEntry): void {
    selectedAgent.value = agent;
    messages.value = [];
    _contextId.value = undefined;
    taskId = undefined;
    provenanceDiagram.value = "";
    contextMetrics.value = null;
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
    if (taskId) message.taskId = taskId;

    const request = {
      jsonrpc: "2.0",
      id: nextId("corr"),
      method: "message.sendStream",
      params: { message },
    };

    isLoading.value = true;

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

    try {
      const response = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!response.ok || !response.body) {
        throw new Error(`HTTP ${response.status}`);
      }

      await readSSEStream(response.body, agentMsgId);
      await Promise.all([fetchProvenanceDiagram(), fetchContextMetrics()]);
    } catch (err) {
      updateMessage(messages, agentMsgId, (msg) => {
        msg.text = `Error: ${err}`;
        msg.isStreaming = false;
      });
    } finally {
      isLoading.value = false;
    }
  }

  async function readSSEStream(
    body: ReadableStream<Uint8Array>,
    agentMsgId: string,
  ): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";

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
            // Yield so the UI can paint incrementally as chunks arrive (not only when stream ends)
            await new Promise((resolve) => setTimeout(resolve, 0));
            // Refresh provenance diagram as tool/message updates arrive (throttled)
            if (_contextId.value && Date.now() - lastDiagramFetchAt >= diagramThrottleMs) {
              lastDiagramFetchAt = Date.now();
              await fetchProvenanceDiagram();
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
    const ctx = chunk.task?.contextId ?? chunk.statusUpdate?.status_update?.contextId ?? chunk.statusUpdate?.statusUpdate?.contextId;
    const tid = chunk.task?.id ?? chunk.statusUpdate?.taskId ?? chunk.statusUpdate?.status_update?.taskId ?? chunk.statusUpdate?.statusUpdate?.taskId;
    if (ctx) _contextId.value = ctx;
    if (tid) taskId = tid;

    // Shape: toolStreamChunk chunks split into two kinds.
    // - Phase (status): toolStreamChunk true, no tool payload — statusUpdate.status_update.status.message only (e.g. "Calling model: unknown (PhaseName)", "Invoking tool: X"). One block per "segment" (new segment after each message).
    // - Tool: toolStreamChunk true, has tool payload — toolName and/or events and/or completion. One block per tool invocation (new block when previous for same tool is DONE).
    // Backend may send tool data at chunk top level (chunk.toolName/events/completion) or inside chunk.task.
    const toolChunk = result.toolStreamChunk
      ? (chunk as ChunkPayload & { toolName?: string; events?: ToolEvent[]; completion?: ToolCompletion; task?: { toolName?: string; events?: ToolEvent[]; completion?: ToolCompletion } })
      : null;
    const toolName = toolChunk?.toolName ?? toolChunk?.task?.toolName;
    const toolEvents =
      toolChunk?.events ??
      toolChunk?.task?.events ??
      (toolChunk &&
      typeof (toolChunk as { chunk?: unknown }).chunk === "object" &&
      (toolChunk as { chunk?: { events?: ToolEvent[] } }).chunk &&
      "events" in (toolChunk as { chunk: object }).chunk
        ? ((toolChunk as { chunk: { events?: ToolEvent[] } }).chunk.events)
        : undefined) ??
      [];
    const toolCompletion =
      toolChunk?.completion ??
      toolChunk?.task?.completion ??
      (toolChunk &&
      typeof (toolChunk as { chunk?: unknown }).chunk === "object" &&
      (toolChunk as { chunk?: object }).chunk &&
      "completion" in (toolChunk as { chunk: object }).chunk
        ? ((toolChunk as { chunk: { completion?: ToolCompletion } }).chunk.completion)
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
    } else if (result.toolStreamChunk && toolChunk) {
      // Phase chunk: toolStreamChunk true but no tool payload — statusUpdate.message only
      const statusText =
        extractTextFromStatusUpdate(chunk) ??
        extractText(chunk.statusUpdate?.status?.message) ??
        extractText((chunk.statusUpdate?.statusUpdate ?? chunk.statusUpdate?.status_update)?.message);
      const trimmed = statusText?.trim();
      // Skip "Invoking tool: X" in the phase block — the tool has its own block; keeps order correct (phase after tool)
      if (trimmed && !trimmed.match(/^Invoking tool: /)) {
        updateMessage(messages, agentMsgId, (msg) => {
          ensureContentBlocks(msg);
          const block = getOrCreatePhaseBlockForAppend(msg);
          block.events.push({ kind: "system_notice", subtype: trimmed, text: trimmed });
          block.status = deriveToolStatus(block);
        });
      }
    } else {
      // Normal text from message / task / statusUpdate
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

    // Check terminal state (state can be in task.status or nested statusUpdate.status_update.status)
    const state = getStateFromChunk(chunk);
    if (
      state === "TASK_STATE_COMPLETED" ||
      state === "TASK_STATE_FAILED" ||
      state === "TASK_STATE_CANCELED" ||
      result.final
    ) {
      updateMessage(messages, agentMsgId, (msg) => {
        msg.isStreaming = false;
      });
    }
    // Input required: stream suspended (no final); agent waiting for user reply.
    if (state === "TASK_STATE_INPUT_REQUIRED") {
      updateMessage(messages, agentMsgId, (msg) => {
        msg.isStreaming = false;
        msg.awaitingInput = true;
        const prompt =
          extractTextFromStatusUpdate(chunk) ??
          extractText(chunk.statusUpdate?.status?.message);
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
    try {
      const res = await fetch(`/mermaid/context/${_contextId.value}`);
      if (res.ok) provenanceDiagram.value = await res.text();
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

  const awaitingInput = computed(() => {
    const last = [...messages.value].reverse().find((m) => m.role === "agent");
    return last?.awaitingInput ?? false;
  });
  const inputRequiredPrompt = computed(() => {
    const last = [...messages.value].reverse().find((m) => m.role === "agent");
    return last?.inputRequiredPrompt ?? "";
  });

  return {
    agents,
    selectedAgent,
    messages,
    isLoading,
    provenanceDiagram,
    contextMetrics,
    contextId: computed(() => _contextId.value),
    awaitingInput,
    inputRequiredPrompt,
    fetchAgents,
    selectAgent,
    sendMessage,
  };
}

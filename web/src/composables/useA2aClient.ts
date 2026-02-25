import { ref, type Ref } from "vue";
import type {
  AgentDiscoveryEntry,
  ChatMessage,
  JSONRPCResponse,
  ChunkPayload,
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

export function useA2aClient() {
  const agents: Ref<AgentDiscoveryEntry[]> = ref([]);
  const selectedAgent: Ref<AgentDiscoveryEntry | null> = ref(null);
  const messages: Ref<ChatMessage[]> = ref([]);
  const isLoading = ref(false);

  // Multi-turn conversation state
  let contextId: string | undefined;
  let taskId: string | undefined;

  // Provenance diagram source (raw mermaid text fetched after each response)
  const provenanceDiagram = ref<string>("");

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
    contextId = undefined;
    taskId = undefined;
    provenanceDiagram.value = "";
  }

  async function sendMessage(text: string): Promise<void> {
    if (!selectedAgent.value || !text.trim()) return;

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
    if (contextId) message.contextId = contextId;
    if (taskId) message.taskId = taskId;

    const request = {
      jsonrpc: "2.0",
      id: nextId("corr"),
      method: "message.sendStream",
      params: { message },
    };

    isLoading.value = true;

    // Placeholder for streaming agent response
    const agentMsgId = nextId("agent-msg");
    messages.value.push({
      id: agentMsgId,
      role: "agent",
      text: "",
      timestamp: new Date(),
      isStreaming: true,
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
      await fetchProvenanceDiagram();
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

    // Track multi-turn state
    if (chunk.task?.contextId) contextId = chunk.task.contextId;
    if (chunk.task?.id) taskId = chunk.task.id;

    // Extract agent text from whichever field carries it
    const text =
      extractText(chunk.message) ??
      extractText(chunk.task?.status?.message) ??
      extractText(chunk.statusUpdate?.status?.message);

    if (text) {
      updateMessage(messages, agentMsgId, (msg) => {
        if (!msg.text) {
          msg.text = text;
        }
      });
    }

    // Check terminal state
    const state =
      chunk.task?.status?.state ?? chunk.statusUpdate?.status?.state;
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
  }

  function extractText(
    message: { parts?: { text?: string }[] } | undefined | null,
  ): string | undefined {
    return message?.parts?.[0]?.text ?? undefined;
  }

  async function fetchProvenanceDiagram(): Promise<void> {
    if (!contextId) return;
    try {
      const res = await fetch(`/mermaid/context/${contextId}`);
      if (res.ok) provenanceDiagram.value = await res.text();
    } catch {
      // provenance endpoint not available; leave existing diagram
    }
  }

  return {
    agents,
    selectedAgent,
    messages,
    isLoading,
    provenanceDiagram,
    fetchAgents,
    selectAgent,
    sendMessage,
  };
}

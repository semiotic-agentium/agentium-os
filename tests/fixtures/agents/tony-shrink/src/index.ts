import type {
  A2aJsonRpcRequest,
  A2aStreamChunk,
  Message,
  SendMessageRequest,
} from "./a2a";

const conversationMemory: Record<string, string[]> = {};

function extractText(params: SendMessageRequest | Record<string, unknown> | null): string {
  if (!params || typeof params !== "object") return "unknown";
  const p = params as Record<string, unknown>;
  if (typeof p.text === "string") return p.text;
  const message = p.message as Message | undefined;
  if (message?.parts?.length && typeof message.parts[0]?.text === "string") {
    return message.parts[0].text;
  }
  return "unknown";
}

function newMessage(messageId: string, text: string): Message {
  return {
    messageId,
    role: "ROLE_AGENT",
    parts: [{ text }],
  };
}

function addMemory(contextId: string, text: string): void {
  if (!conversationMemory[contextId]) conversationMemory[contextId] = [];
  conversationMemory[contextId].push(text);
  if (conversationMemory[contextId].length > 6) conversationMemory[contextId].shift();
}

async function tony_memory(args: { limit?: number }): Promise<{ context_id: string; memory: string[] }> {
  const contextId = (globalThis as unknown as { __baml_context_id?: string }).__baml_context_id ?? "ctx-tony-001";
  const limit =
    typeof args?.limit === "number" && Number.isFinite(args.limit)
      ? Math.max(1, Math.min(20, Math.floor(args.limit)))
      : 6;
  const memory = conversationMemory[contextId] ?? [];
  return { context_id: contextId, memory: memory.slice(-limit) };
}

async function buildBamlResponse(text: string, contextId: string): Promise<string> {
  try {
    (globalThis as unknown as { __baml_context_id?: string }).__baml_context_id = contextId;
    const toolChoice = await (globalThis as unknown as { ChooseTonyMemoryTool: (a: { user_message: string }) => Promise<{ tool_name?: string; limit?: number }> }).ChooseTonyMemoryTool({ user_message: text });
    const toolName = typeof toolChoice?.tool_name === "string" ? toolChoice.tool_name : "memory/tony";
    const toolArgs = { limit: typeof toolChoice?.limit === "number" ? toolChoice.limit : 6 };
    const toolResult = await (globalThis as unknown as { invokeTool: (name: string, args: unknown) => Promise<{ memory?: string[] }> }).invokeTool(toolName, toolArgs);
    const memory = Array.isArray(toolResult?.memory) ? toolResult.memory : [];
    return await (globalThis as unknown as { TonyShrinkChat: (a: { user_message: string; conversation_memory: string[] }) => Promise<string> }).TonyShrinkChat({
      user_message: text,
      conversation_memory: memory,
    });
  } catch {
    return "Alright, I got nothin'. Try sayin' that again.";
  }
}

async function handle_a2a_request(request: A2aJsonRpcRequest): Promise<void> {
  const method = request?.method;
  const params = request?.params;
  if (!params?.message) {
    __baml_a2a_yield({ message: newMessage("resp-unknown", "I don't know what to do with that.") });
    return;
  }
  const msg = params.message as Message;
  const text = extractText(params);
  const messageId = msg.messageId ?? "msg-unknown";
  const contextId = (msg.contextId as string) ?? "ctx-unknown";

  if (method === "message.sendStream") {
    addMemory(contextId, text);
    const body = await buildBamlResponse(text, contextId);
    __baml_a2a_yield({ message: newMessage(`resp-${messageId}`, body) });
    return;
  }
  __baml_a2a_yield({ message: newMessage(`resp-${messageId}`, `I don't know what to do with "${text}".`) });
}

__baml_a2a_register({ handle_a2a_request, tools: { "memory/tony": (args: unknown) => tony_memory(args as { limit?: number }) } });

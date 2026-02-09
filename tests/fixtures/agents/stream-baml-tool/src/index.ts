/**
 * Fixture: stream-baml-tool.
 * Tests async streaming of a BAML tool (FSM) result driven by message.sendStream.
 * Single path: call ChooseCalcTool, return stream with message (sum=...).
 */
import type {
  A2aJsonRpcRequest,
  A2aStreamChunk,
  Message,
  SendMessageRequest,
  Task,
} from "./a2a";

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
  return { messageId, role: "ROLE_AGENT", parts: [{ text }] };
}

function newTask(taskId: string, contextId: string, message?: Message): Task {
  return {
    id: taskId,
    contextId,
    status: { state: "TASK_STATE_WORKING" },
    history: message ? [message] : [],
  };
}

async function handle_a2a_request(request: A2aJsonRpcRequest): Promise<void> {
  const method = request?.method;
  const params = request?.params;
  const text = params && typeof params === "object" ? extractText(params) : "unknown";
  const msg = params?.message as Message | undefined;
  const messageId = msg?.messageId ?? "msg-001";
  const contextId = (msg?.contextId as string) ?? "ctx-001";
  const taskId = `task-${messageId}`;

  if (method === "message.sendStream") {
    try {
      const toolResult = await (globalThis as unknown as { ChooseCalcTool: (a: { user_message: string }) => Promise<{ result?: number } | undefined> }).ChooseCalcTool({
        user_message: text,
      });
      if (toolResult != null && typeof toolResult === "object" && "result" in toolResult) {
        const chunk: A2aStreamChunk = {
          message: newMessage(`resp-${messageId}`, `BAML tool result: sum=${(toolResult as { result: number }).result}`),
          task: newTask(taskId, contextId, msg),
        };
        __baml_a2a_yield(chunk);
        return;
      }
      throw new Error("BAML tool returned no output");
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      __baml_a2a_yield({ message: newMessage(`resp-${messageId}`, `Error: ${errMsg}`) });
      return;
    }
  }

  __baml_a2a_yield({ message: newMessage(`resp-${messageId}`, `Unknown method: ${method}`) });
}

__baml_a2a_register({ handle_a2a_request });

import type {
  A2aJsonRpcRequest,
  A2aStreamChunk,
  Artifact,
  Message,
  SendMessageRequest,
  Task,
  TaskArtifactUpdateEvent,
  TaskStatusUpdateEvent,
} from "./a2a";

const LONG_RITE_TOKEN = "long-rite";
const taskState: Record<string, string> = {};

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
  taskState[taskId] = "TASK_STATE_WORKING";
  return {
    id: taskId,
    contextId,
    status: { state: "TASK_STATE_WORKING" },
    history: message ? [message] : [],
  };
}

function fakeStreamRiteTool(text: string, taskId: string, contextId: string, msg?: Message): void {
  if (taskState[taskId] === "TASK_STATE_CANCELED") {
    __baml_a2a_yield({
      statusUpdate: {
        taskId,
        contextId,
        status: {
          state: "TASK_STATE_CANCELED",
          message: newMessage("rite-canceled", `Rite canceled: ${text}`),
        },
      },
    });
    return;
  }
  __baml_a2a_yield({
    statusUpdate: {
      taskId,
      contextId,
      status: {
        state: "TASK_STATE_WORKING",
        message: newMessage("rite-status", `Rite underway: ${text}`),
      },
    } as TaskStatusUpdateEvent,
  });
  __baml_a2a_yield({
    artifactUpdate: {
      taskId,
      contextId,
      append: false,
      lastChunk: true,
      artifact: {
        artifactId: "rite-log-001",
        name: "Rite Log",
        description: "Compiled litany fragments",
        parts: [
          {
            mediaType: "application/json",
            data: { step: "seal", omen: "frost on the reactor glyphs", note: "recite canticle XVII" },
          },
        ],
      } as Artifact,
    } as TaskArtifactUpdateEvent,
  });
  __baml_a2a_yield({ message: newMessage("rite-msg-001", `Rite complete: ${text}`) });
}

async function handle_a2a_request(request: A2aJsonRpcRequest): Promise<void> {
  const method = request?.method;
  const params = request?.params;
  const text = params && typeof params === "object" ? extractText(params) : "unknown";
  const msg = params?.message as Message | undefined;
  const messageId = msg?.messageId ?? "msg-void-001";
  const contextId = (msg?.contextId as string) ?? "ctx-void-001";
  const taskId = `rite-task-${messageId}`;

  if (method === "message.sendStream") {
    if (text.includes(LONG_RITE_TOKEN)) {
      fakeStreamRiteTool(text, taskId, contextId, msg);
      return;
    }
    try {
      const toolResult = await (globalThis as unknown as { ChooseRiteTool: (a: { user_message: string }) => Promise<{ result?: number } | undefined> }).ChooseRiteTool({
        user_message: text,
      });
      if (toolResult != null && typeof toolResult === "object" && "result" in toolResult) {
        __baml_a2a_yield({
          message: newMessage(`resp-${messageId}`, `BAML rite complete: sum=${(toolResult as { result: number }).result}`),
          task: newTask(taskId, contextId, msg),
        });
        return;
      }
      throw new Error("BAML tool returned no output");
    } catch {
      // fallback
    }
    __baml_a2a_yield({ message: newMessage(`resp-${messageId}`, `Blessings upon ${text}`) });
    return;
  }

  __baml_a2a_yield({ message: newMessage(`resp-${messageId}`, `Unknown rite for ${text}`) });
}

async function handle_a2a_cancel(args: { id: string; tenant?: string }): Promise<void> {
  const taskId = args?.id ?? "unknown";
  taskState[taskId] = "TASK_STATE_CANCELED";
}

__baml_a2a_register({ handle_a2a_request, handle_a2a_cancel });

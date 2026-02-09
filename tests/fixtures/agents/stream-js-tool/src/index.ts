/**
 * Fixture: stream-js-tool.
 * Tests streaming of a JS-only result (statusUpdate, artifactUpdate, message) and tasks.cancel.
 * Trigger: message text containing "stream-task".
 */
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

const TRIGGER = "stream-task";
const taskState: Record<string, string> = {};

function newTask(taskId: string, contextId: string, message?: Message): Task {
  taskState[taskId] = "TASK_STATE_WORKING";
  return {
    id: taskId,
    contextId,
    status: { state: "TASK_STATE_WORKING" },
    history: message ? [message] : [],
  };
}

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

function fakeStream(
  text: string,
  taskId: string,
  contextId: string,
  msg?: Message
): void {
  if (taskState[taskId] === "TASK_STATE_CANCELED") {
    __baml_a2a_yield({
      statusUpdate: {
        taskId,
        contextId,
        status: {
          state: "TASK_STATE_CANCELED",
          message: newMessage("canceled", `Canceled: ${text}`),
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
        message: newMessage("status", `Working: ${text}`),
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
        artifactId: "art-001",
        name: "Artifact",
        description: "Fixture artifact",
        parts: [{ mediaType: "application/json", data: { done: true } }],
      } as Artifact,
    } as TaskArtifactUpdateEvent,
  });
  __baml_a2a_yield({ task: newTask(taskId, contextId, msg) });
  __baml_a2a_yield({ message: newMessage("msg-001", `Complete: ${text}`) });
}

async function handle_a2a_request(request: A2aJsonRpcRequest): Promise<void> {
  const method = request?.method;
  const params = request?.params;
  const text = params && typeof params === "object" ? extractText(params) : "unknown";
  const msg = params?.message as Message | undefined;
  const messageId = msg?.messageId ?? "msg-001";
  const contextId = (msg?.contextId as string) ?? "ctx-001";
  const taskId = `task-${messageId}`;

  if (method === "message.sendStream" && text.includes(TRIGGER)) {
    fakeStream(text, taskId, contextId, msg);
    return;
  }

  __baml_a2a_yield({ message: newMessage(`resp-${messageId}`, `Unknown or no trigger: ${text}`) });
}

async function handle_a2a_cancel(args: { id: string; tenant?: string }): Promise<void> {
  const taskId = args?.id ?? "unknown";
  taskState[taskId] = "TASK_STATE_CANCELED";
}

__baml_a2a_register({ handle_a2a_request, handle_a2a_cancel });

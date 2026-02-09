/**
 * Fixture: stream-js-tool.
 * Tests streaming of a JS-only result (statusUpdate, artifactUpdate, message).
 * Trigger: message text containing "stream-task".
 */
import type {
  Artifact,
  ChatMessage,
  ChatStreamChunk,
  Task,
  TaskArtifactUpdateEvent,
  TaskStatusUpdateEvent,
} from "./a2a";

const TRIGGER = "stream-task";
type SimpleMessage = { parts: { text: string }[] };

function newTask(message?: SimpleMessage): Task {
  return {
    status: { state: "TASK_STATE_WORKING", message },
  };
}

function extractText(message: ChatMessage | null | undefined): string {
  if (!message?.parts?.length) return "unknown";
  const first = message.parts[0];
  if (first && typeof (first as { text?: string }).text === "string") {
    return (first as { text: string }).text;
  }
  return "unknown";
}

function newMessage(text: string): { parts: { text: string }[] } {
  return { parts: [{ text }] };
}

function normalizeMessage(msg?: ChatMessage | null): SimpleMessage | undefined {
  if (!msg?.parts?.length) return undefined;
  const parts = msg.parts
    .filter((part): part is { text: string } => typeof (part as { text?: string })?.text === "string")
    .map((part) => ({ text: (part as { text: string }).text }));
  return parts.length ? { parts } : undefined;
}

function fakeStream(text: string, msg?: ChatMessage | null): void {
  __baml_chat_yield({
    statusUpdate: {
      status: {
        state: "TASK_STATE_WORKING",
        message: newMessage(`Working: ${text}`),
      },
    } as TaskStatusUpdateEvent,
  });
  __baml_chat_yield({
    artifactUpdate: {
      append: false,
      lastChunk: true,
      artifact: {
        name: "Artifact",
        description: "Fixture artifact",
        parts: [{ mediaType: "application/json", data: { done: true } }],
      } as Artifact,
    } as TaskArtifactUpdateEvent,
  });
  __baml_chat_yield({ task: newTask(normalizeMessage(msg)) });
  __baml_chat_yield({ message: newMessage(`Complete: ${text}`) });
}

async function onChatMessage(message: ChatMessage): Promise<void> {
  const text = extractText(message);
  if (text.includes(TRIGGER)) {
    fakeStream(text, message);
    return;
  }
  __baml_chat_yield({ message: newMessage(`Unknown or no trigger: ${text}`) });
}

__baml_chat_register({ onChatMessage });

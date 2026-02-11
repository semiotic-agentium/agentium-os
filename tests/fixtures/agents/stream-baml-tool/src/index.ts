/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: stream-baml-tool.
 * Tests async streaming of a BAML tool (FSM) result driven by message.sendStream.
 * Single path: call ChooseCalcTool, return stream with message (sum=...).
 */
import type { ChatMessage, ChatStreamChunk, Task } from "./a2a";

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

function newTask(message?: { parts: { text: string }[] }): Task {
  return {
    status: { state: "TASK_STATE_WORKING", message },
  };
}

async function onChatMessage(message: ChatMessage): Promise<void> {
  const text = extractText(message);
  try {
    const toolResult = await ChooseCalcTool({ ...message, user_message: text });
    if (toolResult != null && typeof toolResult === "object" && "result" in toolResult) {
      const chunk: ChatStreamChunk = {
        message: newMessage(`BAML tool result: sum=${(toolResult as { result: number }).result}`),
        task: newTask(message),
      };
      __baml_chat_yield(chunk);
      return;
    }
    throw new Error("BAML tool returned no output");
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    __baml_chat_yield({ message: newMessage(`Error: ${errMsg}`) });
  }
}

__baml_chat_register({ onChatMessage });

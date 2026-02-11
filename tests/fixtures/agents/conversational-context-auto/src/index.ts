/// <reference path="./baml-runtime.d.ts" />
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

function shouldCompute(text: string): boolean {
  return /\d+\s*[\+\-\*\/]\s*\d+/.test(text) || text.toLowerCase().includes("compute");
}

async function onChatMessage(message: ChatMessage): Promise<void> {
  const text = extractText(message);

  try {
    if (shouldCompute(text)) {
      const toolResult = await ChooseCalcTool({ ...message, user_message: text });
      if (toolResult != null && typeof toolResult === "object" && "result" in toolResult) {
        const result = (toolResult as { result: number }).result;
        const chunk: ChatStreamChunk = {
          message: newMessage(`Computed result is ${result}. I will remember this conversation.`),
          task: newTask(message),
        };
        __baml_chat_yield(chunk);
        return;
      }
      throw new Error("BAML tool returned no output");
    }

    const reply = await ChatWithContext({ ...message, user_message: text });
    const chunk: ChatStreamChunk = {
      message: newMessage(String(reply)),
      task: newTask(message),
    };
    __baml_chat_yield(chunk);
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    __baml_chat_yield({ message: newMessage(`Error: ${errMsg}`) });
  }
}

__baml_chat_register({ onChatMessage });

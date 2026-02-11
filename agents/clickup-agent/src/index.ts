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

async function onChatMessage(
  message: ChatMessage & { __baml_invocation_token?: string }
): Promise<void> {
  const text = extractText(message);
  const token = message.__baml_invocation_token;

  try {
    const toolResult = await ChooseClickUpAction({
      user_message: text,
      __baml_invocation_token: token,
    });

    if (toolResult != null && typeof toolResult === "object") {
      const output = toolResult as {
        action?: string;
        tasks?: { name: string; status: string; url: string }[];
        items?: { id: string; name: string; kind: string }[];
        message?: string;
      };

      let response = output.message || "Done.";
      if (output.items && output.items.length > 0) {
        const itemList = output.items
          .map((i) => `• [${i.kind}] ${i.name} (id: ${i.id})`)
          .join("\n");
        response += "\n\n" + itemList;
      }
      if (output.tasks && output.tasks.length > 0) {
        const taskList = output.tasks
          .map((t) => `• ${t.name} [${t.status}] — ${t.url}`)
          .join("\n");
        response += "\n\n" + taskList;
      }

      __baml_chat_yield({
        message: newMessage(response),
        task: newTask(newMessage(response)),
      });
      return;
    }

    __baml_chat_yield({
      message: newMessage("ClickUp action completed but returned no data."),
      task: newTask(),
    });
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    __baml_chat_yield({ message: newMessage(`Error: ${errMsg}`) });
  }
}

__baml_chat_register({ onChatMessage });

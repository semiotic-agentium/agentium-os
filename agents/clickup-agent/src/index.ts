/// <reference path="./baml-runtime.d.ts" />
declare function ChooseClickUpAction(args?: Record<string, unknown>): Promise<unknown>;

type ChatMessageWithToken = ChatMessage & { __baml_invocation_token?: string };

async function onChatMessage(message: ChatMessageWithToken): Promise<void> {
  const s = session(message);
  await s.run(async () => {
    const text = s.text() || "unknown";
    const token = message.__baml_invocation_token;

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

      return { message: response };
    }

    return { message: "ClickUp action completed but returned no data." };
  });
}

__chat_register({ onChatMessage });

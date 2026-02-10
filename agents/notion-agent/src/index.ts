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

function isWriteRequest(text: string): boolean {
  const lowered = text.toLowerCase();
  const keywords = ["create", "update", "edit", "delete", "archive", "write", "add page"];
  return keywords.some((k) => lowered.includes(k));
}

function formatSources(sources?: { page_id: string; url: string }[]): string {
  if (!sources || sources.length === 0) return "";
  const lines = sources.map((s) => `• ${s.url}`);
  return "\n\nSources:\n" + lines.join("\n");
}

function formatPages(pages?: { title: string; url: string }[]): string {
  if (!pages || pages.length === 0) return "";
  const lines = pages.map((p) => `• ${p.title} — ${p.url}`);
  return "\n\nPages:\n" + lines.join("\n");
}

async function summarizeBlocks(args: {
  user_message: string;
  page_title: string | null;
  page_url: string | null;
  blocks_text: string;
}): Promise<string | null> {
  try {
    const result = await SummarizeNotionContent(args);
    if (result && typeof result === "string") return result;
    if (result && typeof result === "object") {
      const output = result as { summary?: string };
      if (output.summary) return output.summary;
    }
  } catch {
    return null;
  }
  return null;
}

async function onChatMessage(
  message: ChatMessage & { __baml_invocation_token?: string }
): Promise<void> {
  const text = extractText(message);
  const token = message.__baml_invocation_token;

  if (isWriteRequest(text)) {
    const msg =
      "This Notion tool is read-only in the MVP. I can search pages or summarize page content.";
    __baml_chat_yield({ message: newMessage(msg), task: newTask(newMessage(msg)) });
    return;
  }

  try {
    const toolResult = await ChooseNotionAction({
      user_message: text,
      __baml_invocation_token: token,
    });

    if (toolResult != null && typeof toolResult === "object") {
      const output = toolResult as {
        message?: string;
        pages?: { title: string; url: string }[];
        blocks?: { text?: string | null }[];
        sources?: { page_id: string; url: string }[];
      };

      let response = output.message || "Done.";
      response += formatPages(output.pages);

      const blocksText = (output.blocks || [])
        .map((b) => b.text)
        .filter((t): t is string => Boolean(t && t.trim()))
        .join("\n");

      if (blocksText.length > 0) {
        const pageTitle = output.pages && output.pages[0] ? output.pages[0].title : null;
        const pageUrl = output.pages && output.pages[0] ? output.pages[0].url : null;
        const summary = await summarizeBlocks({
          user_message: text,
          page_title: pageTitle,
          page_url: pageUrl,
          blocks_text: blocksText.slice(0, 8000),
        });
        if (summary) {
          response += `\n\nSummary:\n${summary}`;
        }
      }

      response += formatSources(output.sources);

      __baml_chat_yield({
        message: newMessage(response),
        task: newTask(newMessage(response)),
      });
      return;
    }

    __baml_chat_yield({
      message: newMessage("Notion action completed but returned no data."),
      task: newTask(),
    });
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    __baml_chat_yield({ message: newMessage(`Error: ${errMsg}`) });
  }
}

__baml_chat_register({ onChatMessage });

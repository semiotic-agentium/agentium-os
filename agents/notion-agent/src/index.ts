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

function wantsSummary(text: string): boolean {
  const lowered = text.toLowerCase();
  const keywords = [
    "summarize",
    "summary",
    "what are we working on",
    "status",
    "impact",
    "roadmap",
    "brief",
    "commitments",
  ];
  return keywords.some((k) => lowered.includes(k));
}

type NotionPageSummary = { id: string; title: string; url: string };
type NotionBlockSummary = { text?: string | null };
type NotionSource = { page_id: string; url: string };

type NotionOutput = {
  message?: string;
  pages?: NotionPageSummary[];
  blocks?: NotionBlockSummary[];
  sources?: NotionSource[];
};

type ReadOnlyResponse = {
  message: string;
  next_step?: string;
};

type NotionActionResult = NotionOutput | ReadOnlyResponse | null;

function isReadOnlyResponse(action: NotionActionResult): action is ReadOnlyResponse {
  if (!action || typeof action !== "object") return false;
  const candidate = action as Record<string, unknown>;
  if (typeof candidate.message !== "string") return false;
  const hasPages = "pages" in candidate || "blocks" in candidate || "sources" in candidate;
  return !hasPages;
}

function formatSources(
  sources?: NotionSource[],
  pages?: NotionPageSummary[]
): string {
  if (!sources || sources.length === 0) return "";
  const pageTitleById = new Map<string, string>();
  (pages || []).forEach((p) => pageTitleById.set(p.id, p.title));
  const lines = sources.map((s) => {
    const title = pageTitleById.get(s.page_id);
    return title ? `• ${title} — ${s.url}` : `• ${s.url}`;
  });
  return "\n\nSources:\n" + lines.join("\n");
}

function formatPages(pages?: NotionPageSummary[]): string {
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
  } catch (err) {
    console.warn("SummarizeNotionContent failed", err);
    return null;
  }
  return null;
}

async function onChatMessage(
  message: ChatMessage & { __baml_invocation_token?: string }
): Promise<void> {
  const text = extractText(message);
  const token = message.__baml_invocation_token;

  try {
    const toolResult = await ChooseNotionAction({
      user_message: text,
      __baml_invocation_token: token,
    });

    if (isReadOnlyResponse(toolResult)) {
      const nextStep = toolResult.next_step
        ? `\n\nNext step:\n- ${toolResult.next_step}`
        : "";
      const msg = `${toolResult.message}${nextStep}`;
      __baml_chat_yield({ message: newMessage(msg), task: newTask(newMessage(msg)) });
      return;
    }

    if (toolResult != null && typeof toolResult === "object") {
      const output = toolResult as NotionOutput;

      let response = output.message || "Done.";
      response += formatPages(output.pages);

      const blocksText = (output.blocks || [])
        .map((b) => b.text)
        .filter((t): t is string => Boolean(t && t.trim()))
        .join("\n");

      if (blocksText.length > 0) {
        const pageTitle = output.pages && output.pages[0] ? output.pages[0].title : null;
        const pageUrl = output.pages && output.pages[0] ? output.pages[0].url : null;
        const truncated = blocksText.length > 8000 ? blocksText.slice(0, 8000) : blocksText;
        const summary = await summarizeBlocks({
          user_message: text,
          page_title: pageTitle,
          page_url: pageUrl,
          blocks_text: truncated,
        });
        if (summary) {
          response += `\n\nSummary:\n${summary}`;
        }
      } else if (wantsSummary(text)) {
        response +=
          "\n\nMissing:\n- Page content not retrieved. Provide a Notion page link or ID, or ensure the integration has access.";
      }

      if ((!output.pages || output.pages.length === 0) && (!output.blocks || output.blocks.length === 0)) {
        response +=
          "\n\nMissing:\n- No Notion pages found for this request. Provide a page link or adjust the query, or ensure the integration has access.";
      }

      response += formatSources(output.sources, output.pages);

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

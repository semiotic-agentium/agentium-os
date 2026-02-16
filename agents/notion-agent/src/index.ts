/// <reference path="./baml-runtime.d.ts" />

type ChatMessageWithToken = ChatMessage;

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
      const output = result as { summary?: string; message?: string; text?: string };
      if (output.summary) return output.summary;
      if (output.message) return output.message;
      if (output.text) return output.text;
      console.warn("SummarizeNotionContent returned unexpected shape", result);
      return JSON.stringify(result);
    }
  } catch (err) {
    console.warn("SummarizeNotionContent failed", err);
    return null;
  }
  return null;
}

async function onChatMessage(message: ChatMessageWithToken): Promise<void> {
  const s = session(message);
  await s.run(async () => {
    const text = s.text() || "unknown";

    const toolResult = await ChooseNotionAction({
      user_message: text,
    });

    if (isReadOnlyResponse(toolResult)) {
      const nextStep = toolResult.next_step
        ? `\n\nNext step:\n- ${toolResult.next_step}`
        : "";
      return { message: `${toolResult.message}${nextStep}` };
    }

    if (toolResult != null && typeof toolResult === "object") {
      const output = toolResult as NotionOutput;

      let response = output.message || "Done.";
      if (response === "Retrieved page blocks") {
        response = "Notion summary:";
      }
      response += formatPages(output.pages);

      const blocksText = (output.blocks || [])
        .map((b) => b.text)
        .filter((t): t is string => Boolean(t && t.trim()))
        .join("\n");
      const notableLines = (output.blocks || [])
        .filter((b) => b.block_type === "bulleted_list_item" && b.text)
        .map((b) => b.text as string)
        .filter((t) => t.trim().length > 0);
      const notableSection =
        notableLines.length > 0
          ? `Notable lines:\n- ${Array.from(new Set(notableLines)).join("\n- ")}\n\n`
          : "";

      if (blocksText.length > 0) {
        const pageTitle = output.pages && output.pages[0] ? output.pages[0].title : null;
        const pageUrl = output.pages && output.pages[0] ? output.pages[0].url : null;
        const combinedText = `${notableSection}Full content:\n${blocksText}`;
        const truncated =
          combinedText.length > 8000 ? combinedText.slice(0, 8000) : combinedText;
        const summary = await summarizeBlocks({
          user_message: text,
          page_title: pageTitle,
          page_url: pageUrl,
          blocks_text: truncated,
        });
        if (summary) {
          let formattedSummary = summary;
          if (output.sources && output.sources.length > 0) {
            formattedSummary = formattedSummary.replace(
              /Sources:\s*(?:-?\s*None.*)?/gi,
              ""
            );
          }
          response += `\n\nSummary:\n${formattedSummary.trim()}`;
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

      return { message: response };
    }

    return { message: "Notion action completed but returned no data." };
  });
}

__chat_register({ onChatMessage });

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
  const keywords = ["create", "edit", "delete", "archive", "write"];
  const phrases = [
    "create page",
    "update page",
    "edit page",
    "delete page",
    "archive page",
    "add page",
  ];
  return (
    keywords.some((k) => lowered.includes(`${k} `)) ||
    phrases.some((p) => lowered.includes(p))
  );
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

function formatSources(
  sources?: { page_id: string; url: string }[],
  pages?: { id: string; title: string; url: string }[]
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

async function fetchBlocksForPage(args: {
  page_id: string;
  token?: string;
}): Promise<{
  pages?: { id: string; title: string; url: string }[];
  blocks?: { text?: string | null }[];
  sources?: { page_id: string; url: string }[];
  message?: string;
  } | null> {
  if (!args.token) return null;
  let session: { send: (input: { block_id: string }) => Promise<void>; continue: () => Promise<{ output?: unknown } | null>; finish: () => Promise<void> } | null = null;
  try {
    session = await openToolSession("support/notionGetPageBlocks", args.token);
    await session.send({ block_id: args.page_id });
    const step = await session.continue();
    if (step && step.output && typeof step.output === "object") {
      return step.output as {
        pages?: { id: string; title: string; url: string }[];
        blocks?: { text?: string | null }[];
        sources?: { page_id: string; url: string }[];
        message?: string;
      };
    }
  } catch {
    return null;
  } finally {
    if (session) {
      try {
        await session.finish();
      } catch {
        // ignore
      }
    }
  }
  return null;
}

function mergeOutputs(
  base: {
    message?: string;
    pages?: { id: string; title: string; url: string }[];
    blocks?: { text?: string | null }[];
    sources?: { page_id: string; url: string }[];
  },
  extra: {
    message?: string;
    pages?: { id: string; title: string; url: string }[];
    blocks?: { text?: string | null }[];
    sources?: { page_id: string; url: string }[];
  }
) {
  return {
    message: extra.message || base.message,
    pages: (extra.pages && extra.pages.length > 0 ? extra.pages : base.pages) || [],
    blocks: (extra.blocks && extra.blocks.length > 0 ? extra.blocks : base.blocks) || [],
    sources: (extra.sources && extra.sources.length > 0 ? extra.sources : base.sources) || [],
  };
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
        pages?: { id: string; title: string; url: string }[];
        blocks?: { text?: string | null }[];
        sources?: { page_id: string; url: string }[];
      };

      let merged = output;
      if (
        wantsSummary(text) &&
        output.pages &&
        output.pages.length > 0 &&
        (!output.blocks || output.blocks.length === 0)
      ) {
        const pagesToExpand = output.pages.slice(0, 3);
        const mergedBlocks: { text?: string | null }[] = [];
        const mergedSources: { page_id: string; url: string }[] = [];
        for (const page of pagesToExpand) {
          const blocksResult = await fetchBlocksForPage({
            page_id: page.id,
            token,
          });
          if (blocksResult) {
            mergedBlocks.push(...(blocksResult.blocks || []));
            mergedSources.push(...(blocksResult.sources || []));
          }
        }
        if (mergedBlocks.length > 0) {
          merged = mergeOutputs(output, { blocks: mergedBlocks, sources: mergedSources });
        }
      }

      let response = merged.message || "Done.";
      response += formatPages(merged.pages);

      const blocksText = (merged.blocks || [])
        .map((b) => b.text)
        .filter((t): t is string => Boolean(t && t.trim()))
        .join("\n");

      if (blocksText.length > 0) {
        const pageTitle = merged.pages && merged.pages[0] ? merged.pages[0].title : null;
        const pageUrl = merged.pages && merged.pages[0] ? merged.pages[0].url : null;
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

      if ((!merged.pages || merged.pages.length === 0) && (!merged.blocks || merged.blocks.length === 0)) {
        response +=
          "\n\nMissing:\n- No Notion pages found for this request. Provide a page link or adjust the query, or ensure the integration has access.";
      }

      response += formatSources(merged.sources, merged.pages);

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

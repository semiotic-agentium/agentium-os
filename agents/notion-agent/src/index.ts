/// <reference path="./baml-runtime.d.ts" />

type ChatMessageWithToken = ChatMessage;

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

const NOTION_TOOL_NAME = "support/notion";
const NOTION_ID_PATTERN =
  /([0-9a-fA-F]{32}|[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})/;

type NotionPageSummary = { id: string; title: string; url: string };
type NotionBlockSummary = { block_type?: string; text?: string | null };
type NotionSource = { page_id: string; url: string };
type NotionSummary = {
  commitments?: string[];
  conflicts?: string[];
  missing?: string[];
  sources?: string[];
};

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

type NotionSearchPagesInput = {
  query?: string;
  start_cursor?: string;
  page_size?: number;
};

type NotionGetPageInput = {
  page_id: string;
};

type NotionGetPageBlocksInput = {
  block_id: string;
  start_cursor?: string;
  page_size?: number;
  raw_blocks?: "raw" | "enriched";
  max_depth?: number;
};

type NotionToolInput =
  | NotionSearchPagesInput
  | NotionGetPageInput
  | NotionGetPageBlocksInput;

type NotionActionResult = NotionOutput | ReadOnlyResponse | null;

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

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

// Agent pattern: tools return structured data, agent renders UX.
// See docs/agent-patterns.md for the rationale and checklist.
function isReadOnlyResponse(action: NotionActionResult): action is ReadOnlyResponse {
  if (!action || typeof action !== "object") return false;
  const candidate = action as Record<string, unknown>;
  if (typeof candidate.message !== "string") return false;
  const hasPages =
    "pages" in candidate || "blocks" in candidate || "sources" in candidate;
  return !hasPages;
}

function isNotionOutput(value: unknown): value is NotionOutput {
  if (!isObject(value)) return false;
  return (
    Array.isArray((value as NotionOutput).pages) ||
    Array.isArray((value as NotionOutput).blocks) ||
    Array.isArray((value as NotionOutput).sources)
  );
}

function extractNotionOutput(value: unknown): NotionOutput | null {
  if (isNotionOutput(value)) return value;
  if (isObject(value) && isNotionOutput(value.output)) {
    return value.output;
  }
  return null;
}

function extractNotionId(text: string): string | null {
  const match = text.match(NOTION_ID_PATTERN);
  if (!match) return null;

  const candidate = match[1] ?? match[0];
  const trimmed = text.trim();
  if (trimmed === candidate) return candidate;

  const lowered = text.toLowerCase();
  if (
    lowered.includes("notion") ||
    lowered.includes("block") ||
    lowered.includes("notion.so") ||
    lowered.includes("notion.site")
  ) {
    return candidate;
  }

  return null;
}

async function executeNotionAction(
  input: NotionToolInput,
): Promise<NotionOutput | null> {
  let session: ToolSessionHandle | null = null;
  try {
    session = await openToolSession(NOTION_TOOL_NAME);
    await session.send(input as unknown as Record<string, unknown>);
    const next = await session.continue();
    await session.finish();
    session = null;
    return extractNotionOutput(next);
  } catch (err) {
    if (session) {
      const reason = err instanceof Error ? err.message : String(err);
      try {
        await session.abort(reason);
      } catch {
        // Ignore abort errors because we're already on the error path.
      }
    }
    throw err;
  }
}

function formatSources(sources?: NotionSource[], pages?: NotionPageSummary[]): string {
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

function formatSummaryLines(label: string, items?: string[]): string {
  if (!items || items.length === 0) return `${label}:\n- None found`;
  const lines = items.map((item) => `- ${item}`);
  return `${label}:\n${lines.join("\n")}`;
}

function renderSummary(summary: NotionSummary): string {
  const commitments = formatSummaryLines("Commitments", summary.commitments);
  const conflicts = formatSummaryLines("Conflicts", summary.conflicts);
  const missing = formatSummaryLines("Missing", summary.missing);
  const sources = formatSummaryLines("Sources", summary.sources);
  return [commitments, conflicts, missing, sources].join("\n");
}

async function summarizeBlocks(args: {
  user_message: string;
  page_title: string | null;
  page_url: string | null;
  blocks_text: string;
}): Promise<NotionSummary | null> {
  try {
    const result = await SummarizeNotionContent(args);
    if (result && typeof result === "object") {
      const output = result as NotionSummary;
      if (
        output.commitments ||
        output.conflicts ||
        output.missing ||
        output.sources
      ) {
        return output;
      }
      console.warn("SummarizeNotionContent returned unexpected shape", result);
      return null;
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

    let toolResult: NotionActionResult = null;
    const directId = extractNotionId(text);
    if (directId) {
      try {
        toolResult = await executeNotionAction({
          block_id: directId,
          max_depth: 2,
        });
      } catch (err) {
        console.warn("Direct Notion ID lookup failed, falling back to LLM", err);
        toolResult = await ChooseNotionAction({ user_message: text });
      }
    } else {
      toolResult = await ChooseNotionAction({
        user_message: text,
      });
    }

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

      let renderedSummary = false;
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
          if (output.sources && output.sources.length > 0 && !summary.sources) {
            summary.sources = output.sources.map((source) => source.url);
          }
          response += `\n\nSummary:\n${renderSummary(summary)}`;
          renderedSummary = true;
        }
      } else if (wantsSummary(text)) {
        response +=
          "\n\nMissing:\n- Page content not retrieved. Provide a Notion page link or ID, or ensure the integration has access.";
      }

      if (
        (!output.pages || output.pages.length === 0) &&
        (!output.blocks || output.blocks.length === 0)
      ) {
        response +=
          "\n\nMissing:\n- No Notion pages found for this request. Provide a page link or adjust the query, or ensure the integration has access.";
      }

      if (!renderedSummary) {
        response += formatSources(output.sources, output.pages);
      }

      return { message: response };
    }

    return { message: "Notion action completed but returned no data." };
  });
}

__chat_register({ onChatMessage });

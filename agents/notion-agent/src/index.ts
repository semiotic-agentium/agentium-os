/// <reference path="./baml-runtime.d.ts" />

type ChatMessageWithToken = ChatMessage & {
  __baml_invocation_token?: string;
};

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
const MAX_REACT_STEPS = 8;
const MAX_FINGERPRINT_CHARS = 6000;
const MAX_CONSECUTIVE_REPEATS = 2;
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

type SupportNotionSessionStep = {
  op?: string;
  input?: Record<string, unknown>;
  initial_input?: Record<string, unknown>;
  reason?: string;
};

type SupportNotionSessionPlan = {
  steps: SupportNotionSessionStep[];
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

function normalizeUserMessage(text: string): string {
  const trimmed = text.trim();
  const notionDirective = trimmed.match(/^use\s+notion\s*[:,-]?\s*/i);
  if (!notionDirective) return trimmed;
  const withoutDirective = trimmed.slice(notionDirective[0].length).trim();
  return withoutDirective.length > 0 ? withoutDirective : trimmed;
}

function looksLikePlaceholderSummary(message: string): boolean {
  const lowered = message.toLowerCase();
  const hasStructuredSummary =
    lowered.includes("commitments:") ||
    lowered.includes("conflicts:") ||
    lowered.includes("missing:") ||
    lowered.includes("sources:");
  if (hasStructuredSummary) return false;
  return (
    lowered.includes("let me provide") ||
    lowered.includes("i can summarize") ||
    lowered.includes("i can provide") ||
    lowered.includes("i already have")
  );
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

// Agent pattern: tools return structured data, agent renders UX.
// See docs/agent-patterns.md for the rationale and checklist.
function isReadOnlyResponse(action: unknown): action is ReadOnlyResponse {
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

function isSessionPlan(value: unknown): value is SupportNotionSessionPlan {
  if (!isObject(value) || !Array.isArray(value.steps)) return false;
  return value.steps.every((step) => {
    if (!isObject(step) || typeof step.op !== "string") return false;
    if (step.op === "Send") return isObject(step.input);
    return true;
  });
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

async function executeNotionPlan(
  plan: SupportNotionSessionPlan,
): Promise<NotionOutput | null> {
  let session: ToolSessionHandle | null = null;
  let lastStepOutput: unknown = null;

  for (const step of plan.steps) {
    switch (step.op) {
      case "Open":
        if (!session) {
          session = await openToolSession(NOTION_TOOL_NAME, step.initial_input);
        }
        break;
      case "Send":
        if (!session) session = await openToolSession(NOTION_TOOL_NAME);
        await session.send(step.input || {});
        break;
      case "Next":
        if (!session) session = await openToolSession(NOTION_TOOL_NAME);
        lastStepOutput = await session.continue();
        break;
      case "Finish":
        if (session) {
          await session.finish();
          session = null;
        }
        break;
      case "Abort":
        if (session) {
          await session.abort(step.reason);
          session = null;
        }
        return null;
      default:
        return null;
    }
  }

  if (session) {
    lastStepOutput = await session.continue();
    await session.finish();
  }

  return extractNotionOutput(lastStepOutput);
}

function truncate(text: string, max: number): string {
  return text.length <= max ? text : text.slice(0, max);
}

function fingerprint(output: NotionOutput): string {
  return truncate(
    JSON.stringify({
      message: output.message || "",
      pages: (output.pages || []).slice(0, 10),
      blocks: (output.blocks || []).slice(0, 25),
      sources: (output.sources || []).slice(0, 25),
    }),
    MAX_FINGERPRINT_CHARS,
  );
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

async function renderReadOnlyResponse(
  response: ReadOnlyResponse,
  userText: string,
): Promise<string> {
  if (wantsSummary(userText) && looksLikePlaceholderSummary(response.message)) {
    const fallback = await executeNotionAction({
      query: userText,
      page_size: 5,
    });
    if (fallback && fallback.pages && fallback.pages.length > 0) {
      let message =
        "I found several potentially relevant Notion pages. Pick one and I will summarize it:";
      message += formatPages(fallback.pages);
      message +=
        "\n\nNext step:\n- Reply with one page URL or page ID from the list above.";
      return message;
    }
    return "I need a specific Notion page to produce a reliable summary. Share a page URL/ID, or refine the query.";
  }
  const nextStep = response.next_step
    ? `\n\nNext step:\n- ${response.next_step}`
    : "";
  return `${response.message}${nextStep}`;
}

async function renderNotionOutput(
  output: NotionOutput,
  userText: string,
): Promise<string> {
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
    const truncated = combinedText.length > 8000 ? combinedText.slice(0, 8000) : combinedText;
    const summary = await summarizeBlocks({
      user_message: userText,
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
  } else if (wantsSummary(userText)) {
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

  return response;
}

async function onChatMessage(message: ChatMessageWithToken): Promise<void> {
  const s = session(message);
  await s.run(async () => {
    const originalText = s.text() || "unknown";
    const text = normalizeUserMessage(originalText);
    const token = message.__baml_invocation_token;
    const directId = extractNotionId(text);
    let prevFingerprint: string | null = null;
    let consecutiveRepeats = 0;
    let lastToolOutput: NotionOutput | null = null;

    try {
      for (let step = 1; step <= MAX_REACT_STEPS; step++) {
        let result: unknown;
        if (step === 1 && directId) {
          try {
            result = await executeNotionAction({
              block_id: directId,
              max_depth: 2,
            });
            if (!result) {
              result = await ChooseNotionAction({
                user_message: text,
                __baml_invocation_token: token,
              });
            }
          } catch (err) {
            console.warn("Direct Notion ID lookup failed, falling back to planner", err);
            result = await ChooseNotionAction({
              user_message: text,
              __baml_invocation_token: token,
            });
          }
        } else {
          result = await ChooseNotionAction({
            user_message: text,
            __baml_invocation_token: token,
          });
        }

        if (isSessionPlan(result)) {
          const executedOutput = await executeNotionPlan(result);
          if (executedOutput) {
            result = executedOutput;
          } else {
            return {
              message:
                "Notion planner returned a raw session plan but no tool output was produced.",
            };
          }
        }

        if (isReadOnlyResponse(result)) {
          return { message: await renderReadOnlyResponse(result, text) };
        }

        const output = extractNotionOutput(result);
        if (output) {
          lastToolOutput = output;
          const fp = fingerprint(output);
          if (fp === prevFingerprint) {
            consecutiveRepeats += 1;
          } else {
            consecutiveRepeats = 0;
            prevFingerprint = fp;
          }

          if (consecutiveRepeats >= MAX_CONSECUTIVE_REPEATS) {
            return { message: await renderNotionOutput(output, text) };
          }
          continue;
        }

        if (isObject(result) && typeof result.message === "string") {
          return { message: result.message };
        }

        return { message: "Notion planner returned an unexpected response shape." };
      }

      if (lastToolOutput) {
        const rendered = await renderNotionOutput(lastToolOutput, text);
        return {
          message: `${rendered}\n\nStopped after ${MAX_REACT_STEPS} planning steps.`,
        };
      }

      return {
        message: `Unable to complete the request within ${MAX_REACT_STEPS} planning steps.`,
      };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: `Error: ${errMsg}` };
    }
  });
}

__chat_register({ onChatMessage });

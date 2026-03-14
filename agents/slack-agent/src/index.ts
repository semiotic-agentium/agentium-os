/// <reference path="./baml-runtime.d.ts" />

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

const SLACK_TOOL_NAME = "support/slack";

type SlackAuthPreference = "auto" | "bot" | "user";
type SlackHistoryOrder = "latest_first" | "oldest_first";
type SlackUserResolutionMode = "none" | "resolve_users";
type SlackConversationKind =
  | "public_channel"
  | "private_channel"
  | "im"
  | "mpim";

type ListConversationsInput = {
  kinds: SlackConversationKind[];
  cursor?: string;
  limit?: number;
  exclude_archived?: boolean;
  include_num_members?: boolean;
  auth?: SlackAuthPreference;
};

type GetConversationHistoryInput = {
  channel_id: string;
  cursor?: string;
  limit?: number;
  oldest?: string;
  latest?: string;
  inclusive?: boolean;
  order?: SlackHistoryOrder;
  resolve_users?: SlackUserResolutionMode;
  auth?: SlackAuthPreference;
};

type GetThreadRepliesInput = {
  channel_id: string;
  thread_ts: string;
  cursor?: string;
  limit?: number;
  oldest?: string;
  latest?: string;
  inclusive?: boolean;
  order?: SlackHistoryOrder;
  resolve_users?: SlackUserResolutionMode;
  auth?: SlackAuthPreference;
};

type SlackInput =
  | ListConversationsInput
  | GetConversationHistoryInput
  | GetThreadRepliesInput;

type SlackUserSummary = {
  id: string;
  name?: string;
  display_name?: string;
  real_name?: string;
};

type SlackMessageSummary = {
  channel_id: string;
  ts: string;
  thread_ts?: string;
  user_id?: string;
  user_name?: string;
  text: string;
  source_ref: string;
  permalink?: string;
};

type SlackSource = {
  reference: string;
  permalink?: string;
};

type SlackOutput = {
  messages?: SlackMessageSummary[];
  users?: SlackUserSummary[];
  sources?: SlackSource[];
  has_more?: boolean;
  next_cursor?: string;
  message?: string;
};

type SlackScope =
  | {
      kind: "channel";
      channelId: string;
      oldest?: string;
      latest?: string;
      limit?: number;
    }
  | {
      kind: "thread";
      channelId: string;
      threadTs: string;
      oldest?: string;
      latest?: string;
      limit?: number;
    };

type TodoConfidence = "high" | "medium" | "low";

type TodoItem = {
  task: string;
  owner?: string;
  dueDate?: string;
  confidence: TodoConfidence;
  sources: string[];
};

function extractText(message: ChatMessage): string {
  const parts = message.parts ?? [];
  const textParts = parts
    .map((part) => (typeof part.text === "string" ? part.text : ""))
    .filter((text) => text.length > 0);
  if (textParts.length === 0) return "";
  return textParts.join("\n");
}

function executionMessageId(message: ChatMessage): string {
  if (isObject(message)) {
    const record = message as Record<string, unknown>;
    if (typeof record.messageId === "string" && record.messageId.trim().length > 0) return record.messageId;
    if (typeof record.id === "string" && record.id.trim().length > 0) return record.id;
  }
  return "msg-slack-fallback";
}

function collapseWhitespace(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function parseChannelId(text: string): string | null {
  const match = text.match(/\b([CGD][A-Z0-9]{8,})\b/);
  return match ? match[1] : null;
}

function expandCompactTs(compact: string): string | null {
  if (!/^\d{16}$/.test(compact)) return null;
  const left = compact.slice(0, 10);
  const right = compact.slice(10);
  return `${left}.${right}`;
}

function parseThreadTs(text: string): string | null {
  const direct = text.match(/\b(\d{10}\.\d{6})\b/);
  if (direct) return direct[1];

  const permalink = text.match(/\/p(\d{16})\b/);
  if (permalink) return expandCompactTs(permalink[1]);

  const appThread = text.match(/\/thread\/[CGD][A-Z0-9]{8,}-(\d{16})\b/);
  if (appThread) return expandCompactTs(appThread[1]);

  return null;
}

function parseNumericParam(text: string, key: string): number | undefined {
  const pattern = new RegExp(`${key}\\s*=\\s*(\\d{1,4})`, "i");
  const match = text.match(pattern);
  if (!match) return undefined;
  const parsed = Number(match[1]);
  if (!Number.isFinite(parsed) || parsed <= 0) return undefined;
  return parsed;
}

function parseTsParam(text: string, key: string): string | undefined {
  const pattern = new RegExp(`${key}\\s*=\\s*(\\d{10}(?:\\.\\d{1,6})?)`, "i");
  const match = text.match(pattern);
  if (!match) return undefined;
  return match[1];
}

function parseRangeAndLimit(text: string): {
  oldest?: string;
  latest?: string;
  limit?: number;
} {
  const limitFromParam = parseNumericParam(text, "limit");
  const limitFromPhrase = (() => {
    const match = text.match(/\blast\s+(\d{1,4})\s+messages?\b/i);
    if (!match) return undefined;
    const parsed = Number(match[1]);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
  })();
  const limit = limitFromParam ?? limitFromPhrase;
  return {
    oldest: parseTsParam(text, "oldest"),
    latest: parseTsParam(text, "latest"),
    limit,
  };
}

function parseSlackScope(text: string): SlackScope | null {
  const channelId = parseChannelId(text);
  const threadTs = parseThreadTs(text);
  const range = parseRangeAndLimit(text);
  if (channelId && threadTs) {
    return {
      kind: "thread",
      channelId,
      threadTs,
      oldest: range.oldest,
      latest: range.latest,
      limit: range.limit,
    };
  }
  if (channelId) {
    return {
      kind: "channel",
      channelId,
      oldest: range.oldest,
      latest: range.latest,
      limit: range.limit,
    };
  }
  return null;
}

function isSlackOutput(value: unknown): value is SlackOutput {
  if (!value || typeof value !== "object") return false;
  const maybe = value as SlackOutput;
  return (
    Array.isArray(maybe.messages) ||
    Array.isArray(maybe.users) ||
    Array.isArray(maybe.sources)
  );
}

function extractSlackOutput(value: unknown): SlackOutput | null {
  if (isSlackOutput(value)) return value;
  if (!value || typeof value !== "object") return null;
  const wrapped = value as { output?: unknown };
  if (isSlackOutput(wrapped.output)) return wrapped.output;
  return null;
}

async function runSlackAction(input: SlackInput): Promise<SlackOutput | null> {
  let toolSession: ToolSessionHandle | null = null;
  try {
    toolSession = await openToolSession(SLACK_TOOL_NAME);
    await toolSession.send(input as unknown as Record<string, unknown>);
    const response = await toolSession.continue();
    await toolSession.finish();
    toolSession = null;
    return extractSlackOutput(response);
  } catch (error) {
    if (toolSession) {
      try {
        await toolSession.abort(
          error instanceof Error ? error.message : String(error),
        );
      } catch {
        // Ignore abort failures on error path.
      }
    }
    throw error;
  }
}

function buildSlackAction(scope: SlackScope): SlackInput {
  const shared = {
    limit: scope.limit,
    oldest: scope.oldest,
    latest: scope.latest,
    order: "oldest_first" as SlackHistoryOrder,
    resolve_users: "resolve_users" as SlackUserResolutionMode,
    auth: "auto" as SlackAuthPreference,
  };
  if (scope.kind === "thread") {
    return {
      channel_id: scope.channelId,
      thread_ts: scope.threadTs,
      ...shared,
    };
  }
  return {
    channel_id: scope.channelId,
    ...shared,
  };
}

function buildUserNameById(users: SlackUserSummary[]): Map<string, string> {
  const byId = new Map<string, string>();
  users.forEach((user) => {
    const bestName = user.display_name || user.real_name || user.name;
    if (bestName) byId.set(user.id, bestName);
  });
  return byId;
}

function extractDueDate(text: string): string | undefined {
  const isoDate = text.match(/\b(\d{4}-\d{2}-\d{2})\b/);
  if (isoDate) return isoDate[1];

  const byPhrase = text.match(
    /\bby\s+([A-Za-z]{3,9}\s+\d{1,2}(?:,\s*\d{4})?|tomorrow|eod|end of day|monday|tuesday|wednesday|thursday|friday)\b/i,
  );
  if (byPhrase) return collapseWhitespace(byPhrase[1]);

  return undefined;
}

function extractOwner(
  text: string,
  message: SlackMessageSummary,
  userNameById: Map<string, string>,
): string | undefined {
  const mention = text.match(/<@([A-Z0-9]+)>/);
  if (mention) {
    const mentionId = mention[1];
    return userNameById.get(mentionId) || `@${mentionId}`;
  }

  const plainAt = text.match(/\B@([a-zA-Z0-9._-]+)/);
  if (plainAt) return `@${plainAt[1]}`;

  if (/\b(i will|i'll|i can)\b/i.test(text)) {
    return message.user_name;
  }
  return undefined;
}

function confidenceForTask(
  text: string,
  owner?: string,
  dueDate?: string,
): TodoConfidence {
  const hasStrongCue = /\b(todo|action item|must|please|need to|follow up)\b/i.test(
    text,
  );
  if (hasStrongCue && (owner || dueDate)) return "high";
  if (hasStrongCue) return "medium";
  return owner || dueDate ? "medium" : "low";
}

function isActionableCandidate(text: string): boolean {
  if (!text || text.length < 6) return false;
  if (/^-?\s*\[[xX ]\]/.test(text)) return true;
  return /\b(todo|action|need to|needs to|please|must|should|follow up|ship|deliver|send|update|review)\b/i.test(
    text,
  );
}

function normalizeTaskText(text: string): string {
  let cleaned = text;
  cleaned = cleaned.replace(/^-?\s*\[[xX ]\]\s*/, "");
  cleaned = cleaned.replace(/^\s*(todo|action item|action)\s*[:\-]\s*/i, "");
  cleaned = cleaned.replace(/<@([A-Z0-9]+)>/g, "@$1");
  return collapseWhitespace(cleaned);
}

function splitCandidates(text: string): string[] {
  const lineLevel = text
    .split(/\n+/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  const candidates: string[] = [];
  lineLevel.forEach((line) => {
    line
      .split(/[.;]\s+/)
      .map((fragment) => fragment.trim())
      .filter((fragment) => fragment.length > 0)
      .forEach((fragment) => candidates.push(fragment));
  });
  return candidates;
}

function addOrMergeTodo(target: Map<string, TodoItem>, todo: TodoItem): void {
  const key = `${todo.task.toLowerCase()}|${(todo.owner || "").toLowerCase()}`;
  const existing = target.get(key);
  if (!existing) {
    target.set(key, todo);
    return;
  }

  const confidenceRank = (value: TodoConfidence): number => {
    if (value === "high") return 3;
    if (value === "medium") return 2;
    return 1;
  };

  if (confidenceRank(todo.confidence) > confidenceRank(existing.confidence)) {
    existing.confidence = todo.confidence;
  }
  if (!existing.dueDate && todo.dueDate) {
    existing.dueDate = todo.dueDate;
  }
  if (!existing.owner && todo.owner) {
    existing.owner = todo.owner;
  }
  todo.sources.forEach((source) => {
    if (!existing.sources.includes(source)) {
      existing.sources.push(source);
    }
  });
}

function extractTodosFromOutput(output: SlackOutput): TodoItem[] {
  const messages = output.messages ?? [];
  const users = output.users ?? [];
  const userNameById = buildUserNameById(users);
  const todoMap = new Map<string, TodoItem>();

  messages.forEach((message) => {
    const candidates = splitCandidates(message.text || "");
    candidates.forEach((candidate) => {
      if (!isActionableCandidate(candidate)) return;
      const task = normalizeTaskText(candidate);
      if (!task) return;
      const owner = extractOwner(candidate, message, userNameById);
      const dueDate = extractDueDate(candidate);
      const confidence = confidenceForTask(candidate, owner, dueDate);
      const source = message.permalink || message.source_ref;
      addOrMergeTodo(todoMap, {
        task,
        owner,
        dueDate,
        confidence,
        sources: source ? [source] : [],
      });
    });
  });

  const todos = Array.from(todoMap.values());
  todos.sort((left, right) => {
    const confidenceRank = (value: TodoConfidence): number => {
      if (value === "high") return 3;
      if (value === "medium") return 2;
      return 1;
    };
    return confidenceRank(right.confidence) - confidenceRank(left.confidence);
  });
  return todos.slice(0, 20);
}

function summarizeGaps(todos: TodoItem[]): string[] {
  let missingOwners = 0;
  let missingDueDates = 0;
  todos.forEach((todo) => {
    if (!todo.owner) missingOwners += 1;
    if (!todo.dueDate) missingDueDates += 1;
  });
  const gaps: string[] = [];
  if (missingOwners > 0) {
    gaps.push(`${missingOwners} task(s) do not have an explicit owner in Slack.`);
  }
  if (missingDueDates > 0) {
    gaps.push(`${missingDueDates} task(s) do not include a clear due date.`);
  }
  return gaps;
}

function scopeLabel(scope: SlackScope): string {
  if (scope.kind === "thread") {
    return `thread ${scope.channelId} @ ${scope.threadTs}`;
  }
  return `channel ${scope.channelId}`;
}

function renderTodoResponse(
  scope: SlackScope,
  output: SlackOutput,
  todos: TodoItem[],
): string {
  const messageCount = output.messages?.length ?? 0;
  const lines: string[] = [];
  lines.push(
    `Reviewed ${messageCount} message(s) from ${scopeLabel(scope)} in read-only mode.`,
  );

  if (todos.length === 0) {
    lines.push(
      "No concrete action items were confidently extractable from this scope.",
    );
    lines.push(
      "Try narrowing to a thread permalink or provide a tighter time range (oldest/latest).",
    );
    return lines.join("\n");
  }

  lines.push(`Action items (${todos.length}):`);
  todos.forEach((todo, index) => {
    lines.push(`${index + 1}. Task: ${todo.task}`);
    lines.push(`   Owner: ${todo.owner || "Unassigned"}`);
    lines.push(`   Due: ${todo.dueDate || "Not specified"}`);
    lines.push(`   Confidence: ${todo.confidence}`);
    lines.push(
      `   Sources: ${
        todo.sources.length > 0 ? todo.sources.join(", ") : "No source reference"
      }`,
    );
  });

  const gaps = summarizeGaps(todos);
  if (gaps.length > 0) {
    lines.push("Gaps:");
    gaps.forEach((gap) => lines.push(`- ${gap}`));
  }

  return lines.join("\n");
}

function clarifyingPrompt(): string {
  return (
    "I can extract todos from Slack, but I need scope first. " +
    "Provide a channel id (`C...`) or a thread permalink, and optionally `oldest=<ts>` / `latest=<ts>`."
  );
}

__chat_register({
  run: async (ctx) => {
    let userText = collapseWhitespace(ctx.text || extractText(ctx.message));

    // Clarification: if no Slack channel/thread scope can be parsed from the user's text,
    // ask once before opening any sessions. The scope regex is deterministic — no LLM needed.
    let scope = parseSlackScope(userText);
    if (!scope) {
      const reply = await ctx.emit.awaitInput(clarifyingPrompt());
      const clarifiedText = collapseWhitespace(messageText(reply));
      if (clarifiedText) userText = clarifiedText;
      scope = parseSlackScope(userText);
      if (!scope) {
        return {
          message:
            "Still no Slack channel or thread ID found. Provide a channel ID (e.g. C123ABC456) or a thread permalink.",
        };
      }
    }

    const executionSession = typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession("slack-" + Date.now().toString())
      : null;
    const intentId = "intent-slack-todo-extraction";
    const intentPhase = executionSession
      ? await executionSession.submitIntent({
          intentId,
          description: "Inspect scoped Slack conversation and extract actionable todos with evidence.",
          derivedFromMessageIds: [executionMessageId(ctx.message)],
        })
      : null;
    const executable = intentPhase
      ? await intentPhase.submitPlan({
          intentId,
          planId: "plan-slack-todo-extraction",
          steps: [
            {
              stepId: "step-parse-scope",
              description: "Parse Slack scope and retrieval bounds.",
              order: 0,
              dependsOn: [],
            },
            {
              stepId: "step-fetch-slack",
              description: "Fetch Slack messages from requested scope.",
              order: 1,
              dependsOn: ["step-parse-scope"],
            },
            {
              stepId: "step-extract-todos",
              description: "Extract action items and render response.",
              order: 2,
              dependsOn: ["step-fetch-slack"],
            },
          ],
        })
      : null;

    try {
      if (executable) {
        await executable.startStep?.("step-parse-scope", `Parsed scope: ${scopeLabel(scope)}`);
        await executable.completeStep?.("step-parse-scope", "Scope resolved from user input.");
        await executable.startStep?.("step-fetch-slack", `Fetching messages from ${scopeLabel(scope)}`);
      }

      const action = buildSlackAction(scope);
      const output = await runSlackAction(action);
      if (!output) {
        if (executable) await executable.finish?.();
        return { message: "Slack tool returned no data for the requested scope." };
      }

      if (executable) {
        await executable.completeStep?.("step-fetch-slack", `Retrieved ${output.messages?.length ?? 0} messages.`);
        await executable.startStep?.("step-extract-todos", "Extracting actionable todos from messages.");
      }

      const todos = extractTodosFromOutput(output);

      if (executable) {
        await executable.completeStep?.("step-extract-todos", `Extracted ${todos.length} action item(s).`);
        await executable.finish?.();
      }

      return { message: renderTodoResponse(scope, output, todos) };
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      try { if (executable) await executable.abort?.(errorMessage); } catch (_) { /* best-effort */ }
      return { error: `Slack todo extraction failed: ${errorMessage}` };
    }
  },
});

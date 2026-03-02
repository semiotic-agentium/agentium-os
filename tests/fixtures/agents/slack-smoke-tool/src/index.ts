type ChatPart = { text?: string };
type ChatMessage = { parts?: ChatPart[] };

type ToolSessionHandle = {
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
};

type SessionApi = {
  run(fn: () => Promise<{ message: string }> | { message: string }): Promise<void>;
};

declare function session(message: ChatMessage): SessionApi;
declare function __chat_register(args: {
  onChatMessage: (message: ChatMessage) => Promise<void>;
}): void;
declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<ToolSessionHandle>;

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function countConversations(output: unknown): number {
  if (!isObject(output)) return 0;
  const conversations = output.conversations;
  return Array.isArray(conversations) ? conversations.length : 0;
}

async function onChatMessage(message: ChatMessage): Promise<void> {
  const s = session(message);
  await s.run(async () => {
    let toolSession: ToolSessionHandle | null = null;
    try {
      toolSession = await openToolSession("support/slack");
      await toolSession.send({
        kinds: ["public_channel"],
        limit: 5,
      });
      const next = await toolSession.continue();
      await toolSession.finish();
      toolSession = null;
      const output = isObject(next) && isObject(next.output) ? next.output : next;
      const count = countConversations(output);
      return { message: `Slack smoke fixture fetched ${count} conversation(s).` };
    } catch (error) {
      if (toolSession) {
        try {
          await toolSession.abort(error instanceof Error ? error.message : String(error));
        } catch {
          // Ignore abort failure on error path.
        }
      }
      return {
        message: `Slack smoke fixture encountered an error: ${
          error instanceof Error ? error.message : String(error)
        }`,
      };
    }
  });
}

__chat_register({ onChatMessage });

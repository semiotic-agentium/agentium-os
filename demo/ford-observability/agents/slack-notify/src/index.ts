/// <reference path="./baml-runtime.d.ts" />
import type { RunContext, SessionResult } from "./baml-runtime";

type NotifyRequest = { text: string; context_id: string };

// Max session.continue() hops for the slack_notify tool call. One hop per
// streaming chunk from the tool; chat.postMessage is single-shot but the host
// may interleave status updates, so we keep a small but non-trivial budget.
// Hitting the budget aborts the session and surfaces "exceeded continue
// budget" in runner logs. See coordinator MAX_CONTINUE_HOPS for the longer
// rationale.
const MAX_CONTINUE_HOPS = 16;

function parseRequest(text: string): NotifyRequest {
  try {
    const raw = JSON.parse(text) as unknown;
    if (raw && typeof raw === "object") {
      const obj = raw as Record<string, unknown>;
      if (typeof obj.text === "string" && typeof obj.context_id === "string") {
        return { text: obj.text, context_id: obj.context_id };
      }
    }
  } catch {
    // Plain text fallback below.
  }
  return { text, context_id: "unknown-context" };
}

async function runSingleSendSession(
  toolName: string,
  openInput: Record<string, unknown>,
  sendInput: Record<string, unknown>,
): Promise<unknown> {
  let session = await openToolSession(toolName, openInput);
  try {
    await session.send(sendInput);
    for (let i = 0; i < MAX_CONTINUE_HOPS; i += 1) {
      const next = await session.continue();
      if (next && typeof next === "object") {
        const obj = next as Record<string, unknown>;
        const status = typeof obj.status === "string" ? obj.status.toLowerCase() : "";
        if (status === "streaming") continue;
        if (status === "error") throw new Error(JSON.stringify(obj.error ?? obj));
        await session.finish();
        session = null as unknown as typeof session;
        return "output" in obj ? obj.output : next;
      }
      await session.finish();
      session = null as unknown as typeof session;
      return next;
    }
    throw new Error(`${toolName} exceeded continue budget`);
  } catch (error) {
    if (session) {
      try { await session.abort(error instanceof Error ? error.message : String(error)); } catch {}
    }
    throw error;
  }
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const req = parseRequest(ctx.text || "");
    const compact = req.text.length > 3500 ? `${req.text.slice(0, 3500)}\n…` : req.text;
    ctx.emit.message(`Posting Slack incident summary for context_id=${req.context_id}`);
    const output = await runSingleSendSession("support/slack_notify", {}, {
      text: compact,
      context_id: req.context_id,
    });
    return { message: `Slack notification posted: ${JSON.stringify(output)}` };
  },
});

export {};

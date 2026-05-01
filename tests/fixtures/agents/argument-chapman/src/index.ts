/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: argument-chapman. Two-turn contradiction (Monty Python argument sketch).
 * Direct chat: turn 1 reply + awaitInput("Your next line?") → INPUT_REQUIRED; turn 2 (resume) → COMPLETED.
 * When invoked via system/internal_a2a (`referenceTaskIds` set), responds once and completes so the parent owns suspension.
 */
import type { SessionResult } from "./baml-runtime";

const FALLBACK = "Yes it is.";

/** True when this turn was invoked by `system/internal_a2a` from a parent agent (parent task id wired). */
function isDelegatedFromInternalA2a(message: unknown): boolean {
  if (message == null || typeof message !== "object") return false;
  const m = message as { referenceTaskIds?: unknown; reference_task_ids?: unknown };
  const ids = Array.isArray(m.referenceTaskIds)
    ? m.referenceTaskIds
    : Array.isArray(m.reference_task_ids)
      ? m.reference_task_ids
      : null;
  return ids !== null && ids.length > 0;
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = ctx.text || "Nothing.";
    try {
      // Delegated calls must finish in one stream: parent (Cleese) owns INPUT_REQUIRED.
      // Without this branch, awaitInput here suspends internal_a2a and the parent's step loop fails.
      if (isDelegatedFromInternalA2a(ctx.message)) {
        let reply: string;
        try {
          const result = await ArgumentReply({ other_message: text });
          reply = typeof result === "string" ? result : FALLBACK;
        } catch {
          reply = FALLBACK;
        }
        return { message: reply };
      }

      let reply: string;
      try {
        const result = await ArgumentReply({ other_message: text });
        reply = typeof result === "string" ? result : FALLBACK;
      } catch {
        reply = FALLBACK;
      }
      ctx.emit.message(reply);
      const nextMessage = await ctx.emit.awaitInput("Your next line?");
      const nextText = messageText(nextMessage) || "Nothing.";
      let secondReply: string;
      try {
        const result = await ArgumentReply({ other_message: nextText });
        secondReply = typeof result === "string" ? result : FALLBACK;
      } catch {
        secondReply = FALLBACK;
      }
      ctx.emit.message(secondReply);
      return { message: secondReply };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

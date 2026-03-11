/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: argument-chapman. Two-turn contradiction (Monty Python argument sketch).
 * Turn 1: reply with one line, then awaitInput("Your next line?") → INPUT_REQUIRED.
 * Turn 2 (resume): reply to the user's line, then COMPLETED.
 */
import type { SessionResult } from "./baml-runtime";

const FALLBACK = "Yes it is.";

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = ctx.text || "Nothing.";
    try {
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

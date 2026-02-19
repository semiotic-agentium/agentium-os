/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: argument-chapman. Two-turn contradiction (Monty Python argument sketch).
 * Turn 1: reply with one line, then awaitInput("Your next line?") → INPUT_REQUIRED.
 * Turn 2 (resume): reply to the user's line, then COMPLETED.
 */
import type { SessionResult } from "./baml-runtime";

const FALLBACK = "Yes it is.";
const TIMEOUT_MS = 1200;

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = ctx.text || "Nothing.";
    try {
      const reply = await Promise.race([
        ArgumentReply({ other_message: text }),
        new Promise<string>((resolve) => setTimeout(() => resolve(FALLBACK), TIMEOUT_MS)),
      ]);
      const line = typeof reply === "string" ? reply : FALLBACK;
      ctx.emit.message(line);
      const nextMessage = await ctx.emit.awaitInput("Your next line?");
      const nextText = messageText(nextMessage) || "Nothing.";
      const secondReply = await Promise.race([
        ArgumentReply({ other_message: nextText }),
        new Promise<string>((resolve) => setTimeout(() => resolve(FALLBACK), TIMEOUT_MS)),
      ]);
      const secondLine = typeof secondReply === "string" ? secondReply : FALLBACK;
      ctx.emit.message(secondLine);
      return { message: secondLine };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

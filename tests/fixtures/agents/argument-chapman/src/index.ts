/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: argument-chapman. Replies with one contradiction line (Monty Python argument sketch).
 * Uses __chat_register({ run }) DSL; run(ctx) returns SessionResult.
 */
import type { SessionResult } from "./baml-runtime";

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = ctx.text || "Nothing.";
    try {
      const reply = await Promise.race([
        ArgumentReply({ other_message: text }),
        new Promise<string>((resolve) => {
          setTimeout(() => resolve("Yes it is."), 1200);
        }),
      ]);
      const line = typeof reply === "string" ? reply : "Yes it is.";
      ctx.emit.message(line);
      // Explicit final chunk so stream completes (UntilFinalChunk); shim also emits on return.
      (globalThis as unknown as { __chat_yield?: (chunk: unknown) => void }).__chat_yield?.({
        task: { status: { state: "TASK_STATE_COMPLETED" } },
        final: true,
      });
      return { message: line };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

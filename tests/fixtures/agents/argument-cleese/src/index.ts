/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: argument-cleese. Starts the argument, sends one line to Chapman via internal_a2a, then uses INPUT_REQUIRED.
 * Turn 1: ArgumentReply → emit line → CleeseSendToChapman (via step executor) → emit Chapman reply → awaitInput("Say 'done' to finish.") → INPUT_REQUIRED.
 * Turn 2 (resume): return { message: "Done." } → COMPLETED.
 */
import type { SessionResult } from "./baml-runtime";

function emitFromStepExecutorResult(emit: { message: (text: string) => void }, steps: unknown[]): void {
  for (const step of steps) {
    const s = step as {
      status?: string;
      output?: {
        chunks?: Array<{ message?: { parts?: Array<{ text?: string }> } }>;
      };
    };
    // Look for chunks in Read step output (internal_a2a returns chunks with message.parts)
    const chunks = s.output?.chunks;
    if (Array.isArray(chunks)) {
      for (const chunk of chunks) {
        const text = chunk?.message?.parts?.[0]?.text;
        if (text != null) {
          emit.message(String(text));
        }
      }
    }
  }
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = ctx.text || "It certainly is not.";
    try {
      const firstLine = await ArgumentReply({ other_message: text });
      const myLine = typeof firstLine === "string" ? firstLine : "No it isn't.";
      ctx.emit.message(myLine);

      // Use step executor loop: Open → Send → Read → Finish
      const chapmanRun = await runGeneratedStepExecutor(
        "CleeseSendToChapman",
        { first_line: myLine },
        { max_steps: 6 }
      );
      emitFromStepExecutorResult(ctx.emit, chapmanRun.steps);

      await ctx.emit.awaitInput("Say 'done' to finish.");
      return { message: "Done." };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

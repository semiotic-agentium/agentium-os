/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: argument-cleese. Starts the argument, sends one line to Chapman via internal_a2a, then uses INPUT_REQUIRED.
 * Turn 1: ArgumentReply → emit line → CleeseSendToChapman → emit Chapman reply → awaitInput("Say 'done' to finish.") → INPUT_REQUIRED.
 * Turn 2 (resume): return { message: "Done." } → COMPLETED.
 */
import type { SessionResult } from "./baml-runtime";

function emitFromToolResult(emit: { message: (text: string) => void }, value: unknown): void {
  if (value == null) return;
  const emitPart = (c: { message?: { parts?: Array<{ text?: string }> } }) => {
    const t = c?.message?.parts?.[0]?.text;
    if (t != null) emit.message(String(t));
  };
  if (Array.isArray(value)) {
    for (const item of value) {
      const v = item as { chunks?: Array<{ message?: { parts?: Array<{ text?: string }> } }>; message?: { parts?: Array<{ text?: string }> } };
      if (Array.isArray(v.chunks)) for (const c of v.chunks) emitPart(c);
      else emitPart(v);
    }
    return;
  }
  const obj = value as { chunks?: Array<{ message?: { parts?: Array<{ text?: string }> } }>; message?: { parts?: Array<{ text?: string }> } };
  if (Array.isArray(obj.chunks)) for (const c of obj.chunks) emitPart(c);
  else emitPart(obj);
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = ctx.text || "Start the argument.";
    try {
      const firstLine = await ArgumentReply({ other_message: text });
      const myLine = typeof firstLine === "string" ? firstLine : "No it isn't.";
      ctx.emit.message(myLine);

      const chapmanResult = await CleeseSendToChapman({ first_line: myLine });
      emitFromToolResult(ctx.emit, chapmanResult);

      await ctx.emit.awaitInput("Say 'done' to finish.");
      return { message: "Done." };
    } catch (e) {
      const errMsg = e instanceof Error ? e.message : String(e);
      return { error: errMsg };
    }
  },
});

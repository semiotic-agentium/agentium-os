/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: conversational-context-auto
 * ------------------------------------
 * Provenance-backed automatic conversation context in BAML.
 *
 * What this demonstrates:
 * - Using __chat_register({ run }) so the entrypoint is run(ctx); no session/run boilerplate.
 * - ctx.text and ctx.message; BAML receives user_message for context.
 * - ChooseCalcTool for compute-like input; ChatWithContext for general chat.
 *
 * Flow: message matches compute pattern → ChooseCalcTool; else → ChatWithContext → COMPLETED.
 */

function shouldCompute(text: string): boolean {
  return /\d+\s*[\+\-\*\/]\s*\d+/.test(text) || text.toLowerCase().includes("compute");
}

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "unknown";
    if (shouldCompute(text)) {
      const toolResult = await ChooseCalcTool({ ...ctx.message, user_message: text });
      const result =
        (toolResult as any)?.output?.result ??
        (toolResult as any)?.result ??
        null;
      if (result != null) {
        return { message: `Computed result is ${result}. I will remember this conversation.` };
      }
      return { error: "BAML tool returned no output" };
    }
    const reply = await ChatWithContext({ ...ctx.message, user_message: text });
    return { message: String(reply) };
  },
});

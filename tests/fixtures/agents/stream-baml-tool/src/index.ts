/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: stream-baml-tool
 * -------------------------
 * Tests async streaming of a BAML tool (FSM) result driven by message.sendStream.
 *
 * What this demonstrates:
 * - Using __chat_register({ run }) so the entrypoint is run(ctx); ctx.text and ctx.message.
 * - Returning `{ message }` or `{ error }` from tool result.
 *
 * Flow: any message → ChooseCalcTool(user_message) → stream with sum=... → COMPLETED.
 */

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "unknown";
    // Emit a progress signal early so streaming consumers get a chunk even if the LLM is slow.
    ctx.emit.statusChanged("TASK_STATE_WORKING");
    const toolResult = await ChooseCalcTool({ ...ctx.message, user_message: text });
    if (toolResult != null && typeof toolResult === "object" && "result" in toolResult) {
      return { message: `BAML tool result: sum=${(toolResult as { result: number }).result}` };
    }
    return { error: "BAML tool returned no output" };
  },
});

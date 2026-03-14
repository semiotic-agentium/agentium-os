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
    // Use the step-executor loop: auto-Open → Send (with expression) → Read (get result) → Finish
    const run = await runGeneratedStepExecutor("ChooseCalcTool", { user_message: text }, { max_steps: 6 });
    // Find the Read step result that has the calculator output.
    // Done steps have the form { status: "done", output: { result, expression, formatted } }.
    for (const step of [...run.steps].reverse()) {
      const s = step as { status?: string; output?: { result?: number; expression?: string; formatted?: string }; result?: number; expression?: string; formatted?: string };
      // Accept nested output (session-plan Done step) or flat result (direct tool call).
      const result = s.output?.result ?? s.result;
      if (typeof result === "number") {
        return { message: `BAML tool result: sum=${result}` };
      }
    }
    return { error: "BAML tool returned no output" };
  },
});

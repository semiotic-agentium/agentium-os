/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: stream-baml-tool
 * -------------------------
 * Tests async streaming of a BAML tool (FSM) result driven by message.sendStream.
 *
 * Flow: any message → ChooseCalcTool(user_message) → Open → Send (blocking) → Finish.
 * The blocking Send result carries `result: { result, expression, formatted }`.
 */

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "unknown";
    const run = await runGeneratedStepExecutor("ChooseCalcTool", { user_message: text }, { max_steps: 6 });

    // Blocking Send result: { status:"done", output:"@1 header", archive_ref:"@1", result:{result,expression,...} }
    // Scan steps in reverse for the first step that carries a numeric `result.result`.
    for (const step of [...run.steps].reverse()) {
      const s = step as unknown as Record<string, unknown>;
      // New path: blocking Send puts raw tool JSON in `result`
      const rawResult = s.result as Record<string, unknown> | undefined;
      if (rawResult && typeof rawResult.result === "number") {
        return { message: `BAML tool result: sum=${rawResult.result}` };
      }
      // Legacy path: output object with direct result field
      const out = s.output as Record<string, unknown> | undefined;
      if (out && typeof out.result === "number") {
        return { message: `BAML tool result: sum=${out.result}` };
      }
    }

    // Fallback: check run.last directly
    const last = run.last as unknown as Record<string, unknown> | null;
    if (last) {
      const rawResult = last.result as Record<string, unknown> | undefined;
      if (rawResult && typeof rawResult.result === "number") {
        return { message: `BAML tool result: sum=${rawResult.result}` };
      }
    }

    return { error: "BAML tool returned no output" };
  },
});

export {};

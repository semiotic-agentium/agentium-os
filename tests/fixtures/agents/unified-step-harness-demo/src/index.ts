/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: unified-step-harness-demo — exercises `UnifiedHarnessPick` via `runGeneratedStepExecutor`.
 */

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "";
    const run = await runGeneratedStepExecutor(
      "UnifiedHarnessPick",
      { user_message: text },
      { max_steps: 8 },
    );
    const last = run.last as unknown as Record<string, unknown>;
    const peeled = last.output ?? last;
    return { message: JSON.stringify(peeled) };
  },
});

export {};

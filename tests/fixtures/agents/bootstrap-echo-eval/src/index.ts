/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: bootstrap-echo-eval
 * Deterministic eval agent for bootstrap → publish → deploy → A2A verification.
 * Returns `eval:pass:<user text>` without calling BAML/LLM.
 */

__chat_register({
  run: async (ctx) => {
    const text = (ctx.text || "").trim();
    return { message: `eval:pass:${text || "ping"}` };
  },
});

export {};

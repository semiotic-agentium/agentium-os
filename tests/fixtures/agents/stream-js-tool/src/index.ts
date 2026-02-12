/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: stream-js-tool
 * -----------------------
 * Didactic reference for the A2A DSL when no BAML tools are used.
 *
 * What this demonstrates:
 * - Using __chat_register({ run }) so the entrypoint is run(ctx); ctx.emit for progress/artifact.
 * - Returning `{ message }` for terminal COMPLETED; no awaitInput or branching.
 *
 * Trigger: message text containing "stream-task".
 */

const TRIGGER = "stream-task";

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "unknown";
    if (!text.includes(TRIGGER)) {
      return { message: `Unknown or no trigger: ${text}` };
    }
    ctx.emit.artifact(
      {
        name: "Artifact",
        description: "Fixture artifact",
        parts: [{ mediaType: "application/json", data: { done: true } }],
      },
      false,
      true
    );
    ctx.emit.message(`Complete: ${text}`);
    return { message: `Complete: ${text}` };
  },
});

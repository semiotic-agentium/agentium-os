/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: conversational-persona-demo
 * ------------------------------------
 * Simple conversational BAML persona agent with no tools.
 *
 * What this demonstrates:
 * - Using __chat_register({ run }) so the entrypoint is run(ctx); ctx.text and ctx.message.
 * - Return `{ message }` from PersonaChat reply.
 *
 * Flow: any message → PersonaChat(user_message) → COMPLETED.
 */

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "unknown";
    const reply = await PersonaChat({ ...ctx.message, user_message: text });
    return { message: String(reply) };
  },
});

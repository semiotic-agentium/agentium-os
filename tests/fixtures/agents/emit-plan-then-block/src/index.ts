/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: emit-plan-then-block
 * -----------------------------
 * Reproduces the "plan chunks not reaching client" scenario:
 * - Emits multiple chunks (plan, status) with NO yield between them
 * - Then blocks the event loop (simulates long BAML/LLM call)
 * - Then returns
 *
 * Without a concurrent drain or emit yield, the plan chunks sit in the yield
 * buffer until the advance returns (after the block). This fixture exists so
 * A2A stream tests can assert that plan chunks are delivered to the client.
 *
 * Trigger: message text containing "plan-then-block".
 */
const TRIGGER = "plan-then-block";

const PLAN_MARKER = "--- Plan ---";
const PLAN_SPEC = "Spec: do X. Validation: Y.";
const STARTING_MSG = "Starting development.";

__chat_register({
  run: async (ctx) => {
    const text = ctx.text || "";
    if (!text.includes(TRIGGER)) {
      return { message: `No trigger. Send a message containing "${TRIGGER}".` };
    }

    // Emit plan chunks (no yield between these and the block below)
    ctx.emit.message(`${PLAN_MARKER}\n${PLAN_SPEC}\n---`);
    ctx.emit.message(STARTING_MSG);
    ctx.emit.statusChanged("TASK_STATE_WORKING");

    // Block the event loop ~100ms to simulate a long BAML call.
    // Without concurrent drain or emit yield, the collector cannot drain
    // until advance returns (after this loop).
    const start = Date.now();
    while (Date.now() - start < 100) {
      // busy loop
    }

    return { message: "Done" };
  },
});

/// <reference path="./baml-runtime.d.ts" />

declare function openToolSession(
  toolName: string,
  openInput?: Record<string, unknown>,
): Promise<{
  send(args: Record<string, unknown>): Promise<unknown>;
  continue(readInput?: Record<string, unknown>): Promise<unknown>;
  finish(): Promise<unknown>;
  abort(reason?: string): Promise<unknown>;
}>;

__chat_register({
  run: async (ctx) => {
    const objective =
      (ctx.text || "").trim() ||
      "Find non_trivial_scope_cache_goal using efficient session reads.";

    let handle: Awaited<ReturnType<typeof openToolSession>> | null = null;

    try {
      for (let hop = 0; hop < 24; hop++) {
        // LLM derives current state from ctx.tags['conversation_history'] — no op list passed.
        const plan = await PlanSessionToolEvalStep({ objective });
        const step = plan.plan_steps?.[0];
        if (!step) return { error: "planner returned no plan_steps[0]" };

        let decision: { op?: string; initial_input?: unknown; input?: unknown };
        try {
          decision = JSON.parse(step.sub_message) as typeof decision;
        } catch {
          return { error: "planner sub_message is not valid JSON" };
        }
        const op = decision?.op;

        if (!op) return { error: "planner JSON missing op" };

        if (op === "Open") {
          const openInput =
            decision.initial_input && typeof decision.initial_input === "object"
              ? (decision.initial_input as Record<string, unknown>)
              : { reason: "session-tool-eval" };
          handle = await openToolSession("test/synthetic_session_eval", openInput);
          continue;
        }

        if (!handle) return { error: "session handle missing — Open must come first" };

        if (op === "Send") {
          const input =
            decision.input && typeof decision.input === "object"
              ? (decision.input as Record<string, unknown>)
              : {};
          await handle.send(input);
          continue;
        }

        if (op === "Read") {
          const readInput =
            decision.input && typeof decision.input === "object"
              ? (decision.input as Record<string, unknown>)
              : {};
          const result = await handle.continue(readInput);
          // Result is recorded in conversation_history automatically.
          // Check termination condition from the raw output.
          const out = result as Record<string, unknown> | null;
          const goalId =
            out && typeof out === "object" && typeof out.goal_id === "string"
              ? out.goal_id
              : null;
          if (goalId === "non_trivial_scope_cache_goal") {
            // Goal reached — next hop the LLM will see it in history and emit Finish.
          }
          continue;
        }

        if (op === "Finish") {
          await handle.finish();
          return { message: "session-tool-eval finished" };
        }

        return { error: `unrecognised op '${op}'` };
      }

      return { error: "session-tool-eval exceeded max hops" };
    } catch (err) {
      if (handle) {
        try { await handle.abort(err instanceof Error ? err.message : String(err)); } catch { /* ignore */ }
      }
      return { error: err instanceof Error ? err.message : String(err) };
    }
  },
});

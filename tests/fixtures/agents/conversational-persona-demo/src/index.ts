/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: conversational-persona-demo
 * -------------------------------------
 * ReAct coordinator — dynamic behaviour is entirely in the BAML prompts.
 * TypeScript orchestrates provenance lifecycle and sequences FSM loops.
 * No data extraction, no thunking. History carries context between steps.
 *
 * Flow:
 *   1. InferDiscoveryIntent  — LLM distils user intent (single-shot)
 *   2. GetDiscoverAgentsPlan — FSM: Open→Send→Read→Finish (archives agents @N)
 *   3. MakeStructuredPlan    — LLM plans from history; returns plan_steps or []
 *   4a. FormatCapabilities   — if no steps: in-persona capability summary
 *   4b. DecideDelegationAction × N — FSM: Open→Send→Read→null per delegate
 *   5. PersonaReact          — LLM synthesizes all results from history
 */
import type { SessionResult, StandardStructuredPlan } from "./baml-runtime";

declare function openA2aExecutionSession(token: string): Promise<{
  submitIntent: (args: {
    intentId: string;
    description: string;
    derivedFromMessageIds: string[];
  }) => Promise<{
    submitPlan: (args: {
      intentId: string;
      planId: string;
      steps: Array<{ stepId: string; description: string; order: number; dependsOn: string[] }>;
    }) => Promise<{
      startStep: (stepId: string, text: string) => Promise<unknown>;
      completeStep: (stepId: string, text: string) => Promise<unknown>;
      abort: (reason: string) => Promise<unknown>;
      finish: () => Promise<unknown>;
    }>;
  }>;
}>;

function slug(text: string, fallback: string): string {
  const s = text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48);
  return s || fallback;
}

function messageId(ctx: { message?: unknown }): string {
  const m = ctx.message;
  if (m && typeof m === "object") {
    const id = (m as Record<string, unknown>).messageId ?? (m as Record<string, unknown>).id;
    if (typeof id === "string" && id.trim()) return id;
  }
  return "msg-persona-fallback";
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    const text = (ctx.text ?? "").trim() || "unknown";
    const msgId = messageId(ctx);

    try {
      // ── Phase 1: infer discovery intent (single-shot, goal-level) ───────────
      const intentText = String(await InferDiscoveryIntent({ user_message: text })).trim()
        || "Identify agents relevant to the user intent.";

      // ── Phase 2: discover agents (own provenance session) ───────────────────
      const discoverySession = typeof openA2aExecutionSession === "function"
        ? await openA2aExecutionSession("persona-discover-" + Date.now())
        : null;
      const discoveryIntentPhase = discoverySession
        ? await discoverySession.submitIntent({
            intentId: "intent-" + slug(intentText, "persona"),
            description: intentText,
            derivedFromMessageIds: [msgId],
          })
        : null;
      const discoveryExecutable = discoveryIntentPhase
        ? await discoveryIntentPhase.submitPlan({
            intentId: "intent-" + slug(intentText, "persona"),
            planId: "plan-discover-" + slug(intentText, "persona"),
            steps: [{ stepId: "step-discover", description: "Discover available agents.", order: 0, dependsOn: [] }],
          })
        : null;

      await discoveryExecutable?.startStep("step-discover", "Discovering agents.");
      await runGeneratedStepExecutor("GetDiscoverAgentsPlan", { inferred_intent: intentText }, { max_steps: 8 });
      await discoveryExecutable?.completeStep("step-discover", "Agents discovered.");
      await discoveryExecutable?.finish();

      // ── Phase 3: plan from history (own provenance session) ─────────────────
      const plan = await MakeStructuredPlan({ user_message: text }) as StandardStructuredPlan;
      const steps = Array.isArray(plan?.plan_steps) ? plan.plan_steps : [];
      const objective = String(plan?.objective ?? text);
      const planIntentId = "intent-" + slug(plan?.intent_description ?? objective, "plan");
      const planId = "plan-" + slug(objective, "plan");

      const session = typeof openA2aExecutionSession === "function"
        ? await openA2aExecutionSession("persona-plan-" + Date.now())
        : null;

      const planIntentPhase = session
        ? await session.submitIntent({
            intentId: planIntentId,
            description: String(plan?.intent_description ?? objective),
            derivedFromMessageIds: [msgId],
          })
        : null;

      // ── Phase 4a: capability query (no delegation) ───────────────────────────
      if (steps.length === 0) {
        const capExecutable = planIntentPhase
          ? await planIntentPhase.submitPlan({
              intentId: planIntentId,
              planId: planId + "-caps",
              steps: [{ stepId: "step-caps", description: "Summarise agent capabilities.", order: 0, dependsOn: [] }],
            })
          : null;
        await capExecutable?.startStep("step-caps", "Formatting capabilities.");
        const message = String(await FormatCapabilities({ user_message: text })).trim();
        await capExecutable?.completeStep("step-caps", "Capabilities formatted.");
        await capExecutable?.finish();
        return { message: message.slice(0, 1800) };
      }

      // ── Phase 4b: delegate to specialist agents ──────────────────────────────
      const delegateExecutable = planIntentPhase
        ? await planIntentPhase.submitPlan({
            intentId: planIntentId,
            planId,
            steps: steps.map((s, i) => ({
              stepId: "step-" + i,
              description: String(s.sub_message ?? s.agent_package ?? "delegate"),
              order: i,
              dependsOn: i === 0 ? [] : ["step-" + (i - 1)],
            })),
          })
        : null;

      for (let i = 0; i < steps.length; i++) {
        const step = steps[i];
        const stepId = "step-" + i;
        const pkg = String(step.agent_package ?? "");
        const inst = String(step.agent_instance_id ?? "default");
        const goal = String(step.sub_message ?? objective);
        const agent = pkg + "/" + inst;

        if (!pkg) {
          await delegateExecutable?.completeStep(stepId, "Skipped — no agent target.");
          continue;
        }

        await delegateExecutable?.startStep(stepId, "Delegating to " + agent + ".");
        await runGeneratedStepExecutor("DecideDelegationAction", { goal, agent }, { max_steps: 10 });
        await delegateExecutable?.completeStep(stepId, "Delegation complete.");
      }

      // ── Phase 5: synthesize in persona voice from history ────────────────────
      const message = String(await PersonaReact({ user_message: text, plan_objective: objective })).trim();
      await delegateExecutable?.finish();
      return { message: message.slice(0, 1800) };

    } catch (e) {
      return { error: e instanceof Error ? e.message : String(e) };
    }
  },
});

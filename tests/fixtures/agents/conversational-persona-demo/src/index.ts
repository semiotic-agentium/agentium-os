/// <reference path="./baml-runtime.d.ts" />
/**
 * Fixture: conversational-persona-demo
 * -------------------------------------
 * ReAct coordinator — dynamic behaviour is entirely in the BAML prompts.
 * TypeScript orchestrates provenance lifecycle and sequences FSM loops.
 * No data extraction, no thunking. History carries context between steps.
 *
 * Flow:
 *   1. ClassifyPersonaCoordinatorTurn — Ready (delegatable task) | NeedTaskClarification | MetaOnly
 *      Loop with awaitInput until Ready (with delegatable inferred_intent), MetaOnly, or clarified.
 *   2. GetDiscoverAgentsPlan — **only if** step 1 chose a delegatable task (skip for greetings/meta).
 *   3. MakeStructuredPlan    — LLM plans from history; returns plan_steps or []
 *   4a. FormatCapabilities   — if no steps: in-persona capability summary (StructuredReply)
 *   4b. DecideDelegationAction × N — FSM: Open→Send→Read→null per delegate
 *   5. PersonaReact          — LLM synthesizes all results from history
 */
import type {
  SessionResult,
  StandardStructuredPlan,
  StructuredReply,
} from "./baml-runtime";

function slug(text: string, fallback: string): string {
  const s = text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48);
  return s || fallback;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

/** BAML union PersonaReadyIntent — concrete delegatable task for discovery. */
function isPersonaReadyIntent(v: unknown): v is { inferred_intent: string } {
  return (
    isObject(v) &&
    typeof v.inferred_intent === "string" &&
    v.inferred_intent.trim().length > 0
  );
}

/** BAML union PersonaMetaOnly — greetings / capabilities / chat with no work request. */
function isPersonaMetaOnly(v: unknown): v is { reason: string } {
  return (
    isObject(v) &&
    typeof v.reason === "string" &&
    typeof v.inferred_intent !== "string" &&
    typeof v.question !== "string"
  );
}

/**
 * Model-only Ready labels still need a gate: do not treat greetings or generic discovery prose as work.
 */
function looksDelegatableIntent(raw: string): boolean {
  const s = raw.trim();
  if (s.length < 12) return false;
  if (/^(hi|hello|hey|thanks|thank you|good morning|good afternoon)\b/i.test(s)) return false;
  if (/^(identify|list|find|discover)\s+(available\s+)?agents?\b/i.test(s)) return false;
  if (/list and orient\b/i.test(s)) return false;
  if (/\b(capabilities only|what can you do|who are you)\b/i.test(s)) return false;
  return true;
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    let text = (ctx.text ?? "").trim() || "unknown";

    let runDiscovery = false;
    let intentText = "";

    while (true) {
      const turn = await ClassifyPersonaCoordinatorTurn({ user_message: text });
      if (isObject(turn) && typeof turn.question === "string" && turn.question.trim().length > 0) {
        const reply = await ctx.emit.awaitInput(turn.question.trim());
        const next = messageText(reply).trim();
        if (next) text = next;
        continue;
      }
      if (isPersonaReadyIntent(turn)) {
        const cand = turn.inferred_intent.trim();
        if (looksDelegatableIntent(cand)) {
          intentText = cand;
          runDiscovery = true;
          break;
        }
        const reply = await ctx.emit.awaitInput(
          "That still doesn't sound like a concrete outcome I can route to a specialist. What system, artifact, or deliverable should we target?",
        );
        const next = messageText(reply).trim();
        if (next) text = next;
        continue;
      }
      if (isPersonaMetaOnly(turn)) {
        runDiscovery = false;
        break;
      }
      const reply = await ctx.emit.awaitInput(
        "Say what you want done (scoped task), or ask what coordination can do — I won't search specialists until there's an actual goal.",
      );
      const next = messageText(reply).trim();
      if (next) text = next;
      continue;
    }

    let discoveryError: string | null = null;
    if (runDiscovery) {
      try {
        const discoverySession = typeof openA2aExecutionSession === "function"
          ? await openA2aExecutionSession("persona-discover-" + Date.now())
          : null;
        const discoveryIntentPhase = discoverySession
          ? await discoverySession.submitIntent({
              intentId: "intent-" + slug(intentText, "persona"),
              description: intentText,
            })
          : null;
        const discoveryExecutable = discoveryIntentPhase
          ? await discoveryIntentPhase.submitPlan({
              intentId: "intent-" + slug(intentText, "persona"),
              planId: "plan-discover-" + slug(intentText, "persona"),
              steps: [{ stepId: "step-discover", description: "Discover available agents.", order: 0, dependsOn: [] }],
            })
          : null;

        await discoveryExecutable?.startStep("step-discover");
        await runGeneratedStepExecutor("GetDiscoverAgentsPlan", { inferred_intent: intentText }, { max_steps: 8 });
        await discoveryExecutable?.completeStep("step-discover");
        await discoveryExecutable?.finish();
      } catch (e) {
        discoveryError = e instanceof Error ? e.message : String(e);
        ctx.emit.message(`[discovery failed: ${discoveryError}]`);
      }
    }

    try {
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
          })
        : null;

      if (steps.length === 0) {
        const capExecutable = planIntentPhase
          ? await planIntentPhase.submitPlan({
              intentId: planIntentId,
              planId: planId + "-caps",
              steps: [{ stepId: "step-caps", description: "Summarise agent capabilities.", order: 0, dependsOn: [] }],
            })
          : null;
        await capExecutable?.startStep("step-caps");
        const cap = (await FormatCapabilities({ user_message: text })) as StructuredReply;
        await capExecutable?.completeStep("step-caps");
        await capExecutable?.finish();
        return { message: cap };
      }

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
        const agent_package = String(step.agent_package ?? "");
        const agent_instance_id = String(step.agent_instance_id ?? "default");
        const goal = String(step.sub_message ?? objective);

        if (!agent_package) {
          await delegateExecutable?.completeStep(stepId);
          continue;
        }

        await delegateExecutable?.startStep(stepId);
        await runGeneratedStepExecutor("DecideDelegationAction", { goal, agent_package, agent_instance_id }, { max_steps: 10 });
        await delegateExecutable?.completeStep(stepId);
      }

      const reacted = (await PersonaReact({
        user_message: text,
        plan_objective: objective,
      })) as StructuredReply;
      await delegateExecutable?.finish();
      return { message: reacted };

    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      ctx.emit.message(`[execution error: ${msg}]`);
      return { error: msg };
    }
  },
});

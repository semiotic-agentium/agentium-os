/// <reference path="./baml-runtime.d.ts" />
/**
 * Coordinator agent — intent classification, discovery, structured planning, delegation, synthesis.
 * Behaviour is primarily defined in BAML (`baml_src/coordinator_prompt.baml`).
 * TypeScript sequences execution-session steps and FSM loops; conversation history carries context.
 *
 * Flow:
 *   1. ClassifyCoordinatorTurn — tagged union: ready | clarify | meta
 *      Loop with awaitInput until Ready (with delegatable inferred_intent), MetaOnly, or clarified.
 *   2. GetDiscoverAgentsPlan — **only if** step 1 chose a delegatable task (skip for greetings/meta).
 *   3. MakeStructuredPlan — LLM plans from history; returns plan_steps or []
 *   4a. FormatCapabilities — if no steps: capability summary (StructuredReply)
 *   4b. DecideDelegationAction × N — FSM: Open→Send→Read→null per delegate
 *   5. CoordinatorSynthesizeReply — LLM synthesizes all results from history
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

type CoordinatorTurn =
  | { kind: "ready"; inferred_intent: string }
  | { kind: "clarify"; question: string }
  | { kind: "meta"; reason: string };

function parseCoordinatorTurn(v: unknown): CoordinatorTurn | null {
  if (!isObject(v) || typeof v.kind !== "string") return null;
  switch (v.kind) {
    case "ready":
      return typeof v.inferred_intent === "string" && v.inferred_intent.trim().length > 0
        ? { kind: "ready", inferred_intent: v.inferred_intent }
        : null;
    case "clarify":
      return typeof v.question === "string" && v.question.trim().length > 0
        ? { kind: "clarify", question: v.question }
        : null;
    case "meta":
      return typeof v.reason === "string"
        ? { kind: "meta", reason: v.reason }
        : null;
    default:
      return null;
  }
}

/**
 * Minimal defensive rail: reject empty or obviously greeting-only Ready strings.
 */
function isUsableReadyIntent(raw: string): boolean {
  const s = raw.trim();
  if (s.length < 8) return false;
  if (/^(hi|hello|hey|thanks|thank you)\b/i.test(s)) return false;
  return true;
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    let text = (ctx.text ?? "").trim() || "unknown";

    let runDiscovery = false;
    let intentText = "";

    while (true) {
      const rawTurn = await ClassifyCoordinatorTurn({ user_message: text });
      const turn = parseCoordinatorTurn(rawTurn);
      if (turn?.kind === "clarify") {
        const reply = await ctx.emit.awaitInput(turn.question.trim());
        const next = messageText(reply).trim();
        if (next) text = next;
        continue;
      }
      if (turn?.kind === "ready") {
        const cand = turn.inferred_intent.trim();
        if (isUsableReadyIntent(cand)) {
          intentText = cand;
          runDiscovery = true;
          break;
        }
        const reply = await ctx.emit.awaitInput(
          "What concrete outcome should I route to a specialist?",
        );
        const next = messageText(reply).trim();
        if (next) text = next;
        continue;
      }
      if (turn?.kind === "meta") {
        runDiscovery = false;
        break;
      }
      const reply = await ctx.emit.awaitInput(
        "Say what you want done (a scoped task), or ask what coordination can do — I won't search specialists until there's an actual goal.",
      );
      const next = messageText(reply).trim();
      if (next) text = next;
      continue;
    }

    let discoveryError: string | null = null;
    if (runDiscovery) {
      try {
        const discoverySession = typeof openA2aExecutionSession === "function"
          ? await openA2aExecutionSession("coordinator-discover-" + Date.now())
          : null;
        const discoveryIntentPhase = discoverySession
          ? await discoverySession.submitIntent({
              intentId: "intent-" + slug(intentText, "coord"),
              description: intentText,
            })
          : null;
        const discoveryExecutable = discoveryIntentPhase
          ? await discoveryIntentPhase.submitPlan({
              intentId: "intent-" + slug(intentText, "coord"),
              planId: "plan-discover-" + slug(intentText, "coord"),
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
        ? await openA2aExecutionSession("coordinator-plan-" + Date.now())
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

      const reacted = (await CoordinatorSynthesizeReply({
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

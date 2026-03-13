/// <reference path="./baml-runtime.d.ts" />
import type { RunContext, SessionResult } from "./baml-runtime";

const MAX_REACT_STEPS = 8;
const MAX_CLARIFY = 2;

type NeedClarification = { question: string };
type NotRelevant = { reason: string };
type ClickUpIntent = { intent: string; operation_kind: "read" | "write" | "delete" };
type ClickUpPlanStep = { id: string; description: string; kind: "navigate" | "execute" | "format" };
type ClickUpPlan = { goal: string; steps: ClickUpPlanStep[] };
type FinalResponse = { message: string };
type ClickUpTask = { id?: string; name?: string; status?: string; url?: string };
type ClickUpItem = { id: string; name: string; kind: string };
type ClickUpOutput = { tasks?: ClickUpTask[]; items?: ClickUpItem[]; message?: string };

function isObject(v: unknown): v is Record<string, unknown> {
  return v != null && typeof v === "object";
}

function executionMessageId(message: unknown): string {
  if (isObject(message)) {
    if (typeof message.messageId === "string" && message.messageId.trim().length > 0) return message.messageId;
    if (typeof message.id === "string" && message.id.trim().length > 0) return message.id;
  }
  return "msg-clickup-fallback";
}

function slugGoal(goal: string): string {
  return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
}

function isNeedClarification(v: unknown): v is NeedClarification {
  return isObject(v) && typeof v.question === "string" && v.question.trim().length > 0
    && !("message" in v) && !("intent" in v) && !("reason" in v) && !("steps" in v);
}

function isNotRelevant(v: unknown): v is NotRelevant {
  return isObject(v) && typeof v.reason === "string" && !("question" in v) && !("intent" in v);
}

function isClickUpIntent(v: unknown): v is ClickUpIntent {
  return isObject(v) && typeof v.intent === "string" && v.intent.trim().length > 0
    && typeof v.operation_kind === "string" && !("question" in v) && !("reason" in v);
}

function isClickUpPlan(v: unknown): v is ClickUpPlan {
  return isObject(v) && typeof v.goal === "string" && Array.isArray(v.steps);
}

function isFinalResponse(v: unknown): v is FinalResponse {
  if (!isObject(v)) return false;
  if (typeof v.message !== "string") return false;
  return !("tasks" in v || "items" in v || "steps" in v || "action" in v || "intent" in v);
}

function isToolOutput(v: unknown): v is ClickUpOutput {
  if (!isObject(v)) return false;
  return Array.isArray(v.tasks) || Array.isArray(v.items);
}

function extractToolOutput(v: unknown): ClickUpOutput | null {
  if (isToolOutput(v)) return v;
  if (isObject(v) && isToolOutput(v.output)) return v.output;
  return null;
}

function collectStepResultsJson(steps: unknown[]): string {
  const outputs: unknown[] = [];
  for (const step of steps) {
    const out = extractToolOutput(step);
    if (out) outputs.push(out);
    if (isFinalResponse(step)) outputs.push({ message: step.message });
  }
  try {
    return JSON.stringify(outputs.length > 0 ? outputs : steps.slice(-3), null, 2).slice(0, 6000);
  } catch (_) {
    return "{}";
  }
}

function formatOutput(output: ClickUpOutput): string {
  let response = output.message || "Done.";
  if (output.items && output.items.length > 0) {
    response += "\n\n" + output.items.map((i) => `• [${i.kind}] ${i.name} (id: ${i.id})`).join("\n");
  }
  if (output.tasks && output.tasks.length > 0) {
    response += "\n\n" + output.tasks
      .map((t) => `• ${t.name || "Unnamed task"} [${t.status || "unknown"}]${t.url ? ` — ${t.url}` : ""}`)
      .join("\n");
  }
  return response;
}

function extractFinalMessage(steps: unknown[]): string {
  for (const step of [...steps].reverse()) {
    if (isFinalResponse(step)) return step.message;
    const out = extractToolOutput(step);
    if (out) return formatOutput(out);
    if (isObject(step) && typeof step.message === "string") return step.message;
  }
  return "ClickUp returned no usable response for this request.";
}

/** Execute a resolved plan: open session, run per-step executors, return final message. */
async function runClickUpPlan(
  ctx: RunContext,
  plan: ClickUpPlan,
  operationKind: string,
): Promise<SessionResult> {
  const { goal, steps } = plan;

  const executionSession = typeof openA2aExecutionSession === "function"
    ? await openA2aExecutionSession("clickup-" + Date.now().toString())
    : null;
  const intentId = "intent-clickup-" + slugGoal(goal);
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: goal,
        derivedFromMessageIds: [executionMessageId(ctx.message)],
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: "plan-clickup-" + slugGoal(goal),
        steps: steps.map((s, i) => ({
          stepId: s.id,
          description: s.description,
          order: i,
          dependsOn: i > 0 ? [steps[i - 1]!.id] : [],
        })),
      })
    : null;

  // Execute each plan step independently, threading prior results forward.
  const toolSteps = steps.filter((s) => s.kind !== "format");
  const formatStep = steps.find((s) => s.kind === "format");
  const allStepOutputs: unknown[][] = [];
  let priorResultsJson: string | null = null;

  try {
    for (const toolStep of toolSteps) {
      if (executable) {
        await executable.startStep?.(toolStep.id, `Starting ${toolStep.kind}: ${toolStep.description}`);
      }

      const run = await runGeneratedStepExecutor("ChooseClickUpAction", {
        goal,
        step_description: toolStep.description,
        operation_kind: operationKind,
        prior_results: priorResultsJson,
      }, { max_steps: MAX_REACT_STEPS });

      allStepOutputs.push(run.steps);
      priorResultsJson = collectStepResultsJson(run.steps);

      if (executable) {
        await executable.completeStep?.(
          toolStep.id,
          `Completed ${toolStep.kind}: ${run.steps.length} result(s).`,
        );
      }
    }

    // Format step: extract final message from all accumulated step outputs.
    if (formatStep && executable) {
      await executable.startStep?.(formatStep.id, `Formatting response for: ${goal}`);
    }

    const finalMessage = extractFinalMessage(allStepOutputs.flat());

    if (formatStep && executable) {
      await executable.completeStep?.(formatStep.id, "Response formatted and returned to user.");
    }
    if (executable) await executable.finish?.();

    return { message: finalMessage };
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    try { if (executable) await executable.abort?.(errMsg); } catch (_) { /* best-effort */ }
    return { error: `ClickUp agent error: ${errMsg}` };
  }
}

__chat_register({
  run: async (ctx) => {
    const originalText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    let text = originalText;

    // ── Phase 1: Intent inference ────────────────────────────────────────────
    // InferClickUpIntent classifies whether this is a valid ClickUp request,
    // distills the intent, asks for clarification, or rejects non-ClickUp messages.
    let resolvedIntent: ClickUpIntent | null = null;
    for (let i = 0; i <= MAX_CLARIFY; i++) {
      const intentResult = await InferClickUpIntent({ user_message: text });

      if (isClickUpIntent(intentResult)) {
        resolvedIntent = intentResult;
        break;
      }
      if (isNotRelevant(intentResult)) {
        return { message: `This doesn't look like a ClickUp request — ${intentResult.reason}` };
      }
      if (isNeedClarification(intentResult) && i < MAX_CLARIFY) {
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = clarifiedText;
      } else {
        // Exhausted clarification rounds — treat message as-is.
        resolvedIntent = { intent: text, operation_kind: "read" };
        break;
      }
    }
    if (!resolvedIntent) return { error: "Could not determine a valid ClickUp intent." };

    // ── Phase 2: Planning ────────────────────────────────────────────────────
    // PlanClickUpWork derives an explicit step plan from the validated intent.
    const planResult = await PlanClickUpWork({
      intent: resolvedIntent.intent,
      operation_kind: resolvedIntent.operation_kind,
    });
    const plan: ClickUpPlan = isClickUpPlan(planResult) ? planResult : {
      goal: resolvedIntent.intent,
      steps: [
        { id: "step-navigate", description: "Navigate workspace to find required IDs.", kind: "navigate" },
        { id: "step-execute", description: "Execute the target ClickUp operation.", kind: "execute" },
        { id: "step-format", description: "Format results into user response.", kind: "format" },
      ],
    };

    // ── Phase 3: Execute plan ────────────────────────────────────────────────
    return runClickUpPlan(ctx, plan, resolvedIntent.operation_kind);
  },
});

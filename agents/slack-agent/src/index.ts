/// <reference path="./baml-runtime.d.ts" />

import type {
  ReplyPart,
  RunContext,
  SessionResult,
  SlackPlanStep,
  SlackStructuredPlan,
  StructuredReply,
} from "./baml-runtime";
import { onSlackSourceDispatch } from "./slackSourceIngress";

const MAX_REACT_STEPS = 10;
const PKG_RETRIEVE = "slack-retrieve";
const PKG_SYNTH = "slack-synthesize";

function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value != null && typeof value === "object";
}

function extractText(message: ChatMessage): string {
  const parts = message.parts ?? [];
  const textParts = parts
    .map((part) => (typeof part.text === "string" ? part.text : ""))
    .filter((text) => text.length > 0);
  if (textParts.length === 0) return "";
  return textParts.join("\n");
}

function slugKey(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48) || "slack";
}

function isNeedClarification(v: unknown): v is { question: string } {
  return (
    isObject(v) &&
    typeof v.question === "string" &&
    v.question.trim().length > 0 &&
    !("intent" in v) &&
    !("reason" in v)
  );
}

function isNotRelevant(v: unknown): v is { reason: string } {
  return isObject(v) && typeof v.reason === "string" && !("question" in v) && !("intent" in v);
}

function isSlackIntent(v: unknown): v is { intent: string } {
  return (
    isObject(v) &&
    typeof v.intent === "string" &&
    v.intent.trim().length > 0 &&
    !("question" in v) &&
    !("reason" in v)
  );
}

/**
 * Coordination-only: verify the runtime value matches SlackStructuredPlan after PlanSlackWork.
 * Does not invent or repair plans — agentic planning must satisfy the contract or we surface an error.
 */
function parseSlackStructuredPlanFromPlanning(v: unknown): SlackStructuredPlan | null {
  if (!isObject(v)) return null;
  if (typeof v.intent_description !== "string" || typeof v.objective !== "string") return null;
  if (!Array.isArray(v.plan_steps)) return null;
  const c = v.citations;
  if (c != null && !Array.isArray(c)) return null;
  return v as unknown as SlackStructuredPlan;
}

/**
 * Validate plan_steps against slack_prompt.baml (retrieve → … → single synthesize at end).
 * Returns steps in model order, or a human-readable contract violation message.
 */
function validateSlackPlanForExecution(plan: SlackStructuredPlan): SlackPlanStep[] | string {
  const raw = plan.plan_steps;
  if (raw.length === 0) return "plan_steps is empty";

  const steps: SlackPlanStep[] = [];
  for (let i = 0; i < raw.length; i++) {
    const s = raw[i];
    if (!isObject(s)) return `plan_steps[${i}] is not an object`;
    if (typeof s.sub_message !== "string" || !s.sub_message.trim()) {
      return `plan_steps[${i}].sub_message must be a non-empty string`;
    }
    const pkgRaw = s.agent_package;
    const instRaw = s.agent_instance_id;
    if (typeof pkgRaw !== "string" || typeof instRaw !== "string") {
      return `plan_steps[${i}] missing agent_package or agent_instance_id`;
    }
    const pkg = pkgRaw.trim().toLowerCase();
    if (pkg !== PKG_RETRIEVE && pkg !== PKG_SYNTH) {
      return `plan_steps[${i}] has invalid agent_package "${pkgRaw}" (expected slack-retrieve or slack-synthesize)`;
    }
    steps.push({
      agent_package: pkgRaw.trim(),
      agent_instance_id: instRaw.trim() || "default",
      sub_message: s.sub_message,
    });
  }

  let retrieveCount = 0;
  let synthCount = 0;
  for (const st of steps) {
    const p = st.agent_package.trim().toLowerCase();
    if (p === PKG_RETRIEVE) retrieveCount++;
    if (p === PKG_SYNTH) synthCount++;
  }
  if (retrieveCount < 1) return "plan must include at least one slack-retrieve step";
  if (synthCount !== 1) return "plan must include exactly one slack-synthesize step";
  const lastPkg = steps[steps.length - 1]!.agent_package.trim().toLowerCase();
  if (lastPkg !== PKG_SYNTH) return "last plan step must be slack-synthesize";

  return steps;
}

async function runSlackStructuredPlan(
  _ctx: RunContext,
  userText: string,
  structured: SlackStructuredPlan,
  validatedIntent: string,
  steps: SlackPlanStep[],
): Promise<SessionResult> {
  const goal = structured.objective.trim() || validatedIntent;

  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession("slack-" + Date.now().toString())
      : null;
  const intentSlug = slugKey(structured.intent_description || goal);
  const intentId = "intent-slack-" + intentSlug;
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: structured.intent_description || goal,
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: "plan-slack-" + intentSlug,
        steps: steps.map((s, i) => ({
          stepId: "step-" + i,
          description: s.sub_message,
          order: i,
          dependsOn: i > 0 ? ["step-" + (i - 1)] : [],
        })),
      })
    : null;

  try {
    for (let i = 0; i < steps.length; i++) {
      const step = steps[i]!;
      const stepId = "step-" + i;
      const pkg = step.agent_package.trim().toLowerCase();

      if (executable) await executable.startStep?.(stepId);

      if (pkg === PKG_RETRIEVE) {
        const slackRun = await runGeneratedStepExecutor(
          "ChooseSlackAction",
          {
            goal,
            step_description: step.sub_message,
          },
          { max_steps: MAX_REACT_STEPS },
        );
        if (slackRun.outcome !== "completed") {
          throw new Error(
            slackRun.outcome === "fatal"
              ? slackRun.message
              : `[${slackRun.recovery.code}] ${slackRun.recovery.mistake}`,
          );
        }
      } else if (pkg === PKG_SYNTH) {
        const finalMessage = await ReactToSlackResults({
          goal: `${goal}\nSynthesis brief: ${step.sub_message}`,
          user_message: userText,
        });
        if (executable) await executable.completeStep?.(stepId);
        if (executable) await executable.finish?.();
        return { message: finalMessage };
      } else {
        if (executable) await executable.completeStep?.(stepId);
      }

      if (executable) await executable.completeStep?.(stepId);
    }

    if (executable) await executable.finish?.();
    return { message: textReply("Slack plan completed without a synthesize step.") };
  } catch (e) {
    const errMsg = e instanceof Error ? e.message : String(e);
    try {
      if (executable) await executable.abort?.(errMsg);
    } catch {
      /* best-effort */
    }
    return { error: `Slack agent error: ${errMsg}` };
  }
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const originalText =
      typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : extractText(ctx.message);
    let text = originalText.trim();

    let validatedIntent: string | null = null;
    // NeedClarification must always use awaitInput so the host emits TASK_STATE_INPUT_REQUIRED — never
    // substitute a fake intent after N rounds (that completed the stream without suspending).
    while (true) {
      const intentResult = await InferSlackIntent({});

      if (isSlackIntent(intentResult)) {
        validatedIntent = intentResult.intent;
        break;
      }
      if (isNotRelevant(intentResult)) {
        return {
          message: textReply(`This doesn't look like a Slack request — ${intentResult.reason}`),
        };
      }
      if (isNeedClarification(intentResult)) {
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarified = messageText(reply).trim();
        if (clarified) text = clarified;
        continue;
      }
      return {
        error:
          "InferSlackIntent returned an unexpected shape — expected SlackIntent, NeedClarification, or NotRelevant.",
      };
    }
    if (!validatedIntent) return { error: "Could not determine a valid Slack intent." };

    const planned = await PlanSlackWork({ intent: validatedIntent });
    const structured = parseSlackStructuredPlanFromPlanning(planned);
    if (!structured) {
      return {
        error:
          "Planning failed: PlanSlackWork did not return a valid SlackStructuredPlan shape. Try rephrasing your request.",
      };
    }
    const stepsOrErr = validateSlackPlanForExecution(structured);
    if (typeof stepsOrErr === "string") {
      return {
        error: `Planning output did not satisfy the Slack execution contract: ${stepsOrErr}`,
      };
    }

    return runSlackStructuredPlan(ctx, text, structured, validatedIntent, stepsOrErr);
  },

  onDispatch: onSlackSourceDispatch,
});

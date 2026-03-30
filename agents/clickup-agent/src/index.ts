/// <reference path="./baml-runtime.d.ts" />
import type {
  ClickUpIntent,
  FinalResponse,
  JsonObject,
  JsonValue,
  NeedClarification,
  NotRelevant,
  ReplyPart,
  RunContext,
  SessionResult,
  StandardAgentPlanStep,
  StandardStructuredPlan,
  StructuredReply,
} from "./baml-runtime";

const MAX_REACT_STEPS = 8;
const MAX_CLARIFY = 2;
/** Max chars threaded into the next ChooseClickUpAction hop as `prior_results` (agentic / non-deterministic stream). */
const PRIOR_RESULTS_MAX_CHARS = 6000;

const PKG_CLICKUP_EXECUTE = "clickup-execute";
const PKG_CLICKUP_FORMAT = "clickup-format";

function isJsonObject(v: unknown): v is JsonObject {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function slugGoal(goal: string): string {
  return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
}

function isNeedClarification(v: unknown): v is NeedClarification {
  return isJsonObject(v) && typeof v.question === "string" && v.question.trim().length > 0
    && !("message" in v) && !("intent" in v) && !("reason" in v) && !("steps" in v);
}

function isNotRelevant(v: unknown): v is NotRelevant {
  return isJsonObject(v) && typeof v.reason === "string" && !("question" in v) && !("intent" in v);
}

function isClickUpIntent(v: unknown): v is ClickUpIntent {
  return isJsonObject(v) && typeof v.intent === "string" && v.intent.trim().length > 0
    && typeof v.operation_kind === "string" && !("question" in v) && !("reason" in v);
}

/** Coordination-only: shape check after PlanClickUpWork — no TS plan synthesis. */
function parseStandardStructuredPlanFromPlanning(v: unknown): StandardStructuredPlan | null {
  if (!isJsonObject(v)) return null;
  if (typeof v.intent_description !== "string" || typeof v.objective !== "string") return null;
  if (!Array.isArray(v.plan_steps)) return null;
  const c = v.citations;
  if (c != null && !Array.isArray(c)) return null;
  return v as unknown as StandardStructuredPlan;
}

function isFinalResponse(v: unknown): v is FinalResponse {
  if (!isJsonObject(v)) return false;
  if (typeof v.message !== "string") return false;
  return !("tasks" in v || "items" in v || "steps" in v || "action" in v || "intent" in v);
}

function stringifyUnknown(value: unknown, max: number): string {
  try {
    const s = JSON.stringify(value, null, 2);
    return s.length > max ? `${s.slice(0, max)}\n…` : s;
  } catch {
    const s = String(value);
    return s.length > max ? `${s.slice(0, max)}…` : s;
  }
}

/**
 * If this step wraps a list-shaped tool body, pull it for prettier lines.
 * Supports executor wrappers like:
 * - step.result.{tasks|items}
 * - step.output.result.{tasks|items}
 * - step.output.{tasks|items}
 * - step.{tasks|items}
 */
function extractToolLikePayload(v: unknown): JsonObject | null {
  if (!isJsonObject(v)) return null;

  const candidates: unknown[] = [
    v.result,
    isJsonObject(v.output) ? v.output.result : undefined,
    v.output,
    v,
  ];

  for (const c of candidates) {
    if (!isJsonObject(c)) continue;
    if (Array.isArray(c.tasks) || Array.isArray(c.items)) return c;
  }

  return null;
}

/**
 * Pull a plain message string from common executor wrappers.
 */
function extractStepMessageText(v: unknown): string | null {
  if (!isJsonObject(v)) return null;

  const candidates: unknown[] = [
    v.result,
    isJsonObject(v.output) ? v.output.result : undefined,
    v.output,
    v,
  ];

  for (const c of candidates) {
    if (!isJsonObject(c)) continue;
    if (typeof c.message === "string" && c.message.trim()) return c.message.trim();
  }

  return null;
}

/**
 * Serialize one executor step for `prior_results` — same spirit as claude-session-demo's
 * `formatLastToolOutputFromExecutorRun`: **agentic** hops, not a fixed tool schema.
 */
function executorStepToPriorContextText(step: unknown): string {
  if (step == null) return "";
  if (isFinalResponse(step)) {
    return `[final_response]\n${step.message}`.trim();
  }
  const toolLike = extractToolLikePayload(step);
  if (toolLike) {
    return `[tool_result]\n${formatListLikeToolPayload(toolLike)}`.trim();
  }
  const stepMessage = extractStepMessageText(step);
  if (stepMessage) {
    return `[message]\n${stepMessage}`;
  }
  return `[raw]\n${stringifyUnknown(step, 3500)}`;
}

function collectStepResultsForPriorContext(steps: unknown[]): string {
  const parts: string[] = [];
  for (const step of steps) {
    const block = executorStepToPriorContextText(step);
    if (block) parts.push(block);
  }
  const joined = parts.join("\n\n---\n\n");
  if (joined.trim()) return joined.slice(0, PRIOR_RESULTS_MAX_CHARS);
  return stringifyUnknown(steps.slice(-5), PRIOR_RESULTS_MAX_CHARS);
}

/**
 * Collect structured tool evidence for the final synthesis hop.
 * Keep only tool-shaped payloads (tasks/items) — real API data from tool sessions.
 * FinalResponse (LLM summaries) are intentionally excluded: they may hallucinate
 * counts or facts. When tool evidence is empty, ReactToClickUpResults falls back
 * to conversation history which contains the raw @N tool outputs.
 */
function collectToolResultsJson(steps: unknown[]): string {
  const outputs: unknown[] = [];
  for (const step of steps) {
    const toolLike = extractToolLikePayload(step);
    if (toolLike) outputs.push(toolLike);
  }

  try {
    return JSON.stringify(outputs.length > 0 ? outputs : [], null, 2).slice(0, 12_000);
  } catch {
    return "[]";
  }
}


function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

function finalResponseToStructured(fr: FinalResponse): StructuredReply {
  const msg = fr.message.trim() || "Done.";
  const parts: StructuredReply["parts"] = [{ type: "text", text: msg }];
  const sj = typeof fr.structured_json === "string" ? fr.structured_json.trim() : "";
  if (sj) {
    parts.push({ type: "data", data: sj, media_type: "application/json" });
  }
  const citations = Array.isArray(fr.citations)
    ? fr.citations.filter((c): c is string => typeof c === "string")
    : [];
  return { parts, citations };
}

function formatLineFromClickUpItem(entry: JsonValue): string {
  if (!isJsonObject(entry)) return "";
  const kind = typeof entry.kind === "string" ? entry.kind : "";
  const name = typeof entry.name === "string" ? entry.name : "";
  const id = typeof entry.id === "string" ? entry.id : "";
  if (!name && !id) return "";
  return `• [${kind}] ${name} (id: ${id})`;
}

function formatLineFromClickUpTaskSummary(entry: JsonValue): string {
  if (!isJsonObject(entry)) return "";
  const name = typeof entry.name === "string" ? entry.name : "Unnamed task";
  const status = typeof entry.status === "string" ? entry.status : "unknown";
  const url = typeof entry.url === "string" ? entry.url : "";
  return `• ${name} [${status}]${url ? ` — ${url}` : ""}`;
}

/** Optional readable view when payload has list-shaped `tasks` / `items` arrays. */
function formatListLikeToolPayload(output: JsonObject): string {
  const msg = output.message;
  let response = typeof msg === "string" ? msg : "Done.";
  const items = output.items;
  if (Array.isArray(items) && items.length > 0) {
    response += "\n\n" + items.map(formatLineFromClickUpItem).filter((s) => s.length > 0).join("\n");
  }
  const tasks = output.tasks;
  if (Array.isArray(tasks) && tasks.length > 0) {
    response += "\n\n" + tasks.map(formatLineFromClickUpTaskSummary).filter((s) => s.length > 0).join("\n");
  }
  return response;
}

function extractFinalMessage(steps: unknown[]): StructuredReply {
  for (const step of [...steps].reverse()) {
    if (isFinalResponse(step)) return finalResponseToStructured(step);
    const toolLike = extractToolLikePayload(step);
    if (toolLike) return textReply(formatListLikeToolPayload(toolLike));
    const stepMessage = extractStepMessageText(step);
    if (stepMessage) {
      return textReply(stepMessage);
    }
  }
  if (steps.length > 0) {
    const raw = stringifyUnknown(steps[steps.length - 1], 4000);
    if (raw.trim()) return textReply(`ClickUp session produced:\n${raw}`);
  }
  return textReply("ClickUp returned no usable response for this request.");
}

function filterPlanCitations(plan: StandardStructuredPlan): string[] {
  const raw = plan.citations;
  if (!Array.isArray(raw)) return [];
  return raw.filter((c): c is string => typeof c === "string" && c.trim().length > 0);
}

/**
 * Validate plan_steps against clickup_prompt.baml (zero-or-more execute → single format at end).
 */
function validateClickUpPlanForExecution(plan: StandardStructuredPlan): StandardAgentPlanStep[] | string {
  const raw = plan.plan_steps;
  if (raw.length === 0) return "plan_steps is empty";

  const steps: StandardAgentPlanStep[] = [];
  for (let i = 0; i < raw.length; i++) {
    const s = raw[i];
    if (!isJsonObject(s)) return `plan_steps[${i}] is not an object`;
    if (typeof s.sub_message !== "string" || !s.sub_message.trim()) {
      return `plan_steps[${i}].sub_message must be a non-empty string`;
    }
    const pkgRaw = s.agent_package;
    const instRaw = s.agent_instance_id;
    if (typeof pkgRaw !== "string" || typeof instRaw !== "string") {
      return `plan_steps[${i}] missing agent_package or agent_instance_id`;
    }
    const pkg = pkgRaw.trim().toLowerCase();
    if (pkg !== PKG_CLICKUP_EXECUTE && pkg !== PKG_CLICKUP_FORMAT) {
      return `plan_steps[${i}] has invalid agent_package "${pkgRaw}" (expected clickup-execute or clickup-format)`;
    }
    steps.push({
      agent_package: pkgRaw.trim(),
      agent_instance_id: instRaw.trim() || "default",
      sub_message: s.sub_message,
    });
  }

  let formatCount = 0;
  for (const st of steps) {
    const p = st.agent_package.trim().toLowerCase();
    if (p === PKG_CLICKUP_FORMAT) formatCount++;
  }
  // Allow format-only plans when prior conversation already has enough ClickUp evidence.
  if (formatCount !== 1) return "plan must include exactly one clickup-format step";
  const lastPkg = steps[steps.length - 1]!.agent_package.trim().toLowerCase();
  if (lastPkg !== PKG_CLICKUP_FORMAT) return "last plan step must be clickup-format";
  // Format-only plans (single clickup-format step) are valid — data comes from conversation history.
  // No need to require execute steps.

  return steps;
}

async function runClickUpStructuredPlan(
  _ctx: RunContext,
  structured: StandardStructuredPlan,
  validatedIntent: string,
  operationKind: string,
  steps: StandardAgentPlanStep[],
): Promise<SessionResult> {
  const goal = structured.objective.trim() || validatedIntent;
  const intentSlug = slugGoal(structured.intent_description || goal);

  const executionSession = typeof openA2aExecutionSession === "function"
    ? await openA2aExecutionSession("clickup-" + Date.now().toString())
    : null;
  const intentId = "intent-clickup-" + intentSlug;
  const citations = filterPlanCitations(structured);
  const intentPhase = executionSession
    ? await executionSession.submitIntent({
        intentId,
        description: structured.intent_description || goal,
        ...(citations.length > 0 ? { citations } : {}),
      })
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({
        intentId,
        planId: "plan-clickup-" + intentSlug,
        steps: steps.map((s, i) => ({
          stepId: "step-" + i,
          description: s.sub_message,
          order: i,
          dependsOn: i > 0 ? ["step-" + (i - 1)] : [],
        })),
      })
    : null;

  const allStepOutputsNested: unknown[][] = [];
  let priorResultsText: string | null = null;

  try {
    for (let i = 0; i < steps.length; i++) {
      const step = steps[i]!;
      const stepId = "step-" + i;
      const pkg = step.agent_package.trim().toLowerCase();

      if (executable) await executable.startStep?.(stepId);

      if (pkg === PKG_CLICKUP_EXECUTE) {
        const run = await runGeneratedStepExecutor("ChooseClickUpAction", {
          goal,
          step_description: step.sub_message,
          operation_kind: operationKind,
          prior_results: priorResultsText,
        }, { max_steps: MAX_REACT_STEPS });
        allStepOutputsNested.push(run.steps);
        priorResultsText = collectStepResultsForPriorContext(run.steps);
        if (executable) await executable.completeStep?.(stepId);
      } else if (pkg === PKG_CLICKUP_FORMAT) {
        const allSteps = allStepOutputsNested.flat();
        const toolResultsJson = collectToolResultsJson(allSteps);

        let finalMessage: StructuredReply;
        try {
          finalMessage = await ReactToClickUpResults({
            goal,
            user_message: validatedIntent,
            format_instructions: step.sub_message,
            tool_results_json: toolResultsJson,
          });
        } catch (_) {
          // Fallback keeps chat usable if synthesis parsing/provider call fails.
          finalMessage = extractFinalMessage(allSteps);
        }

        if (executable) await executable.completeStep?.(stepId);
        if (executable) await executable.finish?.();
        return { message: finalMessage };
      } else {
        if (executable) await executable.completeStep?.(stepId);
      }
    }

    if (executable) await executable.finish?.();
    return { message: textReply("ClickUp plan completed without a format step.") };
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

    // ── Phase 1: Intent inference (agentic — can clarify like claude-session-demo RequirementsPhase) ──
    let resolvedIntent: ClickUpIntent | null = null;
    for (let i = 0; i <= MAX_CLARIFY; i++) {
      const intentResult = await InferClickUpIntent({ user_message: text });

      if (isClickUpIntent(intentResult)) {
        resolvedIntent = intentResult;
        break;
      }
      if (isNotRelevant(intentResult)) {
        return {
          message: textReply(`This doesn't look like a ClickUp request — ${intentResult.reason}`),
        };
      }
      if (isNeedClarification(intentResult) && i < MAX_CLARIFY) {
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = clarifiedText;
      } else {
        resolvedIntent = { intent: text, operation_kind: "read" };
        break;
      }
    }
    if (!resolvedIntent) return { error: "Could not determine a valid ClickUp intent." };

    // ── Phase 2: Planning (agentic — StandardStructuredPlan from BAML only) ──
    const planResult = await PlanClickUpWork({
      intent: resolvedIntent.intent,
      operation_kind: resolvedIntent.operation_kind,
    });
    const structured = parseStandardStructuredPlanFromPlanning(planResult);
    if (!structured) {
      return {
        error:
          "Planning failed: PlanClickUpWork did not return a valid StandardStructuredPlan shape. Try rephrasing your request.",
      };
    }
    const stepsOrErr = validateClickUpPlanForExecution(structured);
    if (typeof stepsOrErr === "string") {
      return {
        error: `Planning output did not satisfy the ClickUp execution contract: ${stepsOrErr}`,
      };
    }

    return runClickUpStructuredPlan(
      ctx,
      structured,
      resolvedIntent.intent,
      resolvedIntent.operation_kind,
      stepsOrErr,
    );
  },
});

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
 * If this step wraps a list-shaped JSON body (common after a tool send), pull it for prettier lines.
 * Steps are still **best-effort**: the model may clarify, omit fields, or return unrelated shapes.
 */
function extractToolLikePayload(v: unknown): JsonObject | null {
  if (isJsonObject(v) && (Array.isArray(v.tasks) || Array.isArray(v.items))) return v;
  if (
    isJsonObject(v) &&
    isJsonObject(v.output) &&
    (Array.isArray(v.output.tasks) || Array.isArray(v.output.items))
  ) {
    return v.output;
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
  if (isJsonObject(step) && typeof step.message === "string" && step.message.trim()) {
    return `[message]\n${step.message.trim()}`;
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
    if (isJsonObject(step) && typeof step.message === "string" && step.message.trim()) {
      return textReply(step.message.trim());
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

function extractStepOutputString(step: unknown): string | null {
  if (!isJsonObject(step)) return null;
  const output = step.output;
  if (typeof output === "string" && output.trim().length > 0) return output;
  return null;
}

function executorRunEndedFinished(run: { steps?: unknown[]; last?: unknown }): boolean {
  if (isJsonObject(run.last) && typeof run.last.status === "string") {
    return run.last.status.toLowerCase() === "finished";
  }
  if (!Array.isArray(run.steps)) return false;
  for (const step of [...run.steps].reverse()) {
    if (!isJsonObject(step) || typeof step.status !== "string") continue;
    return step.status.toLowerCase() === "finished";
  }
  return false;
}

function maxArchiveLinesHintFromSteps(steps: unknown[]): number {
  let maxLines = 0;
  for (const step of steps) {
    const output = extractStepOutputString(step);
    if (!output) continue;
    const m = output.match(/\[(\d+)\s+lines?,/i);
    if (!m) continue;
    const lines = Number(m[1]);
    if (Number.isFinite(lines) && lines > maxLines) maxLines = lines;
  }
  return maxLines;
}

function hasReadLikeOutputInSteps(steps: unknown[]): boolean {
  for (const step of steps) {
    const output = extractStepOutputString(step);
    if (!output) continue;
    const lower = output.toLowerCase();
    if (lower.includes("cat -n @") || lower.includes("grep -n '") || lower.includes("grep -n \"")) {
      return true;
    }
  }
  return false;
}

function shouldForceOneMoreReadHop(run: { steps?: unknown[]; last?: unknown }): boolean {
  const steps = Array.isArray(run.steps) ? run.steps : [];
  if (steps.length === 0) return false;
  if (!executorRunEndedFinished(run)) return false;
  if (hasReadLikeOutputInSteps(steps)) return false;
  const maxLines = maxArchiveLinesHintFromSteps(steps);
  return maxLines > 200;
}

/**
 * Validate plan_steps against clickup_prompt.baml (execute → … → single format at end).
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

  let executeCount = 0;
  let formatCount = 0;
  for (const st of steps) {
    const p = st.agent_package.trim().toLowerCase();
    if (p === PKG_CLICKUP_EXECUTE) executeCount++;
    if (p === PKG_CLICKUP_FORMAT) formatCount++;
  }
  if (executeCount < 1) return "plan must include at least one clickup-execute step";
  if (formatCount !== 1) return "plan must include exactly one clickup-format step";
  const lastPkg = steps[steps.length - 1]!.agent_package.trim().toLowerCase();
  if (lastPkg !== PKG_CLICKUP_FORMAT) return "last plan step must be clickup-format";

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

        // Post-parse safety net: when a large archive SendDone is observed but the executor
        // immediately ends with Finish and no Read-like hop, request one constrained
        // follow-up hop to perform bounded drilldown before terminal completion.
        if (shouldForceOneMoreReadHop(run)) {
          const guardPrior = collectStepResultsForPriorContext(run.steps);
          const mergedPrior = [priorResultsText, guardPrior].filter((v): v is string => typeof v === "string" && v.length > 0).join("\n\n---\n\n");
          const drilldownRun = await runGeneratedStepExecutor("ChooseClickUpAction", {
            goal,
            step_description:
              `${step.sub_message}\n\nGuardrail: the latest archive indicates a large result. Before Finish, emit one bounded Read on the latest relevant @N (use offset pagination and optional grep) unless evidence is already explicitly complete for this step.`,
            operation_kind: operationKind,
            prior_results: mergedPrior.slice(0, PRIOR_RESULTS_MAX_CHARS),
          }, { max_steps: 3 });
          allStepOutputsNested.push(drilldownRun.steps);
          priorResultsText = collectStepResultsForPriorContext(drilldownRun.steps);
        } else {
          priorResultsText = collectStepResultsForPriorContext(run.steps);
        }

        if (executable) await executable.completeStep?.(stepId);
      } else if (pkg === PKG_CLICKUP_FORMAT) {
        const finalMessage = extractFinalMessage(allStepOutputsNested.flat());
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

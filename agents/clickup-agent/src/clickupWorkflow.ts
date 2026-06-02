/// <reference path="./baml-runtime.d.ts" />
/**
 * ClickUp plan execution — intent/plan provenance + step-executor loop.
 * Sequenced like slack-agent `runSlackStructuredPlan` / how-to-write-agents §3.6.
 */
import type {
  ClickUpPlanStep,
  ClickUpStructuredPlan,
  FinalResponse,
  JsonObject,
  JsonValue,
  ReplyPart,
  SessionResult,
  StructuredReply,
} from "./baml-runtime";

export const MAX_REACT_STEPS = 8;
export const PKG_CLICKUP_EXECUTE = "clickup-execute";
export const PKG_CLICKUP_FORMAT = "clickup-format";

const PRIOR_RESULTS_MAX_CHARS = 6000;

function isJsonObject(v: unknown): v is JsonObject {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function textReply(text: string): StructuredReply {
  const parts: ReplyPart[] = [{ type: "text", text }];
  return { parts, citations: [] };
}

function slugGoal(goal: string): string {
  return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
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

function isFinalResponse(v: unknown): v is FinalResponse {
  if (!isJsonObject(v)) return false;
  if (typeof v.message !== "string") return false;
  return !("tasks" in v || "items" in v || "steps" in v || "action" in v || "intent" in v);
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

function filterPlanCitations(plan: ClickUpStructuredPlan): string[] {
  const raw = plan.citations;
  if (!Array.isArray(raw)) return [];
  return raw.filter((c): c is string => typeof c === "string" && c.trim().length > 0);
}

/**
 * Commit intent/plan to provenance and run clickup-execute → clickup-format steps.
 * Call from chat (`index.ts`) or dispatch (`withTask` + unit-scoped session label).
 */
export async function runClickUpStructuredPlan(
  sessionLabel: string,
  structured: ClickUpStructuredPlan,
  validatedIntent: string,
  operationKind: string,
  steps: ClickUpPlanStep[],
): Promise<SessionResult> {
  const goal = structured.objective.trim() || validatedIntent;
  const intentSlug = slugGoal(structured.intent_description || goal);

  const executionSession =
    typeof openA2aExecutionSession === "function"
      ? await openA2aExecutionSession(sessionLabel)
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
        const run = await runGeneratedStepExecutor(
          "ChooseClickUpAction",
          {
            goal,
            step_description: step.sub_message,
            operation_kind: operationKind,
            prior_results: priorResultsText,
          },
          { max_steps: MAX_REACT_STEPS },
        );
        if (run.outcome !== "completed") {
          if (run.outcome === "agent_correctable") {
            throw new Error(`[${run.recovery.code}] ${run.recovery.mistake}`);
          }
          throw new Error(run.message);
        }
        allStepOutputsNested.push(run.steps);
        priorResultsText = collectStepResultsForPriorContext(run.steps);
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
    try {
      if (executable) await executable.abort?.(errMsg);
    } catch {
      /* best-effort */
    }
    return { error: `ClickUp agent error: ${errMsg}` };
  }
}

/// <reference path="./baml-runtime.d.ts" />
import type {
  ClickUpIntent,
  FinalResponse,
  JsonObject,
  JsonValue,
  NeedClarification,
  NotRelevant,
  RunContext,
  SessionResult,
  StandardAgentPlanStep,
  StandardStructuredPlan,
  StructuredReply,
} from "./baml-runtime";

const MAX_REACT_STEPS = 8;
const MAX_CLARIFY = 2;
const PRIOR_RESULTS_MAX_CHARS = 6000;

const PKG_CLICKUP_EXECUTE = "clickup-execute";
const PKG_CLICKUP_FORMAT = "clickup-format";

// ── Utility ──────────────────────────────────────────────────────────────────

function isJsonObject(v: unknown): v is JsonObject {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function slugGoal(goal: string): string {
  return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
}

// ── Intent type guards ───────────────────────────────────────────────────────

function isNeedClarification(v: unknown): v is NeedClarification {
  return isJsonObject(v)
    && typeof v.question === "string" && v.question.trim().length > 0
    && !("intent" in v) && !("reason" in v);
}

function isNotRelevant(v: unknown): v is NotRelevant {
  return isJsonObject(v)
    && typeof v.reason === "string"
    && !("question" in v) && !("intent" in v);
}

function isClickUpIntent(v: unknown): v is ClickUpIntent {
  return isJsonObject(v)
    && typeof v.intent === "string" && v.intent.trim().length > 0
    && typeof v.operation_kind === "string";
}

function isFinalResponse(v: unknown): v is FinalResponse {
  if (!isJsonObject(v)) return false;
  if (typeof v.message !== "string") return false;
  return !("tasks" in v || "items" in v || "steps" in v || "action" in v || "intent" in v);
}

// ── Executor step typing ─────────────────────────────────────────────────────
//
// runGeneratedStepExecutor returns steps with these shapes after host processing:
//   Send   → { status:"done", output:"@N …", archive_ref, result:{ tasks?, items?, message } }
//   Read   → { status:"done", output:"…", has_more, next_offset }   (no result field)
//   Open   → { status:"open", session_id, tool_name }
//   Finish → { status:"finished" }
//
// A FinalResponse from the LLM appears as a raw passthrough (no status wrapper).

interface ExecutorStep {
  status: string;
  output?: string;
  archive_ref?: string;
  has_more?: boolean;
  next_offset?: number;
  result?: JsonObject;
}

function asExecutorStep(v: unknown): ExecutorStep | null {
  if (!isJsonObject(v) || typeof v.status !== "string") return null;
  return v as unknown as ExecutorStep;
}

/** Extract the structured tool payload from a Send step's `result` field. */
function getToolPayload(step: ExecutorStep): JsonObject | null {
  const r = step.result;
  if (!r || !isJsonObject(r)) return null;
  if (Array.isArray(r.tasks) || Array.isArray(r.items) || typeof r.message === "string") return r;
  return null;
}

/** Collect all structured tool payloads from an executor run's steps. */
function collectToolPayloads(steps: unknown[]): JsonObject[] {
  const payloads: JsonObject[] = [];
  for (const raw of steps) {
    const step = asExecutorStep(raw);
    if (!step) continue;
    const payload = getToolPayload(step);
    if (payload) payloads.push(payload);
  }
  return payloads;
}

// ── Reply formatting ─────────────────────────────────────────────────────────

function textReply(text: string): StructuredReply {
  return { parts: [{ type: "text", text }], citations: [] };
}

function finalResponseToStructured(fr: FinalResponse): StructuredReply {
  const msg = fr.message.trim() || "Done.";
  const parts: StructuredReply["parts"] = [{ type: "text", text: msg }];
  const sj = typeof fr.structured_json === "string" ? fr.structured_json.trim() : "";
  if (sj) parts.push({ type: "data", data: sj, media_type: "application/json" });
  const citations = Array.isArray(fr.citations)
    ? fr.citations.filter((c): c is string => typeof c === "string")
    : [];
  return { parts, citations };
}

function formatTaskLine(entry: JsonValue): string {
  if (!isJsonObject(entry)) return "";
  const name = typeof entry.name === "string" ? entry.name : "Unnamed task";
  const status = typeof entry.status === "string" ? entry.status : "unknown";
  const url = typeof entry.url === "string" ? entry.url : "";
  return `• ${name} [${status}]${url ? ` — ${url}` : ""}`;
}

function formatItemLine(entry: JsonValue): string {
  if (!isJsonObject(entry)) return "";
  const kind = typeof entry.kind === "string" ? entry.kind : "";
  const name = typeof entry.name === "string" ? entry.name : "";
  const id = typeof entry.id === "string" ? entry.id : "";
  if (!name && !id) return "";
  return `• [${kind}] ${name} (id: ${id})`;
}

function formatPayload(payload: JsonObject): string {
  const parts: string[] = [];
  const msg = typeof payload.message === "string" ? payload.message : "Done.";
  parts.push(msg);

  const tasks = payload.tasks;
  if (Array.isArray(tasks) && tasks.length > 0) {
    parts.push(tasks.map(formatTaskLine).filter(Boolean).join("\n"));
  }
  const items = payload.items;
  if (Array.isArray(items) && items.length > 0) {
    parts.push(items.map(formatItemLine).filter(Boolean).join("\n"));
  }
  return parts.join("\n\n");
}

/**
 * Find the most content-rich Read output from executor steps.
 * Read steps (grep results) have output text but no structured result.
 * Prefer the last Read with substantial content (grep-filtered data).
 */
function findBestReadOutput(rawSteps: unknown[]): string | null {
  let best: string | null = null;
  for (const raw of rawSteps) {
    const step = asExecutorStep(raw);
    if (!step) continue;
    // Read steps: have output string, no result field, status "done"
    if (step.status === "done" && typeof step.output === "string" && !step.result) {
      // Strip the archive header line ("@18 support/clickup ... [209 lines]\n")
      // and the "lines X-Y of Z:" prefix — keep only the YAML content
      const text = step.output;
      const yamlStart = text.indexOf("\n");
      if (yamlStart >= 0) {
        const content = text.slice(yamlStart + 1).trim();
        if (content.length > 50 && (!best || content.length > best.length)) {
          best = content;
        }
      }
    }
  }
  return best;
}

/**
 * Build the final user-facing message from collected payloads and raw steps.
 *
 * Priority: FinalResponse → structured payloads (tasks/items) →
 * Read output text (grep results) → payload messages → fallback.
 */
function buildFinalMessage(payloads: JsonObject[], rawSteps: unknown[]): StructuredReply {
  // 1. Explicit FinalResponse from any step (LLM passthrough or nested in result)
  for (const raw of [...rawSteps].reverse()) {
    if (isFinalResponse(raw)) return finalResponseToStructured(raw);
    const step = asExecutorStep(raw);
    if (step?.result && isFinalResponse(step.result)) {
      return finalResponseToStructured(step.result);
    }
  }

  // 2. Richest structured payload: tasks → items → message
  for (const p of [...payloads].reverse()) {
    if (Array.isArray(p.tasks) && p.tasks.length > 0) return textReply(formatPayload(p));
  }
  for (const p of [...payloads].reverse()) {
    if (Array.isArray(p.items) && p.items.length > 0) return textReply(formatPayload(p));
  }

  // 3. Read output text (grep-filtered results from archive)
  const readOutput = findBestReadOutput(rawSteps);
  if (readOutput) return textReply(readOutput);

  // 4. Any payload message
  for (const p of [...payloads].reverse()) {
    if (typeof p.message === "string" && p.message.trim()) return textReply(p.message.trim());
  }

  return textReply("ClickUp returned no usable response for this request.");
}

// ── Prior results for multi-step threading ───────────────────────────────────

function summarizeForPrior(payloads: JsonObject[]): string {
  if (payloads.length === 0) return "";
  const s = JSON.stringify(payloads.slice(-3), null, 2);
  return s.length > PRIOR_RESULTS_MAX_CHARS
    ? s.slice(0, PRIOR_RESULTS_MAX_CHARS) + "\n…(truncated)"
    : s;
}

// ── Plan parsing and validation ──────────────────────────────────────────────

function parseStructuredPlan(v: unknown): StandardStructuredPlan | null {
  if (!isJsonObject(v)) return null;
  if (typeof v.intent_description !== "string" || typeof v.objective !== "string") return null;
  if (!Array.isArray(v.plan_steps)) return null;
  if (v.citations != null && !Array.isArray(v.citations)) return null;
  return v as unknown as StandardStructuredPlan;
}

function filterCitations(plan: StandardStructuredPlan): string[] {
  if (!Array.isArray(plan.citations)) return [];
  return plan.citations.filter((c): c is string => typeof c === "string" && c.trim().length > 0);
}

function validatePlan(plan: StandardStructuredPlan): StandardAgentPlanStep[] | string {
  const raw = plan.plan_steps;
  if (raw.length === 0) return "plan_steps is empty";

  const steps: StandardAgentPlanStep[] = [];
  for (let i = 0; i < raw.length; i++) {
    const s = raw[i];
    if (!isJsonObject(s)) return `plan_steps[${i}] is not an object`;
    if (typeof s.sub_message !== "string" || !s.sub_message.trim()) {
      return `plan_steps[${i}].sub_message must be a non-empty string`;
    }
    if (typeof s.agent_package !== "string" || typeof s.agent_instance_id !== "string") {
      return `plan_steps[${i}] missing agent_package or agent_instance_id`;
    }
    const pkg = s.agent_package.trim().toLowerCase();
    if (pkg !== PKG_CLICKUP_EXECUTE && pkg !== PKG_CLICKUP_FORMAT) {
      return `plan_steps[${i}] has invalid agent_package "${s.agent_package}"`;
    }
    steps.push({
      agent_package: s.agent_package.trim(),
      agent_instance_id: s.agent_instance_id.trim() || "default",
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
  if (steps[steps.length - 1]!.agent_package.trim().toLowerCase() !== PKG_CLICKUP_FORMAT) {
    return "last plan step must be clickup-format";
  }

  return steps;
}

// ── Plan execution ───────────────────────────────────────────────────────────

async function executePlan(
  _ctx: RunContext,
  plan: StandardStructuredPlan,
  intent: string,
  operationKind: string,
  steps: StandardAgentPlanStep[],
): Promise<SessionResult> {
  const goal = plan.objective.trim() || intent;
  const intentSlug = slugGoal(plan.intent_description || goal);

  const session = typeof openA2aExecutionSession === "function"
    ? await openA2aExecutionSession("clickup-" + Date.now().toString())
    : null;

  const intentId = "intent-clickup-" + intentSlug;
  const citations = filterCitations(plan);
  const intentPhase = session
    ? await session.submitIntent({
        intentId,
        description: plan.intent_description || goal,
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

  const allPayloads: JsonObject[] = [];
  const allRawSteps: unknown[] = [];
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

        const payloads = collectToolPayloads(run.steps);
        allPayloads.push(...payloads);
        allRawSteps.push(...run.steps);
        priorResultsText = summarizeForPrior(payloads);

        if (executable) await executable.completeStep?.(stepId);
      } else if (pkg === PKG_CLICKUP_FORMAT) {
        const reply = buildFinalMessage(allPayloads, allRawSteps);
        if (executable) await executable.completeStep?.(stepId);
        if (executable) await executable.finish?.();
        return { message: reply };
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

// ── Entry point ──────────────────────────────────────────────────────────────

__chat_register({
  run: async (ctx) => {
    const originalText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    let text = originalText;

    // Phase 1: Intent inference with clarification loop
    let resolvedIntent: ClickUpIntent | null = null;
    for (let i = 0; i <= MAX_CLARIFY; i++) {
      const result = await InferClickUpIntent({ user_message: text });

      if (isClickUpIntent(result)) {
        resolvedIntent = result;
        break;
      }
      if (isNotRelevant(result)) {
        return { message: textReply(`This doesn't look like a ClickUp request — ${result.reason}`) };
      }
      if (isNeedClarification(result) && i < MAX_CLARIFY) {
        const reply = await ctx.emit.awaitInput(result.question);
        const clarified = messageText(reply).trim();
        if (clarified) text = clarified;
      } else {
        resolvedIntent = { intent: text, operation_kind: "read" };
        break;
      }
    }
    if (!resolvedIntent) return { error: "Could not determine a valid ClickUp intent." };

    // Phase 2: Planning
    const planResult = await PlanClickUpWork({
      intent: resolvedIntent.intent,
      operation_kind: resolvedIntent.operation_kind,
    });
    const plan = parseStructuredPlan(planResult);
    if (!plan) {
      return { error: "Planning failed: did not return a valid StandardStructuredPlan. Try rephrasing." };
    }
    const stepsOrErr = validatePlan(plan);
    if (typeof stepsOrErr === "string") {
      return { error: `Plan validation failed: ${stepsOrErr}` };
    }

    // Phase 3: Execute and format
    return executePlan(ctx, plan, resolvedIntent.intent, resolvedIntent.operation_kind, stepsOrErr);
  },
});

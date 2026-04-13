import type {
  CitationDetail,
  CitationSimilarityOnRow,
  ContextPlanningTaskSnapshot,
  ProvenanceRowBase,
} from "../types/provenance";

// ── Text helpers ────────────────────────────────────────────────────────────

export function nonEmptyText(value: string | null | undefined): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

export function humanizeId(raw: string | null | undefined): string {
  const value = nonEmptyText(raw);
  if (!value) return "unknown";
  const withoutPrefix = value.replace(/^(intent|plan|step)-/i, "");
  const normalized = withoutPrefix
    .replace(/[_-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (!normalized) return value;
  return normalized.charAt(0).toUpperCase() + normalized.slice(1);
}

export function toHumanText(raw: string): string {
  return raw.replace(/\bllm\b/gi, "LLM");
}

// ── Planning labels ─────────────────────────────────────────────────────────

export function taskKindLabel(taskId: string): string {
  if (taskId.startsWith("live-task:")) return "User task";
  if (taskId.startsWith("a2a-child-")) return "Delegated task";
  return "Task";
}

export function planningTaskTitle(task: ContextPlanningTaskSnapshot): string {
  return `${taskKindLabel(task.taskId)} ${shortIdLocal(task.taskId)}`;
}

export function planningIntentLabel(task: ContextPlanningTaskSnapshot): string {
  const description = nonEmptyText(task.currentIntent?.description);
  if (description) return toHumanText(description);
  return toHumanText(humanizeId(task.currentIntent?.intent_id));
}

export function planningPlanLabel(task: ContextPlanningTaskSnapshot): string {
  const plan = task.currentPlan;
  if (!plan) return "none";
  const stepSummary = plan.steps
    .slice()
    .sort((a, b) => a.order - b.order)
    .map((step) => planningStepLabel(step));
  if (stepSummary.length > 0) {
    return stepSummary.join(" -> ");
  }
  const intentDescription = nonEmptyText(task.currentIntent?.description);
  if (intentDescription) return toHumanText(intentDescription);
  return toHumanText(humanizeId(plan.plan_id));
}

export function planningStepLabel(step: { description?: string; step_id: string; order: number }): string {
  const description = nonEmptyText(step.description);
  if (description) return toHumanText(description);
  const delegateMatch = step.step_id.match(/^step-delegate-(\d+)$/i);
  if (delegateMatch) {
    const idx = Number.parseInt(delegateMatch[1] ?? "0", 10);
    const order = Number.isFinite(idx) ? idx + 1 : step.order + 1;
    return `Delegation step ${order}`;
  }
  const genericMatch = step.step_id.match(/^(?:step-)?(.+?)-(\d+)$/i);
  if (genericMatch) {
    const base = toHumanText(humanizeId(genericMatch[1]));
    const idx = Number.parseInt(genericMatch[2] ?? "0", 10);
    if (Number.isFinite(idx)) return `${base} ${idx + 1}`;
  }
  return `Step ${step.order + 1} · ${toHumanText(humanizeId(step.step_id))}`;
}

export function planningStatusLabel(status: string): string {
  const normalized = status.toLowerCase();
  if (normalized === "in_progress") return "in progress";
  return normalized;
}

export function planProgressPercent(task: ContextPlanningTaskSnapshot): number {
  const total = task.stepSummary.total;
  if (!total || total <= 0) return 0;
  return Math.round((task.stepSummary.completed / total) * 100);
}

// ── Status/severity CSS classes ─────────────────────────────────────────────

export function stepStatusClass(status: string): string {
  const normalized = status.toLowerCase();
  if (normalized === "completed") return "step-status-completed";
  if (normalized === "failed") return "step-status-failed";
  if (normalized === "running" || normalized === "in_progress") return "step-status-running";
  return "step-status-pending";
}

export function driftSeverityClass(severity: string | null | undefined): string {
  if (!severity) return "";
  const normalized = severity.toLowerCase();
  if (normalized === "block") return "drift-severity-block";
  if (normalized === "warn") return "drift-severity-warn";
  if (normalized === "acceptable") return "drift-severity-ok";
  return "";
}

export function driftSeverityLabel(severity: string | null | undefined): string {
  if (!severity) return "";
  return severity.toLowerCase();
}

export function formatDriftScore(score: number | null | undefined): string {
  if (score == null) return "—";
  return score.toFixed(2);
}

export function taskHasDrift(task: ContextPlanningTaskSnapshot): boolean {
  return task.drift != null && task.drift.compositeSeverity != null;
}

export function groundingCallCount(task: ContextPlanningTaskSnapshot): number {
  const calls = task.drift?.driftedCalls;
  if (!calls?.length) return 0;
  return calls.filter((c) => (c.citations?.length ?? 0) > 0).length;
}

// ── Citation helpers ────────────────────────────────────────────────────────

export function citationRefLabel(c: CitationDetail | CitationSimilarityOnRow): string {
  const raw = "raw" in c && c.raw ? c.raw : (c.isHistory ? `#${c.n}` : `@${c.n}`);
  return raw;
}

export function citationSimClass(sim: number, negated: boolean): string {
  if (negated) return "cite-sim-counter";
  if (sim >= 0.65) return "cite-sim-high";
  if (sim >= 0.40) return "cite-sim-mid";
  return "cite-sim-low";
}

export function citationSimLabel(sim: number, negated: boolean): string {
  if (negated) return "counter";
  if (sim >= 0.65) return "strong";
  if (sim >= 0.40) return "moderate";
  return "weak";
}

export function normalizePerCitationItem(item: Record<string, unknown>): CitationSimilarityOnRow {
  const raw = typeof item.raw === "string" ? item.raw : undefined;
  const n = typeof item.n === "number" ? item.n : Number(item.n ?? 0);
  let isHistory = true;
  if (item.isHistory === true || item.is_history === true) isHistory = true;
  else if (item.isHistory === false || item.is_history === false) isHistory = false;
  const negated = Boolean(item.negated);
  const similarity = typeof item.similarity === "number" ? item.similarity : Number(item.similarity ?? 0);
  const activityAnchor =
    typeof item.activityAnchor === "string"
      ? item.activityAnchor
      : typeof item.activity_anchor === "string"
        ? item.activity_anchor
        : undefined;
  const contentPreview =
    typeof item.contentPreview === "string"
      ? item.contentPreview
      : typeof item.content_preview === "string"
        ? item.content_preview
        : undefined;
  return { n, isHistory, negated, similarity, raw, activityAnchor, contentPreview };
}

export function rowCitationDrift(row: ProvenanceRowBase): { perCitation: CitationSimilarityOnRow[]; meanSimilarity: number } | null {
  const drift = row.drift as Record<string, unknown> | undefined;
  const citRaw = drift?.citation as Record<string, unknown> | undefined;
  if (!citRaw) return null;
  const arr = (citRaw.perCitation ?? citRaw.per_citation) as unknown[] | undefined;
  if (!Array.isArray(arr) || arr.length === 0) return null;
  const perCitation = arr
    .map((x) =>
      x && typeof x === "object" ? normalizePerCitationItem(x as Record<string, unknown>) : null,
    )
    .filter((x): x is CitationSimilarityOnRow => x != null);
  if (perCitation.length === 0) return null;
  const meanRaw = citRaw.meanSimilarity ?? citRaw.mean_similarity;
  const meanSimilarity = typeof meanRaw === "number" ? meanRaw : Number(meanRaw ?? 1.0);
  return { perCitation, meanSimilarity };
}

export function meanPositiveSimilarity(citations: CitationDetail[]): number {
  const pos = citations.filter((c) => !c.negated);
  const use = pos.length > 0 ? pos : citations;
  if (use.length === 0) return 0;
  return use.reduce((s, c) => s + c.similarity, 0) / use.length;
}

// ── Explore table helpers ───────────────────────────────────────────────────

const structuredPayloadKeys = new Set(["llm_call", "llm_result", "tool_call", "tool_result"]);

export function parseMaybeJson(value: string): unknown {
  let current: unknown = value;
  for (let i = 0; i < 4; i += 1) {
    if (typeof current !== "string") break;
    const trimmed = current.trim();
    const looksJsonContainer =
      (trimmed.startsWith("{") && trimmed.endsWith("}")) ||
      (trimmed.startsWith("[") && trimmed.endsWith("]"));
    const looksJsonString = trimmed.startsWith('"') && trimmed.endsWith('"');
    if (!looksJsonContainer && !looksJsonString) break;
    try {
      current = JSON.parse(trimmed);
    } catch {
      break;
    }
  }
  return current;
}

export function decodeJsonLikeDeep(value: unknown, depth = 0): unknown {
  if (depth > 8) return value;
  if (typeof value === "string") {
    const parsed = parseMaybeJson(value);
    if (parsed === value) return value;
    return decodeJsonLikeDeep(parsed, depth + 1);
  }
  if (Array.isArray(value)) {
    return value.map((item) => decodeJsonLikeDeep(item, depth + 1));
  }
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      out[k] = decodeJsonLikeDeep(v, depth + 1);
    }
    return out;
  }
  return value;
}

export function normalizeDisplayValue(value: unknown, key: string): unknown {
  if (structuredPayloadKeys.has(key)) return decodeJsonLikeDeep(value);
  if (typeof value !== "string") return value;
  return parseMaybeJson(value);
}

export function formatAsReadableData(value: unknown, indent = 0): string {
  const pad = "  ".repeat(indent);
  const padChild = "  ".repeat(indent + 1);
  if (value === null || value === undefined) return String(value);
  if (typeof value === "boolean" || typeof value === "number") return String(value);
  if (typeof value === "string") {
    const lines = value.split("\n");
    if (lines.length <= 1) return value;
    return lines.map((line, i) => (i === 0 ? line : padChild + line)).join("\n");
  }
  if (Array.isArray(value)) {
    if (value.length === 0) return "[]";
    return value
      .map((item, i) => {
        const formatted = formatAsReadableData(item, indent + 1);
        if (formatted.includes("\n")) {
          const indented = formatted.replace(/^/gm, padChild);
          return `${pad}[${i}]\n${indented}`;
        }
        return `${pad}[${i}] ${formatted}`;
      })
      .join("\n");
  }
  if (typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>);
    if (entries.length === 0) return "{}";
    return entries
      .map(([k, v]) => {
        const formatted = formatAsReadableData(v, indent + 1);
        if (formatted.includes("\n")) {
          const indented = formatted.replace(/^/gm, padChild);
          return `${pad}${k}:\n${indented}`;
        }
        return `${pad}${k}: ${formatted}`;
      })
      .join("\n");
  }
  return String(value);
}

type SelectedRowEntry = {
  key: string;
  kind: "scalar" | "json";
  display: string;
};

export type { SelectedRowEntry };

export function formatSelectedValue(value: unknown, key: string): Omit<SelectedRowEntry, "key"> {
  const normalized = normalizeDisplayValue(value, key);
  if (normalized === null || normalized === undefined) {
    return { kind: "scalar", display: String(normalized) };
  }
  if (typeof normalized === "object") {
    return { kind: "json", display: formatAsReadableData(normalized) };
  }
  return { kind: "scalar", display: String(normalized) };
}

export function formatCellValue(col: string, value: unknown): string {
  if (value == null) return "";
  if (col === "drift" && typeof value === "object") {
    const d = value as Record<string, unknown>;
    const plan = d.plan as Record<string, unknown> | undefined;
    if (plan?.compositeSeverity) {
      const score = typeof plan.planAdherenceScore === "number"
        ? (plan.planAdherenceScore as number).toFixed(2)
        : "?";
      return `${score} ${plan.compositeSeverity}`;
    }
    if (typeof d.score === "number") {
      return `${(d.score as number).toFixed(2)} ${d.severity ?? ""}`;
    }
    return "";
  }
  if (col === "duration_ms" && typeof value === "number") {
    return value >= 1000 ? `${(value / 1000).toFixed(1)}s` : `${value}ms`;
  }
  if (col === "total_tokens" && typeof value === "number") {
    return value >= 1000 ? `${(value / 1000).toFixed(1)}k` : String(value);
  }
  if (col === "timestamp_ms" && typeof value === "number" && value > 0) {
    const d = new Date(value);
    return `${d.getHours().toString().padStart(2, "0")}:${d.getMinutes().toString().padStart(2, "0")}:${d.getSeconds().toString().padStart(2, "0")}`;
  }
  const s = String(value);
  return s.length > 48 ? s.slice(0, 45) + "..." : s;
}

export function formatColumnHeader(col: string): string {
  const labels: Record<string, string> = {
    baml_prompt: "Function",
    agent_display: "Agent",
    duration_ms: "Duration",
    total_tokens: "Tokens",
    activity_outcome: "Outcome",
    activity_kind: "Kind",
    activity_id: "Activity ID",
    cached_input_tokens: "Cached",
    failure_class: "Failure",
    failure_evidence: "Evidence",
    timestamp_ms: "Time",
  };
  return labels[col] ?? col;
}

// ── Shared constants ────────────────────────────────────────────────────────

export const payloadPriorityKeys = new Set([
  "message_text",
  "message_content",
  "llm_call",
  "llm_result",
  "tool_call",
  "tool_result",
]);

export const preferredSelectedRowKeys = [
  "activity_kind",
  "activity_id",
  "timestamp_ms",
  "context_id",
  "task_id",
  "agent_display",
  "agent_package",
  "agent_version",
  "agent_id",
  "message_id",
  "provider",
  "model",
  "tool_name",
  "baml_prompt",
  "failure_class",
  "failure_evidence",
  "duration_ms",
  "total_tokens",
  "cached_input_tokens",
  "message_text",
  "message_content",
  "llm_call",
  "llm_result",
  "tool_call",
  "tool_result",
];

export const driftHelp = {
  intent: "How closely the LLM response aligns with the declared intent. Measures whether the agent is still working on what the user originally asked for.",
  step: "How closely the LLM response matches the current plan step description. Detects when the agent does the wrong step or overshoots scope.",
  trajectory: "Running average of all responses vs the original intent. Detects gradual cumulative drift that individual call scores miss — the 'boiling frog' signal.",
  adherence: "Weighted composite of intent and step alignment. Early plan steps are weighted more heavily because they anchor the entire downstream execution.",
  composite: "Worst-case severity across all four dimensions. If any single dimension crosses a threshold, the composite reflects it.",
  planDriftTab:
    "Plan alignment (intent, step, trajectory, adherence) for this task. Expand each call to see grounding: how much the response resembles each cited history/archive snippet. Grounding is embedding similarity — not proof the facts are correct.",
  tactical:
    "Cosine similarity between the prompt's user message and the LLM response. Only meaningful when the prompt includes a user message.",
  grounding:
    "Each score compares the model's answer to the text of the ref it cited (#N history, @N archive). ≥0.65 strong, 0.40–0.65 moderate, <0.40 weak. Counter-evidence (!# / !@) is shown but excluded from the mean. See docs/drift-catalogue.md for full calibration.",
  groundingEmpty:
    "No resolved citation grounding on this row: the model may not have emitted citations, refs could not be resolved, or embedding scoring did not run.",
} as const;

// Private shortId for use within this module (avoids circular import with format.ts)
function shortIdLocal(id: string): string {
  if (id.length <= 12) return id;
  return `${id.slice(0, 8)}...${id.slice(-4)}`;
}

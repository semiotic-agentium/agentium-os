/** Synthetic coordinator tool: one graph-backed row per plan-step execution. */

export const EXECUTION_SESSION_STEP_TOOL_NAME = "a2a/execution_session_step";

export function isExecutionSessionStepTool(name: string): boolean {
  return (
    name === EXECUTION_SESSION_STEP_TOOL_NAME ||
    name.endsWith("/execution_session_step")
  );
}

/** Card header / primary label — avoids repeating the raw wire tool path. */
export function planStepCardDisplayName(_wireToolName: string): string {
  return "Plan step";
}

/** Turn `plan-deliver-a-report` into short readable title text (no hard max). */
export function humanizePlanSlug(slug: string): string {
  const s = slug.trim();
  if (!s) return "";
  const tail = s.replace(/^plan[-_]/i, "").trim();
  return tail.replace(/-/g, " ").replace(/\s+/g, " ").trim();
}

export function humanizeStepSlug(slug: string): string {
  return slug
    .trim()
    .replace(/_/g, " ")
    .replace(/-/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function readStringField(o: Record<string, unknown>, keys: string[]): string | undefined {
  for (const k of keys) {
    const v = o[k];
    if (typeof v === "string" && v.trim()) return v.trim();
  }
  return undefined;
}

/**
 * Prefer coordinator-authored description when present; otherwise humanize ids.
 * Intentionally avoids the generic 72-char clip used for other tools so plan titles stay readable.
 */
export function formatExecutionSessionToolUseDetail(input: unknown): string | null {
  if (input == null) return null;
  let o: Record<string, unknown>;
  try {
    if (typeof input === "string") {
      o = JSON.parse(input) as Record<string, unknown>;
    } else if (typeof input === "object" && !Array.isArray(input)) {
      o = input as Record<string, unknown>;
    } else {
      return null;
    }
  } catch {
    return null;
  }

  const description = readStringField(o, [
    "step_description",
    "stepDescription",
    "description",
    "title",
    "step_title",
    "stepTitle",
    "summary",
  ]);

  const stepRaw = readStringField(o, ["step_id", "stepId"]);
  const planRaw = readStringField(o, ["plan_id", "planId"]);

  const stepLine = stepRaw ? humanizeStepSlug(stepRaw) : "";
  const planLine = planRaw ? humanizePlanSlug(planRaw) : "";

  if (description) {
    const bits = [description];
    if (stepLine && !description.toLowerCase().includes(stepLine.toLowerCase())) {
      bits.push(`Step: ${stepLine}`);
    }
    return bits.join("\n");
  }

  if (stepLine && planLine) {
    return `${stepLine}\n${planLine}`;
  }
  if (stepLine) return stepLine;
  if (planLine) return planLine;
  return null;
}

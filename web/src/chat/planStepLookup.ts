import type { ContextPlanningTaskSnapshot, PlanningPlanView } from "../types/provenance";

/** Composite key for plan-local step identity (matches synthetic execution-session tool args). */
export function planStepLookupKey(planId: string, stepId: string): string {
  return `${planId}\0${stepId}`;
}

/**
 * Maps `(plan_id, step_id)` → provenance step description (`prov_label`), including superseded
 * plans from `planHistory` plus `currentPlan`.
 */
export function buildPlanStepDescriptionLookup(
  tasks: ContextPlanningTaskSnapshot[],
): Map<string, string> {
  const m = new Map<string, string>();
  for (const task of tasks) {
    const plans: PlanningPlanView[] = [];
    if (Array.isArray(task.planHistory) && task.planHistory.length > 0) {
      plans.push(...task.planHistory);
    }
    if (task.currentPlan) {
      plans.push(task.currentPlan);
    }
    for (const plan of plans) {
      const pid = typeof plan.plan_id === "string" ? plan.plan_id.trim() : "";
      if (!pid) continue;
      for (const step of plan.steps ?? []) {
        const sid = typeof step.step_id === "string" ? step.step_id.trim() : "";
        const desc = typeof step.description === "string" ? step.description.trim() : "";
        if (!sid || !desc) continue;
        m.set(planStepLookupKey(pid, sid), desc);
      }
    }
  }
  return m;
}

export function lookupPlanStepDescription(
  map: Map<string, string>,
  planId: string,
  stepId: string,
): string | undefined {
  const p = planId.trim();
  const s = stepId.trim();
  if (!p || !s) return undefined;
  return map.get(planStepLookupKey(p, s));
}

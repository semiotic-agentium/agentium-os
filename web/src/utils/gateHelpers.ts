// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

import type {
  ContextPlanningTaskSnapshot,
  GateDecision,
  ProvenanceRowBase,
} from "../types/provenance";

export type GatePosture = "ok" | "denied" | "ask" | "gated";

export function gateDecisionClass(decision: GateDecision | string | null | undefined): string {
  const d = (decision ?? "").toLowerCase();
  if (d === "deny") return "gate-decision-deny";
  if (d === "ask") return "gate-decision-ask";
  if (d === "pass_gated") return "gate-decision-gated";
  if (d === "pass") return "gate-decision-ok";
  return "gate-decision-unknown";
}

export function gateDecisionLabel(decision: GateDecision | string | null | undefined): string {
  const d = (decision ?? "").toLowerCase();
  if (d === "deny") return "denied";
  if (d === "ask") return "ask";
  if (d === "pass_gated") return "pass (gated)";
  if (d === "pass") return "pass";
  return decision ? String(decision) : "—";
}

export function gateReasonLabel(code: string | null | undefined): string {
  if (!code) return "—";
  return code.replace(/_/g, " ");
}

export function nodeStrengthClass(strength: number): string {
  if (strength >= 0.8) return "gate-strength-strong";
  if (strength >= 0.4) return "gate-strength-moderate";
  return "gate-strength-weak";
}

export function taskHasGateActivity(task: ContextPlanningTaskSnapshot): boolean {
  const g = task.gate;
  if (!g) return false;
  return (
    g.denyCount > 0 ||
    g.askCount > 0 ||
    g.passGatedCount > 0 ||
    g.passCount > 0 ||
    (g.gateEvents?.length ?? 0) > 0
  );
}

export function preventionRatioLabel(ratio: number | null | undefined): string {
  if (ratio == null || Number.isNaN(ratio)) return "—";
  return `${Math.round(ratio * 100)}%`;
}

export function taskGatePosture(task: ContextPlanningTaskSnapshot): GatePosture {
  const g = task.gate;
  if (!g) return "ok";
  if (g.denyCount > 0) return "denied";
  if (g.askCount > 0) return "ask";
  if (g.passGatedCount > 0) return "gated";
  return "ok";
}

export function gatePostureClass(posture: GatePosture): string {
  if (posture === "denied") return "gate-posture-denied";
  if (posture === "ask") return "gate-posture-ask";
  if (posture === "gated") return "gate-posture-gated";
  return "gate-posture-ok";
}

export function rowGateDecision(row: ProvenanceRowBase): Record<string, unknown> | null {
  const gate = row.gate;
  if (gate && typeof gate === "object") return gate as Record<string, unknown>;
  return null;
}

export function rowCitationIntegrity(row: ProvenanceRowBase): Record<string, unknown> | null {
  const cit = row.citation_integrity ?? row.citationIntegrity;
  if (cit && typeof cit === "object") return cit as Record<string, unknown>;
  return null;
}

export function integrityStatusClass(status: string): string {
  const s = status.toLowerCase();
  if (s === "resolved") return "integrity-resolved";
  if (s === "negated") return "integrity-negated";
  if (s === "unresolved") return "integrity-unresolved";
  return "integrity-unknown";
}

export const gateHelp = {
  gateTab:
    "Structural grounding before action. Shows deny/ask/pass_gated decisions and deficient parse nodes — not deliberate misparse detection.",
  preventionRatio:
    "Prevented errors divided by prevented plus friction denials. Higher means denials caught real plan changes.",
  citationIntegrity:
    "Whether each citation ref resolved in the provenance graph. No embedding similarity — resolved/unresolved/negated only.",
  dryRun: "Recording only — gate evaluated but did not block execution.",
};

export function planningIntentLabel(task: ContextPlanningTaskSnapshot): string {
  return task.currentIntent?.description ?? "No committed intent";
}

export function planningTaskTitle(task: ContextPlanningTaskSnapshot): string {
  const id = task.taskId;
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

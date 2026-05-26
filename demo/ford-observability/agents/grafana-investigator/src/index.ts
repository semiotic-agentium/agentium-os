/// <reference path="./baml-runtime.d.ts" />
import type { RunContext, SessionResult } from "./baml-runtime";

// Agentic investigator. The LLM picks which Grafana MCP tool to run each outer
// hop (via DecideInvestigationAction) and signals completion by returning null.
// AnalyzeGrafanaEvidence then reads the archived tool outputs from the transcript
// and emits a structured InvestigatorReport for the coordinator.

const MAX_OUTER_HOPS = 5;          // covers p95 incident + p95 baseline + loki + annotations + 1 reactive hop.
const MAX_INNER_FSM_STEPS = 6;     // per Grafana MCP tool session: Open → Send → PageRead/SearchRead* → Finish, with buffer for one extra paging step.

type Incident = {
  contextId?: string;
  status?: string;
  alertName?: string;
  service?: string;
  severity?: string;
  startsAt?: string;
  endsAt?: string;
  fingerprint?: string;
  dashboardURL?: string;
  panelURL?: string;
};

type InvestigationRequest = {
  task?: string;
  incident?: Incident;
  goal?: string;
};

function parseRequest(text: string): InvestigationRequest {
  try {
    const raw = JSON.parse(text) as unknown;
    return raw && typeof raw === "object" ? raw as InvestigationRequest : {};
  } catch {
    return { task: text };
  }
}

function defaultGoal(incident: Incident): string {
  const svc = incident.service ?? "checkout-api";
  const alert = incident.alertName ?? "GrafanaAlert";
  return `Diagnose Grafana alert ${alert} on ${svc}. Gather metric (baseline + incident), log, and synthetic trace evidence; identify the most plausible cause.`;
}

function incidentBrief(incident: Incident): string {
  return JSON.stringify({
    service: incident.service ?? "checkout-api",
    alert_name: incident.alertName ?? "GrafanaAlert",
    status: incident.status ?? "firing",
    severity: incident.severity ?? "warning",
    startsAt: incident.startsAt,
    endsAt: incident.endsAt,
    fingerprint: incident.fingerprint,
    dashboardURL: incident.dashboardURL,
    panelURL: incident.panelURL,
  });
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const req = parseRequest(ctx.text ?? "");
    const incident = req.incident ?? {};
    const brief = incidentBrief(incident);
    const goal = req.goal && req.goal.length > 0 ? req.goal : defaultGoal(incident);

    ctx.emit.message(`Investigating ${incident.alertName ?? "alert"} on ${incident.service ?? "service"}`);

    // Outer ReAct loop: each iteration runs one full tool-session FSM driven by
    // the LLM. The LLM returns `null` (string? arm) when it has enough evidence.
    for (let hop = 0; hop < MAX_OUTER_HOPS; hop += 1) {
      const result = await runGeneratedStepExecutor(
        "DecideInvestigationAction",
        { incident_brief: brief, investigation_goal: goal },
        { max_steps: MAX_INNER_FSM_STEPS },
      );

      if (result.outcome === "fatal") {
        ctx.emit.message(`investigation aborted: ${result.message}`);
        break;
      }
      if (result.outcome === "agent_correctable") {
        ctx.emit.message(`investigation correction [${result.recovery.code}]: ${result.recovery.mistake}`);
        break;
      }
      // outcome === "completed" — LLM returned `null` (string? arm) signalling done,
      // or finished a tool session normally. `last` is `null` only on the null arm.
      if (result.last === null || typeof result.last === "string") {
        break;
      }
    }

    ctx.emit.message("Synthesising evidence");
    const report = await AnalyzeGrafanaEvidence({ incident_json: brief });
    return { message: JSON.stringify(report) };
  },
});

export {};

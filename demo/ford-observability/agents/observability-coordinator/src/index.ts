/// <reference path="./baml-runtime.d.ts" />
//
// Multi-alert handling — Phase A (current): Grafana batches alerts with the
// same alertname/folder into one webhook payload. We iterate envelope.alerts[]
// and fan out one investigator + one synthesis + one Slack post per alert via
// Promise.all. Demo only fires HighLatency so N=1 in practice, but the loop
// stops alerts[1..] from being silently dropped if Grafana ever groups.
//
// Multi-alert handling — Phase B (deferred, gated on phase-2 alert rules
// landing: ServiceDown, HighErrorRate, DependencyTimeout):
//   - Switch SynthesizeIncidentReport to accept incidents_json: Incident[] and
//     packs_json: InvestigatorPack[] (BAML schema change + prompt rework).
//   - Fan out investigators in parallel, then call synthesis ONCE over the
//     full batch so the LLM can correlate across services (e.g. checkout-api
//     latency + payments-api ServiceDown → shared dependency).
//   - Emit a single grouped Slack post per webhook envelope instead of N.
//   - Needs correlation eval before enabling; not worth doing blind while only
//     one rule exists.
//
import type {
  HostDispatchAck,
  HostDispatchRequest,
  JsonObject,
  SessionResult,
  StructuredReply,
} from "./baml-runtime";

const INVESTIGATOR = { agent_package: "grafana-investigator", agent_instance_id: "default" };
const SLACK = { agent_package: "slack-notify", agent_instance_id: "default" };
const UI_BASE_URL = "http://localhost:18080";

type GrafanaAlert = {
  status: string;
  labels?: Record<string, unknown>;
  annotations?: Record<string, unknown>;
  startsAt?: string;
  endsAt?: string;
  fingerprint?: string;
  dashboardURL?: string;
  panelURL?: string;
};

type Incident = {
  contextId: string;
  messageId: string;
  status: string;
  alertName: string;
  service: string;
  severity: string;
  summary: string;
  description: string;
  startsAt: string;
  endsAt: string;
  fingerprint: string;
  dashboardURL: string;
  panelURL: string;
};

function s(value: unknown, fallback = ""): string {
  return typeof value === "string" && value.length > 0 ? value : fallback;
}

function extractAlerts(request: HostDispatchRequest): GrafanaAlert[] {
  const envelope = request.messages[0] as JsonObject | undefined;
  const alerts = Array.isArray(envelope?.alerts) ? envelope.alerts : [];
  const out: GrafanaAlert[] = alerts.filter((a) => !!a && typeof a === "object") as GrafanaAlert[];
  return out.length > 0 ? out : [{} as GrafanaAlert];
}

function incidentFromAlert(request: HostDispatchRequest, alert: GrafanaAlert): Incident {
  const envelope = request.messages[0] as JsonObject | undefined;
  const labels = alert.labels ?? {};
  const annotations = alert.annotations ?? {};
  const status = s(alert.status, s(envelope?.status, "firing"));
  return {
    contextId: s(request.context_id, "unknown-context"),
    messageId: s(request.message_id, "unknown-message"),
    status,
    alertName: s(labels.alertname, "GrafanaAlert"),
    service: s(labels.service, "checkout-api"),
    severity: s(labels.severity, "warning"),
    summary: s(annotations.summary, `${s(labels.service, "service")} alert ${status}`),
    description: s(annotations.description),
    startsAt: s(alert.startsAt, new Date().toISOString()),
    endsAt: s(alert.endsAt),
    fingerprint: s(alert.fingerprint),
    dashboardURL: s(alert.dashboardURL),
    panelURL: s(alert.panelURL),
  };
}

async function runSingleSendSession(
  toolName: string,
  openInput: Record<string, unknown>,
  sendInput: Record<string, unknown>,
): Promise<unknown> {
  let session = await openToolSession(toolName, openInput);
  try {
    await session.send(sendInput);
    for (let i = 0; i < 16; i += 1) {
      const next = await session.continue();
      if (next && typeof next === "object") {
        const obj = next as Record<string, unknown>;
        const status = typeof obj.status === "string" ? obj.status.toLowerCase() : "";
        if (status === "streaming") continue;
        if (status === "error") throw new Error(JSON.stringify(obj.error ?? obj));
        await session.finish();
        session = null as unknown as typeof session;
        return "output" in obj ? obj.output : next;
      }
      await session.finish();
      session = null as unknown as typeof session;
      return next;
    }
    throw new Error(`${toolName} exceeded continue budget`);
  } catch (error) {
    if (session) {
      try { await session.abort(error instanceof Error ? error.message : String(error)); } catch {}
    }
    throw error;
  }
}

async function a2a(target: { agent_package: string; agent_instance_id: string }, text: string): Promise<string> {
  const raw = await runSingleSendSession(
    "system/internal_a2a",
    { target },
    { parts: [{ text }] },
  );
  return typeof raw === "string" ? raw : JSON.stringify(raw);
}

function buildInvestigatorPrompt(incident: Incident): string {
  return JSON.stringify({
    goal: `Diagnose Grafana alert ${incident.alertName} on ${incident.service}. Pick the right Grafana MCP tools, gather metric (baseline + incident windows), log, and synthetic trace evidence, then return a structured InvestigatorReport JSON.`,
    incident,
  });
}

function linksFor(incident: Incident): Record<string, string> {
  const hasContext = incident.contextId && incident.contextId !== "unknown-context";
  const encoded = hasContext ? encodeURIComponent(incident.contextId) : "";
  return {
    grafana_dashboard: incident.dashboardURL || "not provided",
    grafana_panel: incident.panelURL || "not provided",
    agentium_dashboard: hasContext ? `${UI_BASE_URL}/?view=dashboard&contextId=${encoded}` : "context unavailable",
    transcript_api: hasContext ? `${UI_BASE_URL}/contexts/${encoded}/conversation-history` : "context unavailable",
    llm_provenance_api: hasContext ? `${UI_BASE_URL}/provenance/llm-calls?context_id=${encoded}` : "context unavailable",
    tool_provenance_api: hasContext ? `${UI_BASE_URL}/provenance/tool-calls?context_id=${encoded}` : "context unavailable",
  };
}

function stringifyReply(reply: StructuredReply | string): string {
  if (typeof reply === "string") return reply;
  const parts = Array.isArray(reply?.parts) ? reply.parts : [];
  const text = parts
    .map((p: unknown) => {
      if (!p || typeof p !== "object") return "";
      const obj = p as Record<string, unknown>;
      if (obj.type === "text" && typeof obj.text === "string") return obj.text;
      if (obj.type === "data" && typeof obj.data === "string") return obj.data;
      return "";
    })
    .filter((s: string) => s.length > 0)
    .join("\n\n");
  return text || JSON.stringify(reply);
}

async function buildReport(incident: Incident, investigatorPack: string): Promise<{ reply: StructuredReply; text: string }> {
  const reply = await SynthesizeIncidentReport({
    incident_json: JSON.stringify(incident),
    investigator_pack_json: investigatorPack,
    links_json: JSON.stringify(linksFor(incident)),
  });
  return { reply, text: stringifyReply(reply) };
}

__chat_register({
  run: async (_ctx): Promise<SessionResult> => {
    return { message: "observability-coordinator handles grafana.alert.v1 dispatch events. POST Grafana alerts to runner /webhooks/grafana." };
  },

  onDispatch: async (request: HostDispatchRequest): Promise<HostDispatchAck> => {
    if (request.message_type !== "grafana.alert.v1") {
      return { accepted: false, detail: `unsupported message_type=${request.message_type}` };
    }

    const alerts = extractAlerts(request);
    const incidents = alerts.map((a) => incidentFromAlert(request, a));

    const results = await Promise.all(incidents.map(async (incident) => {
      const isResolved = incident.status.toLowerCase() === "resolved";

      let investigatorPack = "";
      if (isResolved) {
        investigatorPack = JSON.stringify({
          service: incident.service,
          alert_name: incident.alertName,
          status: "resolved",
          likely_cause: "Alert resolved; no fresh investigation requested.",
          confidence: "unknown",
          metrics: [],
          log_samples: [],
          traces: [],
          open_questions: [
            "Confirm whether latency/error metrics have returned to baseline.",
          ],
          caveats: [
            "No new evidence gathered on resolution — see prior firing investigation in the same context.",
          ],
        });
      } else {
        try {
          investigatorPack = await a2a(INVESTIGATOR, buildInvestigatorPrompt(incident));
        } catch (error) {
          investigatorPack = JSON.stringify({
            service: incident.service,
            alert_name: incident.alertName,
            status: incident.status,
            likely_cause: "Investigation failed before evidence collection.",
            confidence: "unknown",
            metrics: [],
            log_samples: [],
            traces: [],
            open_questions: ["Re-run investigation manually."],
            caveats: [`Investigator A2A failed: ${error instanceof Error ? error.message : String(error)}`],
          });
        }
      }

      let reportText: string;
      try {
        const built = await buildReport(incident, investigatorPack);
        reportText = built.text;
      } catch (error) {
        reportText = `Report synthesis failed: ${error instanceof Error ? error.message : String(error)}\n\nInvestigator pack:\n${investigatorPack}`;
      }

      let slackError: string | null = null;
      try {
        await a2a(SLACK, JSON.stringify({
          context_id: incident.contextId,
          text: reportText,
        }));
      } catch (error) {
        slackError = error instanceof Error ? error.message : String(error);
      }

      const header = `incident ${incident.alertName}/${incident.service} ${incident.status} context_id=${incident.contextId}`;
      const detail = slackError
        ? `${header}; Slack notify failed: ${slackError}\n\n${reportText}`
        : `${header}\n\n${reportText}`;
      return detail;
    }));

    return {
      accepted: true,
      detail: results.join("\n\n---\n\n"),
    };
  },
});

export {};

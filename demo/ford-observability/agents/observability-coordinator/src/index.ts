/// <reference path="./baml-runtime.d.ts" />
import type { HostDispatchAck, HostDispatchRequest, JsonObject, SessionResult } from "./baml-runtime";

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

function firstAlert(request: HostDispatchRequest): GrafanaAlert {
  const envelope = request.messages[0] as JsonObject | undefined;
  const alerts = Array.isArray(envelope?.alerts) ? envelope.alerts : [];
  const alert = alerts[0];
  return alert && typeof alert === "object" ? (alert as GrafanaAlert) : {} as GrafanaAlert;
}

function incidentFromDispatch(request: HostDispatchRequest): Incident {
  const envelope = request.messages[0] as JsonObject | undefined;
  const alert = firstAlert(request);
  const labels = alert.labels ?? {};
  const annotations = alert.annotations ?? {};
  const status = s(alert.status, s(envelope?.status, "firing"));
  const contextId = s(request.context_id, "unknown-context");
  return {
    contextId,
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
    for (let i = 0; i < 12; i += 1) {
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
  return typeof raw === "string" ? raw : JSON.stringify(raw, null, 2);
}

function buildInvestigatorPrompt(incident: Incident): string {
  return JSON.stringify({
    task: "Investigate Grafana alert. Gather bounded Prometheus metrics, Loki logs, and synthetic trace annotations. Return structured findings with citations/archive refs when available.",
    incident,
    required_queries: {
      p95_latency: `histogram_quantile(0.95, sum(rate(demo_service_request_duration_seconds_bucket{service=\"${incident.service}\"}[2m])) by (le))`,
      error_rate: `sum(rate(demo_service_requests_total{service=\"${incident.service}\",status=~\"5..\"}[2m]))`,
      request_rate: `sum(rate(demo_service_requests_total{service=\"${incident.service}\"}[2m]))`,
      up: `up{job=\"${incident.service}\"}`,
      logs: `{service=\"${incident.service}\"} |= \"payments-api\"`,
      annotations: `tags=kind=trace service=${incident.service} limit<=20`,
    },
  }, null, 2);
}

function synthesizeReport(incident: Incident, findings: string): string {
  const dashboard = incident.contextId === "unknown-context"
    ? "context unavailable"
    : `${UI_BASE_URL}/?view=dashboard&contextId=${incident.contextId}`;
  return `🚨 Grafana Alert: ${incident.alertName} ${incident.status}\nService: ${incident.service}\nSeverity: ${incident.severity}\nStarted: ${incident.startsAt}\n\nSummary:\n- ${incident.summary}\n- Coordinator delegated metrics/logs/synthetic-trace investigation to grafana-investigator.\n- Canonical report lives in Agentium provenance under context_id ${incident.contextId}.\n\nInvestigator findings:\n${findings}\n\nLinks:\n- Grafana dashboard: ${incident.dashboardURL || "not provided"}\n- Grafana panel: ${incident.panelURL || "not provided"}\n- Agentium dashboard: ${dashboard}\n\nSuggested next actions:\n1. Check payments-api dependency latency and timeout logs.\n2. Confirm whether this matches demo injection.\n3. Watch resolution and verify p95 returns to baseline.`;
}

__chat_register({
  run: async (ctx): Promise<SessionResult> => {
    return { message: "observability-coordinator handles grafana.alert.v1 dispatch events. POST Grafana alerts to runner /webhooks/grafana." };
  },

  onDispatch: async (request: HostDispatchRequest): Promise<HostDispatchAck> => {
    if (request.message_type !== "grafana.alert.v1") {
      return { accepted: false, detail: `unsupported message_type=${request.message_type}` };
    }

    const incident = incidentFromDispatch(request);
    let findings = "Investigation failed before evidence collection.";
    try {
      findings = await a2a(INVESTIGATOR, buildInvestigatorPrompt(incident));
    } catch (error) {
      findings = `Investigation error: ${error instanceof Error ? error.message : String(error)}`;
    }

    const report = synthesizeReport(incident, findings);
    try {
      await a2a(SLACK, JSON.stringify({
        context_id: incident.contextId,
        text: report,
      }, null, 2));
    } catch (error) {
      return {
        accepted: true,
        detail: `incident ${incident.alertName}/${incident.service} investigated; Slack notify failed: ${error instanceof Error ? error.message : String(error)}`,
      };
    }

    return {
      accepted: true,
      detail: `incident ${incident.alertName}/${incident.service} investigated and Slack notification requested context_id=${incident.contextId}`,
    };
  },
});

export {};

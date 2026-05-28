/// <reference path="./baml-runtime.d.ts" />
//
// Multi-alert handling — Phase A (current): Grafana batches alerts with the
// same alertname/folder into one webhook payload ({ alerts: [...] }). The
// grafana-alerts producer emits one alert per dispatch (messages[0] is the
// alert object). We iterate the extracted alert list and fan out one
// investigator + one synthesis + one Slack post per alert via Promise.all.
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
// Agentium dashboard/provenance/transcript endpoints live on the runner's
// HTTP API. The runner is ClusterIP-only in the demo, so the URL is not
// stable for Slack readers; operators reach it via `kubectl port-forward
// svc/agentium-runner 18080:18080`. See demo/ford-observability/OPERATING.md.

// Wall-clock bounds per tool-call session inside runSingleSendSession.
// Two-tier deadline: A2A children (system/internal_a2a) wrap a full sub-agent
// run (outer ReAct loop + nested LLM/MCP calls + synthesis), so they need a
// much larger window than a single in-process MCP/host tool round-trip.
// Stall timer is unified — if no chunk arrives for SESSION_STALL_MS, the
// callee is treated as wedged regardless of remaining deadline.
const A2A_DEADLINE_MS = 10 * 60 * 1000;          // system/internal_a2a child agent
const TOOL_DEADLINE_MS = 2 * 60 * 1000;          // single MCP / host tool call
const SESSION_STALL_MS = 90 * 1000;              // no progress for this long → abort

type GrafanaAlert = {
  status: string;
  labels?: Record<string, unknown>;
  annotations?: Record<string, unknown>;
  startsAt?: string;
  endsAt?: string;
  fingerprint?: string;
  dashboardURL?: string;
  panelURL?: string;
  generatorURL?: string;
  valueString?: string;
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
  generatorURL: string;
  valueString: string;
  labels: Record<string, unknown>;
  annotations: Record<string, unknown>;
};

function s(value: unknown, fallback = ""): string {
  return typeof value === "string" && value.length > 0 ? value : fallback;
}

function isGrafanaAlertShape(value: unknown): value is GrafanaAlert {
  if (!value || typeof value !== "object") return false;
  const obj = value as Record<string, unknown>;
  return (
    typeof obj.status === "string"
    || typeof obj.startsAt === "string"
    || typeof obj.fingerprint === "string"
    || (obj.labels !== undefined && typeof obj.labels === "object" && obj.labels !== null)
    || (obj.annotations !== undefined && typeof obj.annotations === "object" && obj.annotations !== null)
  );
}

function extractAlerts(request: HostDispatchRequest): GrafanaAlert[] {
  const message = request.messages[0];
  if (!message || typeof message !== "object") {
    return [{} as GrafanaAlert];
  }

  // Full Grafana webhook envelope: { status, alerts: [...], groupLabels, ... }
  const envelope = message as JsonObject;
  if (Array.isArray(envelope.alerts)) {
    const out = envelope.alerts.filter((a) => !!a && typeof a === "object") as GrafanaAlert[];
    if (out.length > 0) return out;
  }

  // support/grafana-alerts producer: messages[0] IS the alert (one dispatch per alert).
  if (isGrafanaAlertShape(envelope)) {
    return [envelope];
  }

  return [{} as GrafanaAlert];
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
    generatorURL: s(alert.generatorURL),
    valueString: s(alert.valueString),
    labels,
    annotations,
  };
}

async function runSingleSendSession(
  toolName: string,
  openInput: Record<string, unknown>,
  sendInput: Record<string, unknown>,
): Promise<unknown> {
  let session = await openToolSession(toolName, openInput);
  const deadlineMs = toolName === "system/internal_a2a" ? A2A_DEADLINE_MS : TOOL_DEADLINE_MS;
  try {
    await session.send(sendInput);
    const startedAt = Date.now();
    let lastProgressAt = startedAt;
    for (;;) {
      const now = Date.now();
      if (now - startedAt > deadlineMs) {
        throw new Error(`${toolName} exceeded ${deadlineMs}ms deadline`);
      }
      if (now - lastProgressAt > SESSION_STALL_MS) {
        throw new Error(`${toolName} stalled (${SESSION_STALL_MS}ms with no progress)`);
      }
      const next = await session.continue();
      if (next && typeof next === "object") {
        const obj = next as Record<string, unknown>;
        const status = typeof obj.status === "string" ? obj.status.toLowerCase() : "";
        if (status === "streaming") {
          // Transport heartbeat — refresh stall clock but do not terminate.
          lastProgressAt = Date.now();
          continue;
        }
        if (status === "error") throw new Error(JSON.stringify(obj.error ?? obj));
        await session.finish();
        session = null as unknown as typeof session;
        return "output" in obj ? obj.output : next;
      }
      await session.finish();
      session = null as unknown as typeof session;
      return next;
    }
  } catch (error) {
    if (session) {
      try { await session.abort(error instanceof Error ? error.message : String(error)); } catch {}
    }
    throw error;
  }
}

function isInfrastructureNotice(text: string): boolean {
  const t = text.trim();
  return t.startsWith("Calling model:") || t.startsWith("Invoking tool:");
}

function isA2aStatusBlurb(text: string): boolean {
  const t = text.trim();
  if (t.length === 0 || t === "{}") return true;
  if (t.startsWith("Investigating ") || t === "Synthesising evidence") return true;
  if (t.startsWith("Posting Slack incident summary")) return true;
  return false;
}

function tryParseJsonObject(text: string): Record<string, unknown> | null {
  try {
    const value = JSON.parse(text) as unknown;
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? value as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

function looksLikeInvestigatorReport(value: Record<string, unknown>): boolean {
  return (
    typeof value.service === "string"
    && (
      typeof value.likely_cause === "string"
      || Array.isArray(value.metrics)
      || Array.isArray(value.log_samples)
    )
  );
}

function textPartsFromA2aOutput(raw: unknown): string[] {
  if (typeof raw === "string") {
    return raw.trim().length > 0 ? [raw] : [];
  }
  if (!raw || typeof raw !== "object") return [];
  const chunks = Array.isArray((raw as Record<string, unknown>).chunks)
    ? (raw as Record<string, unknown>).chunks as unknown[]
    : [];
  const texts: string[] = [];
  for (const chunk of chunks) {
    if (!chunk || typeof chunk !== "object") continue;
    const message = (chunk as Record<string, unknown>).message;
    if (!message || typeof message !== "object") continue;
    const parts = Array.isArray((message as Record<string, unknown>).parts)
      ? (message as Record<string, unknown>).parts as unknown[]
      : [];
    for (const part of parts) {
      if (!part || typeof part !== "object") continue;
      const text = (part as Record<string, unknown>).text;
      if (typeof text === "string" && text.trim().length > 0) {
        texts.push(text);
      }
    }
  }
  return texts;
}

/** Pull the delegated agent's substantive reply from an internal_a2a Done payload. */
function extractA2aFinalText(raw: unknown): string {
  if (typeof raw === "string") {
    const trimmed = raw.trim();
    return trimmed.length > 0 ? trimmed : raw;
  }

  const texts = textPartsFromA2aOutput(raw);
  const substantive = texts.filter((t) => !isInfrastructureNotice(t) && !isA2aStatusBlurb(t));

  for (let i = substantive.length - 1; i >= 0; i -= 1) {
    const parsed = tryParseJsonObject(substantive[i]);
    if (parsed && looksLikeInvestigatorReport(parsed)) {
      return substantive[i];
    }
  }

  for (let i = substantive.length - 1; i >= 0; i -= 1) {
    if (tryParseJsonObject(substantive[i])) {
      return substantive[i];
    }
  }

  for (let i = texts.length - 1; i >= 0; i -= 1) {
    const t = texts[i];
    if (!isInfrastructureNotice(t) && t.trim() !== "{}") {
      return t;
    }
  }

  return JSON.stringify(raw);
}

async function a2a(target: { agent_package: string; agent_instance_id: string }, text: string): Promise<string> {
  const raw = await runSingleSendSession(
    "system/internal_a2a",
    { target },
    { parts: [{ text }] },
  );
  return extractA2aFinalText(raw);
}

function buildInvestigatorPrompt(incident: Incident): string {
  return JSON.stringify({
    goal: `Diagnose Grafana alert ${incident.alertName} on ${incident.service}. Gather metric (baseline + incident windows) and Loki log evidence, then return a structured InvestigatorReport JSON. Do not gather or report trace evidence.`,
    incident,
  });
}

function linksFor(incident: Incident): Record<string, string> {
  return {
    grafana_dashboard: incident.dashboardURL || "not provided",
    grafana_panel: incident.panelURL || "not provided",
    context_id: incident.contextId && incident.contextId !== "unknown-context"
      ? incident.contextId
      : "context unavailable",
  };
}

type MetricFinding = {
  name: string;
  query: string;
  incident_peak: string;
  baseline: string;
  delta_summary: string;
};

type LogSample = { line: string; why: string };

type InvestigatorPack = {
  service: string;
  alert_name: string;
  status: string;
  likely_cause: string;
  confidence: string;
  metrics: MetricFinding[];
  log_samples: LogSample[];
  traces: [];
  open_questions: string[];
  caveats: string[];
};

function truncate(value: string, max: number): string {
  return value.length <= max ? value : `${value.slice(0, Math.max(0, max - 3))}...`;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}

function parseInvestigatorPack(text: string): { ok: true; pack: InvestigatorPack } | { ok: false; reason: string } {
  const obj = tryParseJsonObject(text);
  if (!obj) return { ok: false, reason: "investigator reply was not a JSON object" };

  const metrics = Array.isArray(obj.metrics) ? obj.metrics : [];
  const logSamples = Array.isArray(obj.log_samples) ? obj.log_samples : [];
  const pack: InvestigatorPack = {
    service: s(obj.service, "unknown-service"),
    alert_name: s(obj.alert_name, "GrafanaAlert"),
    status: s(obj.status, "unknown"),
    likely_cause: truncate(s(obj.likely_cause, "Cause not determined."), 300),
    confidence: s(obj.confidence, "unknown").toLowerCase(),
    metrics: metrics
      .filter((m): m is Record<string, unknown> => !!m && typeof m === "object" && !Array.isArray(m))
      .map((m) => ({
        name: truncate(s(m.name, "metric"), 80),
        query: truncate(s(m.query, "n/a"), 120),
        incident_peak: truncate(s(m.incident_peak, "n/a"), 40),
        baseline: truncate(s(m.baseline, "n/a"), 40),
        delta_summary: truncate(s(m.delta_summary, "n/a"), 180),
      })),
    log_samples: logSamples
      .filter((l): l is Record<string, unknown> => !!l && typeof l === "object" && !Array.isArray(l))
      .map((l) => ({
        line: truncate(s(l.line, ""), 200),
        why: truncate(s(l.why, "log evidence"), 120),
      }))
      .filter((l) => l.line.length > 0),
    traces: [],
    open_questions: stringArray(obj.open_questions).map((q) => truncate(q, 160)).slice(0, 5),
    caveats: stringArray(obj.caveats)
      .filter((c) => !c.toLowerCase().includes("grafana annotation"))
      .map((c) => truncate(c, 180)),
  };

  const evidenceText = JSON.stringify({ cause: pack.likely_cause, logs: pack.log_samples }).toLowerCase();
  if (evidenceText.includes("log_kind") || evidenceText.includes("payments-api")) {
    pack.caveats = ["Dependency span evidence comes from structured Loki logs, not Tempo/OTLP traces."];
  }
  if (!pack.open_questions.some((q) => q.toLowerCase().includes("demo injection"))) {
    pack.open_questions = pack.open_questions.length > 0 ? pack.open_questions : ["Confirm whether this matches the expected demo injection."];
  }

  if (!pack.service || !pack.alert_name || !pack.status) {
    return { ok: false, reason: "investigator report missing required identity fields" };
  }
  return { ok: true, pack };
}

function statusEmoji(status: string): string {
  const s = status.toLowerCase();
  if (s === "firing") return "🚨";
  if (s === "resolved") return "✅";
  return "⚠️";
}

function maybeUrlLine(label: string, value: string): string {
  return value.startsWith("http") ? `- ${label}: [link](${value})` : `- ${label}: ${value}`;
}

function buildDeterministicMarkdown(incident: Incident, pack: InvestigatorPack, links: Record<string, string>): string {
  const status = incident.status || pack.status;
  const resolved = status.toLowerCase() === "resolved";
  const title = `## ${incident.alertName || pack.alert_name} Alert - ${incident.service || pack.service} ${statusEmoji(status)}`;
  const lines: string[] = [title, ""];

  lines.push(`### ${resolved ? "Resolution summary" : "Summary"}`);
  if (pack.metrics.length > 0) {
    for (const metric of pack.metrics.slice(0, 4)) {
      lines.push(`- **${metric.name}:** ${metric.baseline} → ${metric.incident_peak}. ${metric.delta_summary}`);
    }
  } else if (resolved) {
    lines.push("- Alert resolved; no fresh evidence collected for resolution notification.");
  } else {
    lines.push("- Evidence incomplete: no metric findings were available.");
  }
  if (pack.confidence === "unknown") lines.push("- Confidence is unknown; evidence may be incomplete.");
  lines.push("");

  lines.push("### Likely cause");
  lines.push(`${pack.likely_cause} (${pack.confidence})`);
  lines.push("");

  lines.push("### Evidence");
  if (pack.metrics.length > 0) {
    for (const metric of pack.metrics.slice(0, 4)) {
      lines.push(`- **${metric.name}:** incident=${metric.incident_peak}, baseline=${metric.baseline}; query=${metric.query}`);
    }
  }
  for (const sample of pack.log_samples.slice(0, 2)) {
    lines.push(`- Log sample: \`${sample.line}\` (${sample.why})`);
  }
  if (pack.metrics.length === 0 && pack.log_samples.length === 0) {
    lines.push("- No metric or log samples were available in the investigator pack.");
  }
  for (const caveat of pack.caveats) lines.push(`- ${caveat}`);
  lines.push("");

  lines.push("### References");
  lines.push(maybeUrlLine("grafana_dashboard", links.grafana_dashboard));
  lines.push(maybeUrlLine("grafana_panel", links.grafana_panel));
  lines.push(`- context_id: \`${links.context_id}\``);
  lines.push("");

  lines.push("### Suggested next actions");
  const questions = pack.open_questions.length > 0 ? pack.open_questions : ["Confirm whether this matches the expected demo injection."];
  questions.slice(0, 5).forEach((q, i) => lines.push(`${i + 1}. ${q}`));

  return lines.join("\n");
}

function structuredTextReply(text: string): StructuredReply {
  return { parts: [{ type: "text", text }], citations: [] } as StructuredReply;
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
  const parsed = parseInvestigatorPack(investigatorPack);
  if (parsed.ok) {
    const text = buildDeterministicMarkdown(incident, parsed.pack, linksFor(incident));
    return { reply: structuredTextReply(text), text };
  }

  const fallbackPack = JSON.stringify({
    service: incident.service,
    alert_name: incident.alertName,
    status: incident.status,
    likely_cause: `Investigator returned unparseable data: ${parsed.reason}`,
    confidence: "unknown",
    metrics: [],
    log_samples: [],
    traces: [],
    open_questions: ["Re-run investigation manually."],
    caveats: [`Raw investigator reply: ${truncate(investigatorPack, 1000)}`],
  });

  const reply = await SynthesizeIncidentReport({
    incident_json: JSON.stringify(incident),
    investigator_pack_json: fallbackPack,
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

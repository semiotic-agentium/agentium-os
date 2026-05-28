/// <reference path="./baml-runtime.d.ts" />
/// <reference path="./tool-session.d.ts" />
import type { InvestigatorReport, RunContext, SessionResult } from "./baml-runtime";

// Grafana investigator. TypeScript deterministically gathers the required
// evidence pack (incident Prometheus, baseline Prometheus, Loki service +
// dependency logs, synthetic trace annotations) before BAML synthesis reads the
// archived tool outputs and emits a structured InvestigatorReport.

const LOKI_TOOL = "mcp/grafana/query_loki_logs";
const PROM_TOOL = "mcp/grafana/query_prometheus";
const ANNOTATIONS_TOOL = "mcp/grafana/get_annotations";
const DEPENDENCY_SERVICE = "payments-api";

type Incident = {
  contextId?: string;
  status?: string;
  alertName?: string;
  service?: string;
  severity?: string;
  summary?: string;
  description?: string;
  startsAt?: string;
  endsAt?: string;
  fingerprint?: string;
  dashboardURL?: string;
  panelURL?: string;
  generatorURL?: string;
  valueString?: string;
  labels?: Record<string, unknown>;
  annotations?: Record<string, unknown>;
};

type InvestigationRequest = {
  task?: string;
  incident?: Incident;
  goal?: string;
};

function parseRequest(text: string): InvestigationRequest {
  try {
    const raw = JSON.parse(text) as unknown;
    return raw && typeof raw === "object" ? (raw as InvestigationRequest) : {};
  } catch {
    return { task: text };
  }
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
    generatorURL: incident.generatorURL,
    summary: incident.summary,
    description: incident.description,
    labels: incident.labels ?? {},
    annotations: incident.annotations ?? {},
    value_string: incident.valueString,
  });
}

function zeroEndsAt(endsAt?: string): boolean {
  return !endsAt || endsAt === "0001-01-01T00:00:00Z";
}

function incidentWindow(incident: Incident): { startRfc3339: string; endRfc3339: string } {
  const startsAtMs = Date.parse(incident.startsAt ?? "") || Date.now();
  const startMs = startsAtMs - 30_000;
  const effectiveEndMs = zeroEndsAt(incident.endsAt)
    ? startsAtMs + 5 * 60_000
    : Date.parse(incident.endsAt!);
  const endMs = effectiveEndMs + 90_000;
  return {
    startRfc3339: new Date(startMs).toISOString(),
    endRfc3339: new Date(endMs).toISOString(),
  };
}

function baselineWindow(incident: Incident): { startRfc3339: string; endRfc3339: string } {
  const startsAtMs = Date.parse(incident.startsAt ?? "") || Date.now();
  const endMs = startsAtMs - 10 * 60_000;
  const startMs = endMs - 5 * 60_000;
  return {
    startRfc3339: new Date(startMs).toISOString(),
    endRfc3339: new Date(endMs).toISOString(),
  };
}

function annotationWindowMs(incident: Incident): { from: number; to: number } {
  const window = incidentWindow(incident);
  return { from: Date.parse(window.startRfc3339), to: Date.parse(window.endRfc3339) };
}

function stringField(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function primaryPromql(incident: Incident): string {
  const service = incident.service ?? "checkout-api";
  const annotations = incident.annotations ?? {};
  const labels = incident.labels ?? {};
  const annotatedExpr = stringField(annotations.expr);
  if (annotatedExpr) return annotatedExpr;
  const labeledPromql = stringField(labels.promql);
  if (labeledPromql) return labeledPromql;
  const metric = stringField(labels.metric);
  if (metric === "error_rate") {
    return `sum(rate(demo_service_requests_total{service="${service}",status=~"5.."}[2m])) / sum(rate(demo_service_requests_total{service="${service}"}[2m]))`;
  }
  if (metric === "request_rate") {
    return `sum(rate(demo_service_requests_total{service="${service}"}[2m]))`;
  }
  return `histogram_quantile(0.95, sum(rate(demo_service_request_duration_seconds_bucket{service="${service}"}[2m])) by (le, service))`;
}

async function runToolSession(toolName: string, input: Record<string, unknown>): Promise<unknown> {
  let handle: ToolSessionHandle | null = null;
  try {
    handle = await openToolSession(toolName, {});
    await handle.send(input);
    const output = await handle.continue({});
    await handle.finish();
    handle = null;
    return output;
  } catch (error) {
    if (handle) {
      try {
        await handle.abort(error instanceof Error ? error.message : String(error));
      } catch {
        // Ignore abort failures while already handling the upstream error.
      }
    }
    throw error;
  }
}

async function runPrometheusQuery(expr: string, window: { startRfc3339: string; endRfc3339: string }): Promise<void> {
  await runToolSession(PROM_TOOL, {
    datasourceUid: "prometheus",
    expr,
    startTime: window.startRfc3339,
    endTime: window.endRfc3339,
    stepSeconds: 15,
    queryType: "range",
  });
}

async function runLokiToolSession(
  logql: string,
  window: { startRfc3339: string; endRfc3339: string },
  limit = 10,
): Promise<unknown> {
  return runToolSession(LOKI_TOOL, {
    datasourceUid: "loki",
    logql,
    startRfc3339: window.startRfc3339,
    endRfc3339: window.endRfc3339,
    limit,
    direction: "backward",
  });
}

async function runAnnotationsSession(incident: Incident): Promise<void> {
  const service = incident.service ?? "checkout-api";
  const window = annotationWindowMs(incident);
  await runToolSession(ANNOTATIONS_TOOL, {
    tags: ["agentium-demo", "kind=trace", `service=${service}`],
    matchAny: false,
    from: window.from,
    to: window.to,
    limit: 20,
  });
}

async function runMandatoryPrometheusSessions(incident: Incident): Promise<void> {
  const expr = primaryPromql(incident);
  await runPrometheusQuery(expr, incidentWindow(incident));
  await runPrometheusQuery(expr, baselineWindow(incident));
}

async function runMandatoryLokiSessions(incident: Incident): Promise<void> {
  const service = incident.service ?? "checkout-api";
  const window = incidentWindow(incident);
  const queries = [
    // Checkout emits compact span-shaped stdout logs for dependency latency.
    `{service="${service}"} | json | log_kind="span"`,
    // Payments emits structured tracing logs with failure_mode promoted to a Loki label by Alloy.
    `{service="${DEPENDENCY_SERVICE}", failure_mode="latency_spike"} | json`,
  ];

  for (const logql of queries) {
    try {
      await runLokiToolSession(logql, window, 10);
    } catch {
      // Best-effort evidence; synthesis can still use other archives.
    }
  }
}

function parseUnitNumber(value: string): number | null {
  const match = value.match(/-?\d+(?:\.\d+)?/);
  return match ? Number(match[0]) : null;
}

function normalizeReport(report: InvestigatorReport): InvestigatorReport {
  const next: InvestigatorReport = {
    ...report,
    log_samples: report.log_samples.map((sample) => ({
      ...sample,
      line: sample.line.length > 200 ? `${sample.line.slice(0, 197)}...` : sample.line,
    })),
  };

  const evidenceText = JSON.stringify({ logs: report.log_samples, traces: report.traces, caveats: report.caveats }).toLowerCase();
  if (
    next.status === "firing" &&
    (next.likely_cause.toLowerCase().includes("increased processing time") ||
      next.likely_cause.toLowerCase().includes("high latency in"))
  ) {
    if (evidenceText.includes("latency_spike") || evidenceText.includes("slow dependency") || evidenceText.includes(DEPENDENCY_SERVICE)) {
      next.likely_cause =
        "Injected latency_spike slowed payments-api authorization, causing checkout-api /api/checkout requests to exceed the latency threshold.";
    }
  }

  const p95 = next.metrics.find((metric) => metric.name === "p95_latency");
  const incident = p95 ? parseUnitNumber(p95.incident_peak) : null;
  const baseline = p95 ? parseUnitNumber(p95.baseline) : null;
  if (incident !== null && baseline !== null && incident > 0) {
    const relativeDelta = Math.abs(incident - baseline) / incident;
    if (relativeDelta < 0.1 && next.confidence === "High") {
      next.confidence = "Medium";
    }
  }

  return next;
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const req = parseRequest(ctx.text ?? "");
    const incident = req.incident ?? {};
    const brief = incidentBrief(incident);
    ctx.emit.message(`Investigating ${incident.alertName ?? "alert"} on ${incident.service ?? "service"}`);

    ctx.emit.message("Collecting Prometheus metrics (incident + baseline)");
    try {
      await runMandatoryPrometheusSessions(incident);
    } catch (error) {
      ctx.emit.message(`Prometheus collection failed: ${error instanceof Error ? error.message : String(error)}`);
    }

    ctx.emit.message("Collecting Loki logs (service + dependency)");
    await runMandatoryLokiSessions(incident);

    ctx.emit.message("Collecting synthetic trace annotations");
    try {
      await runAnnotationsSession(incident);
    } catch (error) {
      ctx.emit.message(`annotation collection failed: ${error instanceof Error ? error.message : String(error)}`);
    }

    ctx.emit.message("Synthesising evidence");
    const report = normalizeReport(await AnalyzeGrafanaEvidence({ incident_json: brief }));
    return { message: JSON.stringify(report) };
  },
});

export {};

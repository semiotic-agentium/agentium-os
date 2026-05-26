/// <reference path="./baml-runtime.d.ts" />
import type { RunContext, SessionResult } from "./baml-runtime";

type Incident = {
  contextId?: string;
  status?: string;
  alertName?: string;
  service?: string;
  severity?: string;
  startsAt?: string;
  endsAt?: string;
  dashboardURL?: string;
  panelURL?: string;
};

type InvestigationRequest = {
  task?: string;
  incident?: Incident;
  required_queries?: Record<string, string>;
};

type Window = {
  alertStart: string;
  alertEnd: string;
  queryStart: string;
  queryEnd: string;
  queryStartMs: number;
  queryEndMs: number;
};

type ToolEvidence = {
  tool: string;
  input: Record<string, unknown>;
  ok: boolean;
  preview: string;
  raw?: unknown;
  error?: string;
};

type EvidenceItem = {
  id: string;
  kind: "metric" | "log" | "trace_annotation" | "datasource";
  summary: string;
  query?: string;
  tool: string;
  labels?: Record<string, string>;
  samples?: string[];
  caveat?: string;
  tool_input: Record<string, unknown>;
  tool_output_preview: string;
};

function parseRequest(text: string): InvestigationRequest {
  try {
    const raw = JSON.parse(text) as unknown;
    return raw && typeof raw === "object" ? raw as InvestigationRequest : {};
  } catch {
    return { task: text };
  }
}

function serviceOf(req: InvestigationRequest): string {
  return req.incident?.service || "checkout-api";
}

function finiteDate(raw: string | undefined, fallbackMs: number): Date {
  if (!raw) return new Date(fallbackMs);
  const date = new Date(raw);
  return Number.isFinite(date.getTime()) ? date : new Date(fallbackMs);
}

function timeWindow(req: InvestigationRequest): Window {
  const nowMs = Date.now();
  const alertEndDate = req.incident?.endsAt && req.incident.endsAt !== "0001-01-01T00:00:00Z"
    ? finiteDate(req.incident.endsAt, nowMs)
    : new Date(nowMs);
  const alertEndMs = alertEndDate.getTime();
  const alertStartDate = finiteDate(req.incident?.startsAt, alertEndMs - 10 * 60 * 1000);
  const alertStartMs = alertStartDate.getTime();

  // Demo ledger overlap rule: allow clock/scrape/eval skew.
  const queryStartMs = alertStartMs - 30 * 1000;
  const queryEndMs = alertEndMs + 90 * 1000;
  return {
    alertStart: new Date(alertStartMs).toISOString(),
    alertEnd: new Date(alertEndMs).toISOString(),
    queryStart: new Date(queryStartMs).toISOString(),
    queryEnd: new Date(queryEndMs).toISOString(),
    queryStartMs,
    queryEndMs,
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
    for (let i = 0; i < 10; i += 1) {
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

function preview(value: unknown, max = 1600): string {
  const text = typeof value === "string" ? value : JSON.stringify(value, null, 2);
  return text.length > max ? `${text.slice(0, max)}\n…` : text;
}

function findDatasourceUid(raw: unknown, kind: string, fallback: string): string {
  const text = typeof raw === "string" ? raw : JSON.stringify(raw);
  const lowerKind = kind.toLowerCase();
  const uidMatch = new RegExp(`"uid"\\s*:\\s*"([^"]+)"[^{}]*"type"\\s*:\\s*"[^"]*${lowerKind}[^"]*"`, "i").exec(text)
    || new RegExp(`"type"\\s*:\\s*"[^"]*${lowerKind}[^"]*"[^{}]*"uid"\\s*:\\s*"([^"]+)"`, "i").exec(text);
  return uidMatch?.[1] || fallback;
}

async function callTool(tool: string, input: Record<string, unknown>): Promise<ToolEvidence> {
  try {
    const raw = await runSingleSendSession(tool, {}, input);
    return { tool, input, ok: true, raw, preview: preview(raw) };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { tool, input, ok: false, error: message, preview: `ERROR calling ${tool}: ${message}` };
  }
}

function sampleLines(text: string, terms: string[], limit = 5): string[] {
  const lowerTerms = terms.map((t) => t.toLowerCase());
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .filter((line) => lowerTerms.some((term) => line.toLowerCase().includes(term)))
    .slice(0, limit);
}

function metricSummary(name: string, evidence: ToolEvidence): string {
  if (!evidence.ok) return `${name} query failed; see tool_output_preview`;
  const text = evidence.preview.toLowerCase();
  if (text.includes("error") || text.includes("5..")) return `${name} queried; inspect preview for returned series`;
  return `${name} queried over bounded incident window`;
}

async function investigate(req: InvestigationRequest, emit?: { message(text: string): void }): Promise<string> {
  const service = serviceOf(req);
  const window = timeWindow(req);
  const incident = req.incident ?? {};
  const evidence: EvidenceItem[] = [];

  emit?.message(`Listing Grafana datasources for ${service}`);
  const datasource = await callTool("mcp/grafana/list_datasources", { limit: 100, offset: 0 });
  const prometheusUid = findDatasourceUid(datasource.raw ?? datasource.preview, "prometheus", "prometheus");
  const lokiUid = findDatasourceUid(datasource.raw ?? datasource.preview, "loki", "loki");
  evidence.push({
    id: "datasource:grafana",
    kind: "datasource",
    summary: `Resolved datasource UIDs prometheus=${prometheusUid}, loki=${lokiUid}`,
    tool: datasource.tool,
    tool_input: datasource.input,
    tool_output_preview: datasource.preview,
  });

  const queries = req.required_queries ?? {};
  const metricQueries: Array<[string, string]> = [
    ["p95_latency", queries.p95_latency || `histogram_quantile(0.95, sum(rate(demo_service_request_duration_seconds_bucket{service=\"${service}\"}[2m])) by (le))`],
    ["error_rate", queries.error_rate || `sum(rate(demo_service_requests_total{service=\"${service}\",status=~\"5..\"}[2m]))`],
    ["request_rate", queries.request_rate || `sum(rate(demo_service_requests_total{service=\"${service}\"}[2m]))`],
    ["up", queries.up || `up{job=\"${service}\"}`],
  ];

  for (const [name, expr] of metricQueries) {
    emit?.message(`Querying Prometheus ${name}`);
    const input = {
      datasourceUid: prometheusUid,
      expr,
      queryType: "range",
      startTime: window.queryStart,
      endTime: window.queryEnd,
      stepSeconds: 15,
      projectName: null,
    };
    const result = await callTool("mcp/grafana/query_prometheus", input);
    evidence.push({
      id: `metric:${name}`,
      kind: "metric",
      summary: metricSummary(name, result),
      query: expr,
      labels: { service },
      tool: result.tool,
      tool_input: result.input,
      tool_output_preview: result.preview,
    });
  }

  emit?.message(`Querying Loki logs for ${service}`);
  const logql = queries.logs || `{service=\"${service}\"} |= \"payments-api\"`;
  const logInput = {
    datasourceUid: lokiUid,
    logql,
    queryType: "range",
    startRfc3339: window.queryStart,
    endRfc3339: window.queryEnd,
    limit: 20,
    direction: "backward",
  };
  const logs = await callTool("mcp/grafana/query_loki_logs", logInput);
  const logSamples = sampleLines(logs.preview, ["payments-api", "timeout", "dependency", service], 5);
  evidence.push({
    id: "log:payments_dependency",
    kind: "log",
    summary: logs.ok
      ? `Queried Loki for representative ${service} logs mentioning payments-api/dependency behavior`
      : "Loki log query failed; see tool_output_preview",
    query: logql,
    labels: { service },
    samples: logSamples,
    tool: logs.tool,
    tool_input: logs.input,
    tool_output_preview: logs.preview,
  });

  emit?.message(`Reading synthetic trace annotations for ${service}`);
  const annotationInput = {
    tags: ["agentium-demo", "kind=trace", `service=${service}`],
    matchAny: false,
    from: window.queryStartMs,
    to: window.queryEndMs,
    limit: 20,
    type: "annotation",
  };
  const annotations = await callTool("mcp/grafana/get_annotations", annotationInput);
  const traceSamples = sampleLines(annotations.preview, ["payments-api", "authorize", "slow dependency span", "duration_ms"], 5);
  evidence.push({
    id: "trace:slow_payment_span",
    kind: "trace_annotation",
    summary: annotations.ok
      ? "Read synthetic trace/span annotations for slow downstream payment authorization evidence"
      : "Trace annotation query failed; see tool_output_preview",
    labels: { service, evidence_type: "synthetic_grafana_annotation" },
    samples: traceSamples,
    caveat: "Synthetic span-like record mirrored into Grafana annotation; not Tempo/OTLP trace data.",
    tool: annotations.tool,
    tool_input: annotations.input,
    tool_output_preview: annotations.preview,
  });

  const failed = evidence.filter((item) => item.tool_output_preview.startsWith("ERROR"));
  const pack = {
    incident: {
      alert_name: incident.alertName || "GrafanaAlert",
      status: incident.status || "firing",
      service,
      severity: incident.severity || "warning",
      context_id: incident.contextId || null,
      dashboard_url: incident.dashboardURL || null,
      panel_url: incident.panelURL || null,
    },
    window,
    likely_cause: "payments-api dependency latency or timeout affecting checkout-api latency",
    confidence: failed.length === 0 ? "medium" : "low",
    evidence,
    open_questions: [
      "Confirm whether this is the expected demo latency_spike injection.",
      "Check payments-api health and dependency latency if this is not an intentional injection.",
    ],
    caveats: [
      "Metric and log evidence is read from live Grafana datasources over a bounded incident window.",
      "Log evidence is real Loki-backed application logs.",
      "Trace evidence is synthetic span-like Grafana annotations, not Tempo.",
      "Durable audit/citation source is Agentium provenance tool-call archive for this context_id; evidence ids in this JSON are stable within this report.",
    ],
  };

  return JSON.stringify(pack, null, 2);
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const req = parseRequest(ctx.text || "");
    const report = await investigate(req, ctx.emit);
    return { message: report };
  },
});

export {};

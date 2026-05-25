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

function timeWindow(req: InvestigationRequest): { start: string; end: string; startMs: number; endMs: number } {
  const endDate = req.incident?.endsAt && req.incident.endsAt !== "0001-01-01T00:00:00Z"
    ? new Date(req.incident.endsAt)
    : new Date();
  const endMs = Number.isFinite(endDate.getTime()) ? endDate.getTime() : Date.now();
  const startDate = req.incident?.startsAt ? new Date(req.incident.startsAt) : new Date(endMs - 10 * 60 * 1000);
  const startMs = Number.isFinite(startDate.getTime()) ? startDate.getTime() : endMs - 10 * 60 * 1000;
  return {
    start: new Date(startMs).toISOString(),
    end: new Date(endMs).toISOString(),
    startMs,
    endMs,
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

function pretty(value: unknown): string {
  const text = typeof value === "string" ? value : JSON.stringify(value, null, 2);
  return text.length > 1400 ? `${text.slice(0, 1400)}\n…` : text;
}

function findDatasourceUid(raw: unknown, kind: string, fallback: string): string {
  const text = typeof raw === "string" ? raw : JSON.stringify(raw);
  const lowerKind = kind.toLowerCase();
  const uidMatch = new RegExp(`"uid"\\s*:\\s*"([^"]+)"[^{}]*"type"\\s*:\\s*"[^"]*${lowerKind}[^"]*"`, "i").exec(text)
    || new RegExp(`"type"\\s*:\\s*"[^"]*${lowerKind}[^"]*"[^{}]*"uid"\\s*:\\s*"([^"]+)"`, "i").exec(text);
  return uidMatch?.[1] || fallback;
}

async function maybeCall(tool: string, input: Record<string, unknown>): Promise<string> {
  try {
    const out = await runSingleSendSession(tool, {}, input);
    return pretty(out);
  } catch (error) {
    return `ERROR calling ${tool}: ${error instanceof Error ? error.message : String(error)}`;
  }
}

async function investigate(req: InvestigationRequest, emit?: { message(text: string): void }): Promise<string> {
  const service = serviceOf(req);
  const { start, end, startMs, endMs } = timeWindow(req);
  const incident = req.incident ?? {};

  emit?.message(`Listing Grafana datasources for ${service}`);
  const datasourceRaw = await maybeCall("mcp/grafana/list_datasources", { limit: 100, offset: 0 });
  const prometheusUid = findDatasourceUid(datasourceRaw, "prometheus", "prometheus");
  const lokiUid = findDatasourceUid(datasourceRaw, "loki", "loki");

  const queries = req.required_queries ?? {};
  const promInputs: Array<[string, string]> = [
    ["p95_latency", queries.p95_latency || `histogram_quantile(0.95, sum(rate(demo_service_request_duration_seconds_bucket{service=\"${service}\"}[2m])) by (le))`],
    ["error_rate", queries.error_rate || `sum(rate(demo_service_requests_total{service=\"${service}\",status=~\"5..\"}[2m]))`],
    ["request_rate", queries.request_rate || `sum(rate(demo_service_requests_total{service=\"${service}\"}[2m]))`],
    ["up", queries.up || `up{job=\"${service}\"}`],
  ];

  const metricFindings: string[] = [];
  for (const [name, expr] of promInputs) {
    emit?.message(`Querying Prometheus ${name}`);
    const result = await maybeCall("mcp/grafana/query_prometheus", {
      datasourceUid: prometheusUid,
      expr,
      queryType: "range",
      startTime: start,
      endTime: end,
      stepSeconds: 15,
      projectName: null,
    });
    metricFindings.push(`### metric:${name}\nPromQL: ${expr}\n${result}`);
  }

  emit?.message(`Querying Loki logs for ${service}`);
  const logQuery = queries.logs || `{service=\"${service}\"} |= \"payments-api\"`;
  const logs = await maybeCall("mcp/grafana/query_loki_logs", {
    datasourceUid: lokiUid,
    logql: logQuery,
    queryType: "range",
    startRfc3339: start,
    endRfc3339: end,
    limit: 20,
    direction: "backward",
  });

  emit?.message(`Reading synthetic trace annotations for ${service}`);
  const annotations = await maybeCall("mcp/grafana/get_annotations", {
    tags: ["agentium-demo", "kind=trace", `service=${service}`],
    matchAny: false,
    from: startMs,
    to: endMs,
    limit: 20,
    type: "annotation",
  });

  return `Incident: ${incident.alertName || "GrafanaAlert"} ${incident.status || "firing"} service=${service} severity=${incident.severity || "warning"}\nWindow: ${start} .. ${end}\nDatasources: prometheus=${prometheusUid} loki=${lokiUid}\n\n## Datasource discovery\n${datasourceRaw}\n\n## Metrics evidence\n${metricFindings.join("\n\n")}\n\n## Loki log evidence\nLogQL: ${logQuery}\n${logs}\n\n## Synthetic trace annotation evidence\n${annotations}\n\nNotes:\n- Logs are Loki-backed application logs.\n- Trace evidence is synthetic span-like Grafana annotations, not Tempo.\n- All queries are bounded to alert window and limits where supported.`;
}

__chat_register({
  run: async (ctx: RunContext): Promise<SessionResult> => {
    const req = parseRequest(ctx.text || "");
    const report = await investigate(req, ctx.emit);
    return { message: report };
  },
});

export {};

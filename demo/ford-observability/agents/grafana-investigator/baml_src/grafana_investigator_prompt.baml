// Hand-written prompts for the Grafana investigator.
//
// TypeScript deterministically gathers Prometheus metrics and Loki log evidence.
// `AnalyzeGrafanaEvidence` reads the transcript archives and emits a structured
// InvestigatorReport for the coordinator to synthesize.
//
// Available tools (read-only, mounted by manifest.json):
//   mcp/grafana/list_datasources   — resolve Prometheus/Loki UIDs once.
//   mcp/grafana/query_prometheus   — PromQL: baseline window vs incident window.
//   mcp/grafana/query_loki_logs    — LogQL over the alert window.

enum InvestigatorConfidence {
  High   @alias("high")
  Medium @alias("medium")
  Low    @alias("low")
  Unknown @alias("unknown")
}

class InvestigatorMetricFinding {
  name string @description("Stable id: p95_latency | error_rate | request_rate | up.")
  query string @description("PromQL expression that was executed.")
  incident_peak string @description("Peak/representative value over the incident window with unit, e.g. \"1.82s\" or \"0.04 rps\". Use \"n/a\" when not derivable.")
  baseline string @description("Representative pre-incident value with unit, or \"n/a\".")
  delta_summary string @description("One short sentence comparing incident vs baseline.")
}

class InvestigatorLogSample {
  line string @description("One representative log line copied verbatim from the Loki response.")
  why string @description("Why this line is evidence — short phrase.")
}

class InvestigatorTraceFinding {
  operation string @description("Operation/route from the synthetic span annotation.")
  dependency string @description("Downstream dependency named in the span.")
  duration_ms int @description("Span duration in milliseconds.")
  note string @description("Short evidence note from the annotation.")
}

class InvestigatorReport {
  service string
  alert_name string
  status string @description("firing | resolved | unknown.")
  likely_cause string @description("One concrete sentence naming the most plausible cause grounded in transcript evidence.")
  confidence InvestigatorConfidence
  metrics InvestigatorMetricFinding[] @description("One row per metric query (p95_latency, error_rate, request_rate, up). Use \"n/a\" fields rather than dropping rows.")
  log_samples InvestigatorLogSample[] @description("Up to 5 representative log lines. Empty array allowed.")
  traces InvestigatorTraceFinding[] @description("Deprecated for this demo; always emit an empty array. Dependency span evidence belongs in log_samples.")
  open_questions string[] @description("1–3 short follow-up questions; always include whether this matches an expected demo injection.")
  caveats string[] @description("Caveats about evidence sources.")
}

/// Outer ReAct step. LLM picks one of the four MCP grafana tools to run, supplies
/// its input, then the generated step executor drives the full session FSM for
/// that tool. Return `null` to signal "no more tool calls; ready to synthesize".
///
/// Discipline:
/// - First hop should usually be `mcp/grafana/list_datasources` so subsequent
///   queries can reference real datasourceUid values from the transcript.
/// - Use baseline (>=1 min before startsAt) and incident windows for Prometheus
///   queries so the synthesizer can compute deltas.
/// - Bound every Loki query with limit and time range from the incident
///   envelope.
/// - Stop (`null`) once metric + log evidence is in the transcript or when
///   further queries would not change confidence.
function DecideInvestigationAction(
  incident_brief: string,
  investigation_goal: string
) -> McpGrafanaListDatasourcesSessionPlan | McpGrafanaQueryPrometheusSessionPlan | McpGrafanaQueryLokiLogsSessionPlan | string? {
  client DefaultClient
  prompt #"
    You are the Grafana incident investigator. Coordinator delegated this goal:
    {{ investigation_goal }}

    Incident envelope:
    {{ incident_brief }}

    You drive ONE tool session per outer hop. The runtime narrows your return
    type per hop in the FSM (Open / Send / PageRead / Finish / Abort). The four
    MCP tools available to you are listed at the top of this file's comment.

    Investigation shape (each outer step = one tool session; aim for ≤5 total):

    Pick the next tool by checking what is already in the transcript and what
    is still missing. Higher-signal evidence first, lower-signal last:

    1. Prometheus over the INCIDENT window for the alert's primary metric.
    2. Prometheus over a BASELINE window of the same metric (so a delta exists).
    3. Loki logs over the incident window.
    4. One reactive follow-up only when the prior archives suggest a sharper
       query (e.g. error_rate metric, narrower LogQL with a term you spotted).

    Primary metric selection (in priority order — pick the FIRST match):
      a. `annotations.expr` present → query that PromQL verbatim over the
         incident window. This is the alert's own rule expression and is the
         strongest signal of what fired.
      b. Else `labels.promql` present → query that PromQL verbatim.
      c. Else `labels.metric == "p95_latency_seconds"` → query
         histogram_quantile(0.95, sum(rate(demo_service_request_duration_seconds_bucket{service="<svc>"}[2m])) by (le, service))
      d. Else `labels.metric == "error_rate"` → query
         sum(rate(demo_service_requests_total{service="<svc>",status=~"5.."}[2m])) /
         sum(rate(demo_service_requests_total{service="<svc>"}[2m]))
      e. Else `labels.metric == "request_rate"` → query
         sum(rate(demo_service_requests_total{service="<svc>"}[2m]))
      f. Else fall back to (c) p95 latency as the demo default.
    NEVER use `up{}` as the primary metric — it answers liveness, not the alert.
    `up{}` is only acceptable as a tie-breaker reactive hop when other evidence
    is contradictory.

    Stop rule: return `null` ONLY when the transcript already contains a
    completed Send or PageRead archive from `mcp/grafana/query_loki_logs`
    AND you have metric evidence (incident + baseline Prometheus archives).
    FORBIDDEN to return `null` before Loki has been queried — the host will
    force a Loki session anyway, wasting a hop. This demo does not use trace
    annotations. Never re-issue an identical query. Never run a query whose
    answer is already visible in the transcript.

    Tool input recipes:
    - Use the well-known datasource UIDs `"prometheus"` and `"loki"` directly;
      do NOT call list_datasources unless the transcript shows those UIDs are
      invalid.
    - Incident window: `startsAt - 30s … endsAt + 90s`, `stepSeconds=15`.
      If endsAt is the zero placeholder (`"0001-01-01T00:00:00Z"` or empty),
      use `endsAt = startsAt + 5m`.
    - Baseline window: ≈10 minutes ending ≥1 minute before startsAt; same PromQL.
    - Loki: runtime already queries bounded service + dependency evidence:
      `{service="<svc>"} | json | log_kind="span"` and
      `{service="payments-api", failure_mode="latency_spike"} | json`,
      each with `limit≤10`, `direction="backward"`. Do not expand into label
      variants unless a visible archive says those exact labels are invalid.
    Within one session you may issue multiple PageRead/SearchRead ops to page
    deeper or grep for terms (`payments-api`, `timeout`, `5..`, `slow
    dependency`, route paths) before Finish — these do NOT count toward the
    outer-session budget. After each Send, PageRead `last_archive_ref` with
    `offset=0, limit=120` before Finish so lines materialise.

    Runtime phase policy:
    - SELECT / entry, status == "awaiting_open": emit Open for ONE tool.
    - ACT / status == "just_opened": emit Send with the minimal valid input
      derived from the incident envelope + transcript.
    - status == "done", last_step_op == "send": emit PageRead for
      last_archive_ref with offset=0, limit=120.
    - status == "done", last_step_op == "read": emit Finish.
    - last_completion == "INTERRUPTED": emit Abort when legal.

    Output: ONE JSON object matching the narrowed return type, OR the literal
    `null` to end the investigation. No prose, no markdown.

    Current FSM state from the host:
    - status: {{ session_context.status }}
    - last_step_op: {{ session_context.last_step_op }}
    - last_archive_ref: {{ session_context.last_archive_ref }}
    - last_output_header: {{ session_context.last_output_header }}

    {{ ctx.output_format }}

    {{ ctx.tags['conversation_transcript'] }}
  "#
}

/// Terminal synthesis. Reads all tool archives visible in the transcript and
/// emits a structured InvestigatorReport. No tool calls here.
function AnalyzeGrafanaEvidence(
  incident_json: string,
) -> InvestigatorReport {
  client DefaultClient
  prompt #"
    You are the Grafana investigator's synthesis step. Tool sessions for this
    investigation are already complete; their outputs are archived in the
    transcript below as @N entries. Do NOT propose further tool calls — extract
    findings from the visible archives only.

    Required behaviour:
    1. For each metric (p95_latency, error_rate, request_rate, up) extract a
       peak/representative value from the incident-window Prometheus archive
       and a baseline value from the baseline-window archive. The runtime
       deterministically runs the alert PromQL twice before synthesis: first
       incident window, then a baseline window ending 10 minutes before
       startsAt. Do not collapse these two archives into one value. Convert p95
       latency to seconds with two decimals (e.g. "1.82s"); rates as "X rps".
       Use "n/a" only when the archive truly lacks the value.
    2. HARD RULE — log_samples anti-hallucination:
       - Set `log_samples` to `[]` unless the transcript contains a visible
         `@N` archive from tool `mcp/grafana/query_loki_logs` (Send or PageRead).
       - When that archive exists, copy 2–5 lines verbatim from the Loki
         archive body. MCP Grafana responses use `data[].line`; raw Loki API
         responses use `data.result[].values[][1]`. NEVER invent timestamps, English summaries, alert text, or placeholder sentences
         (e.g. "High latency observed", "Request taking longer than usual").
       - If no Loki archive exists OR every Loki archive shows empty
         `data.result`, keep `log_samples: []` and add caveat
         "No Loki evidence collected".
    3. Always set `traces: []`. This demo does not use Tempo/OTLP traces or
       Grafana annotation traces. Loki log lines with `log_kind="span"` are
       log evidence only and belong in `log_samples`, never `traces[]`.
    4. likely_cause must name a concrete mechanism, not a tautology. FORBIDDEN:
       "high latency due to increased processing time", "p95 latency is high",
       or restating alert text. Prefer mechanisms visible in evidence, e.g.
       `failure_mode="latency_spike"`, `payments-api` dependency latency,
       or synthetic trace note `slow dependency span`. Good example:
       "Injected latency_spike slowed payments-api authorization, causing
       checkout-api /api/checkout requests to exceed the latency threshold."
    5. confidence: "high" only when ≥2 evidence kinds corroborate the cause;
       "medium" when 1 corroborates without contradictions; "low" when
       evidence is thin or a tool errored; "unknown" only as last resort.
    6. If Loki span logs are used, include exact caveat:
       "Dependency span evidence comes from structured Loki logs, not Tempo/OTLP traces."
    7. STRICT OUTPUT SHAPE. Emit ONLY the fields declared in InvestigatorReport:
       service, alert_name, status, likely_cause, confidence, metrics,
       log_samples, traces, open_questions, caveats. Do NOT include any extra
       fields such as `evidence`, `tool_output_preview`, `tool`, `tool_input`,
       `incident`, `window`, `query`, `labels`, `samples`, `id`, `kind`,
       `summary`. Do NOT echo raw tool outputs. Each log_samples.line MUST be
       ≤200 characters; truncate longer lines. Use "n/a" strings rather than
       embedding raw JSON. Any extra field is a hard failure.

    Incident envelope:
    {{ incident_json }}

    {% if ctx.tags['conversation_transcript'] %}
    Session history (raw tool archives visible as @N):
    {{ ctx.tags['conversation_transcript'] }}
    {% endif %}

    {{ ctx.output_format }}

    Return exactly one InvestigatorReport JSON object. No prose.
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

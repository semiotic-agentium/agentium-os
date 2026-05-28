// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually
//
// Host tools use a session FSM. Which ops are valid on a given model hop is defined only by the
// narrowed BAML return type for that function, not by duplicated prose. Step classes (Open/Send/…)
// and `*SendInput` / `*OpenInput` give field names and @description; see the shared prelude
// and each session-plan parent `prompt`: task + domain lines only; generated per-phase executors
// add archive prefix, optional tool_schema_prelude, narrowed union, transcript, and output binding.

// Shared standard planning types
class StandardAgentPlanStep {
  agent_package string @description("Exact agent_package from discovery results (e.g. 'extrospection-agent'). Copy verbatim — do not truncate or reformat.")
  agent_instance_id string @description("Agent instance ID, usually 'default'. Copy from discovery results.")
  sub_message string
}

class StandardStructuredPlan {
  intent_description string
  objective string
  plan_steps StandardAgentPlanStep[]
  citations string[]? @description("Optional refs (#N, @N) for observability; use @N:L when a step depends on specific tool lines; omit or [].")
}

/// Step row for LLM-authored plans committed via execution-session `submitPlan`.
/// `step_id` is a plan-local alias for `startStep` / `completeStep` — not a global provenance identifier;
/// the host derives canonical step entity ids from task scope + plan + this slug.
/// `depends_on` lists prerequisite `step_id` values (DAG). Omit or use [] when none; use `order` for display and tie-breaking.
class ProvenancePlanStep {
  step_id string @description("Plan-local slug (may be LLM-authored); not globally unique — host compounds with task/plan for canonical ids. Reuse in startStep/completeStep.")
  description string @description("What this step does.")
  order int @description("Ordinal for display and tie-breaking.")
  depends_on string[]? @description("Prerequisite step_ids (same alias namespace as step_id); omit or [] if none.")
}

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
  status string @description("FSM status: awaiting_open | just_opened | done")
  last_step_op string? @description("Previous FSM step op when known: open | send | read | finish | abort")
  last_step_status string? @description("Previous FSM step status when known: open | done | finished | aborted")
  last_archive_ref string? @description("Archive ref from the previous Send when available, e.g. @4")
  last_output_header string? @description("Compact archive header from the previous Send/Read when available")
  last_completion string? @description("Tool-specific terminal completion marker when available, e.g. DONE | INPUT_REQUIRED | INTERRUPTED")
}

/// Opaque JSON transport wrapper for host-managed tools and event payloads.
/// Generated BAML callers pass serialized JSON in `__baml_opaque_json`.
/// Direct host-side callers may still send arbitrary raw JSON.
class OpaqueJson {
  opaque_json string @alias("__baml_opaque_json") @description("Serialized JSON payload.")
}

// Structured reply types for synthesis functions
enum ReplyMediaType {
  TextPlain @alias("text/plain")
  TextMarkdown @alias("text/markdown")
  ApplicationJson @alias("application/json")
  TextCsv @alias("text/csv")
}

class TextPart {
  type "text"
  text string
}

class DataPart {
  type "data"
  data string @description("Serialised content (e.g. JSON string, CSV string).")
  media_type ReplyMediaType @description("MIME type of the data field.")
}

type ReplyPart = TextPart | DataPart

// Canonical synthesis + wire shape: same type in BAML and TypeScript (SessionResult.message, emitters).
class StructuredReply {
  parts ReplyPart[] @description("Ordered parts: start with TextPart (type \"text\") — set media_type TextMarkdown for rich answers (see TextPart); optionally append DataPart (type \"data\", media_type ApplicationJson) with a JSON string for machine-readable UI payloads.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Line-filtered archive read (emit op SearchRead).
class ArchiveSearchReadInput {
  archive_ref string @description("archive_ref names a Send output archive: set it to the exact @N shown on that Send line in the transcript (e.g. @1, @2). Do not use #N or any # prefix — # is for session history lines only and is invalid in this field. Do not invent @N. PageRead/SearchRead materialize line bodies from that handle; allowed with a real visible @N without Open or an active session.")
  grep string @description("Non-empty filter over rendered archive lines. SearchRead is for locating lines that match; use when you have a concrete term. Shape: substring or regex per host (e.g. name, -i error). For full contiguous slices without filtering, use PageRead.")
  offset int? @description("0-based offset over grep matches (after the filter). First window: offset=0; next: previous next_offset while matches remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("archive_ref names a Send output archive: set it to the exact @N shown on that Send line in the transcript (e.g. @1, @2). Do not use #N or any # prefix — # is for session history lines only and is invalid in this field. Do not invent @N. PageRead/SearchRead materialize line bodies from that handle; allowed with a real visible @N without Open or an active session.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). First window: offset=0; next: previous next_offset while lines remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Global line-filtered archive read step. Legal before Open, during a session, or after a Send.
class ArchiveSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

/// Global contiguous archive paging step. Legal before Open, during a session, or after a Send.
class ArchivePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

/// Terminal hop without a host tool session: answer from visible conversation archives only.
/// Do not emit when you need a fresh Open/Send — use ENTRY Open or ACTIVE Send instead.
class ReadOnlyFinishStep {
  op "ReadOnlyFinish"
  reply StructuredReply @description("User-visible answer; citations must include every @N archive line evidence relied on — omitting citations when grounding on archives is invalid.")
}
// Domain types generated from JSON schemas
class McpGrafanaGetAnnotationsInput {
  alertUid string? @description("Filter by alert UID")
  dashboardUid string? @description("Filter by dashboard UID")
  from int? @description("Epoch ms start time")
  limit int? @description("Max results default 100")
  matchAny bool? @description("If true, match any tag (OR). If false, match all tags (AND). Default: false")
  panelId int? @description("Filter by panel ID")
  tags string[]? @description("Filter by tags. Multiple tags allowed; use matchAny to control AND/OR logic")
  to int? @description("Epoch ms end time")
  type string? @description("annotation or alert")
  userId int? @description("Filter by creator user ID")
}

type McpGrafanaGetAnnotationsOutput = OpaqueJson

class McpGrafanaListDatasourcesInput {
  limit int? @description("Maximum number of datasources to return (max 100)")
  offset int? @description("Number of datasources to skip for pagination")
  type string? @description("The type of datasources to search for. For example, \'prometheus\', \'loki\', \'tempo\', etc...")
}

type McpGrafanaListDatasourcesOutput = OpaqueJson

class McpGrafanaQueryLokiLogsInput {
  datasourceUid string @description("The UID of the datasource to query")
  direction string? @description("Optionally, the direction of the query: \'forward\' (oldest first) or \'backward\' (newest first, default)")
  endRfc3339 string? @description("Optionally, the end time of the query in RFC3339 format")
  limit int? @description("Optionally, the maximum number of log lines to return (default max: 100, configurable by MCP server).")
  logql string @description("The LogQL query to execute against Loki. This can be a simple label matcher or a complex query with filters, parsers, and expressions. Supports full LogQL syntax including label matchers, filter operators, pattern expressions, and pipeline operations.")
  queryType string? @description("Query type: \'range\' (default) or \'instant\'. Instant queries return a single value at one point in time. Range queries return values over a time window. Use \'instant\' for metric queries when you want the current value.")
  startRfc3339 string? @description("Optionally, the start time of the query in RFC3339 format")
  stepSeconds int? @description("Resolution step in seconds for range metric queries. When running metric queries with queryType=\'range\', this controls the time resolution of the returned data points.")
}

type McpGrafanaQueryLokiLogsOutput = OpaqueJson

class McpGrafanaQueryPrometheusInput {
  datasourceUid string @description("The UID of the datasource to query")
  endTime string @description("The end time. Supported formats are RFC3339 or relative to now (e.g. \'now\', \'now-1.5h\', \'now-2h45m\'). Valid time units are \'ns\', \'us\' (or \'\u{b5}s\'), \'ms\', \'s\', \'m\', \'h\', \'d\'.")
  expr string @description("The PromQL expression to query")
  projectName string? @description("GCP project name to query (Cloud Monitoring datasources only). Overrides or substitutes the defaultProject configured on the datasource.")
  queryType string? @description("The type of query to use. Either \'range\' or \'instant\'")
  startTime string? @description("The start time. Required if queryType is \'range\', ignored if queryType is \'instant\' Supported formats are RFC3339 or relative to now (e.g. \'now\', \'now-1.5h\', \'now-2h45m\'). Valid time units are \'ns\', \'us\' (or \'\u{b5}s\'), \'ms\', \'s\', \'m\', \'h\', \'d\'.")
  stepSeconds int? @description("The time series step size in seconds. Required if queryType is \'range\', ignored if queryType is \'instant\'")
}

type McpGrafanaQueryPrometheusOutput = OpaqueJson

/// Tool card for mcp/grafana/list_datasources: metadata for polymorphic Open selection.
class McpGrafanaListDatasourcesToolCard {
  tool_name "mcp/grafana/list_datasources"
  description "List all configured datasources in Grafana. Use this to discover available datasources and their UIDs. Supports filtering by type and pagination."
  session_policy "Strict"
  input_summary "{ limit?: integer, offset?: integer, type?: string }"
  tags string[] @description("Tool tags: mcp, mcp:grafana")
}

class McpGrafanaListDatasourcesOpenStep {
  op "Open"
  tool_name "mcp/grafana/list_datasources" @description("Tool to open")
}

class McpGrafanaListDatasourcesSendStep {
  op "Send"
  input McpGrafanaListDatasourcesInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class McpGrafanaListDatasourcesSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class McpGrafanaListDatasourcesPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class McpGrafanaListDatasourcesFinishStep {
  op "Finish"
}

class McpGrafanaListDatasourcesAbortStep {
  op "Abort"
}

type McpGrafanaListDatasourcesSessionStep = McpGrafanaListDatasourcesOpenStep | McpGrafanaListDatasourcesSendStep | McpGrafanaListDatasourcesSearchReadStep | McpGrafanaListDatasourcesPageReadStep | McpGrafanaListDatasourcesFinishStep | McpGrafanaListDatasourcesAbortStep

class McpGrafanaListDatasourcesSessionPlan {
  step McpGrafanaListDatasourcesSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for mcp/grafana/query_prometheus: metadata for polymorphic Open selection.
class McpGrafanaQueryPrometheusToolCard {
  tool_name "mcp/grafana/query_prometheus"
  description "WORKFLOW: list_prometheus_metric_names -> list_prometheus_label_values -> query_prometheus. Query a PromQL-compatible datasource (Prometheus, Thanos, Mimir, Cloud Monitoring, etc.) using a PromQL expression. Supports instant queries (single point) and range queries (time range). Time: RFC3339 or relative expressions like 'now'\\, 'now-1h'."
  session_policy "Strict"
  input_summary "{ datasourceUid: string, endTime: string, expr: string, projectName?: string, queryType?: string, startTime?: string, stepSeconds?: integer }"
  tags string[] @description("Tool tags: mcp, mcp:grafana")
}

class McpGrafanaQueryPrometheusOpenStep {
  op "Open"
  tool_name "mcp/grafana/query_prometheus" @description("Tool to open")
}

class McpGrafanaQueryPrometheusSendStep {
  op "Send"
  input McpGrafanaQueryPrometheusInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class McpGrafanaQueryPrometheusSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class McpGrafanaQueryPrometheusPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class McpGrafanaQueryPrometheusFinishStep {
  op "Finish"
}

class McpGrafanaQueryPrometheusAbortStep {
  op "Abort"
}

type McpGrafanaQueryPrometheusSessionStep = McpGrafanaQueryPrometheusOpenStep | McpGrafanaQueryPrometheusSendStep | McpGrafanaQueryPrometheusSearchReadStep | McpGrafanaQueryPrometheusPageReadStep | McpGrafanaQueryPrometheusFinishStep | McpGrafanaQueryPrometheusAbortStep

class McpGrafanaQueryPrometheusSessionPlan {
  step McpGrafanaQueryPrometheusSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for mcp/grafana/query_loki_logs: metadata for polymorphic Open selection.
class McpGrafanaQueryLokiLogsToolCard {
  tool_name "mcp/grafana/query_loki_logs"
  description "Executes a LogQL query against a Loki datasource to retrieve log entries or metric values. Returns a list of results, each containing a timestamp, labels, and either a log line (`line`) or a numeric metric value (`value`). Defaults to the last hour, a limit of 10 entries, and 'backward' direction (newest first). Supports full LogQL syntax for log and metric queries (e.g., `{app=\"foo\"} |= \"error\"`, `rate({app=\"bar\"}[1m])`). Prefer using `query_loki_stats` first to check stream size and `list_loki_label_names` and `list_loki_label_values` to verify labels exist."
  session_policy "Strict"
  input_summary "{ datasourceUid: string, direction?: string, endRfc3339?: string, limit?: integer, logql: string, queryType?: string, startRfc3339?: string, stepSeconds?: integer }"
  tags string[] @description("Tool tags: mcp, mcp:grafana")
}

class McpGrafanaQueryLokiLogsOpenStep {
  op "Open"
  tool_name "mcp/grafana/query_loki_logs" @description("Tool to open")
}

class McpGrafanaQueryLokiLogsSendStep {
  op "Send"
  input McpGrafanaQueryLokiLogsInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class McpGrafanaQueryLokiLogsSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class McpGrafanaQueryLokiLogsPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class McpGrafanaQueryLokiLogsFinishStep {
  op "Finish"
}

class McpGrafanaQueryLokiLogsAbortStep {
  op "Abort"
}

type McpGrafanaQueryLokiLogsSessionStep = McpGrafanaQueryLokiLogsOpenStep | McpGrafanaQueryLokiLogsSendStep | McpGrafanaQueryLokiLogsSearchReadStep | McpGrafanaQueryLokiLogsPageReadStep | McpGrafanaQueryLokiLogsFinishStep | McpGrafanaQueryLokiLogsAbortStep

class McpGrafanaQueryLokiLogsSessionPlan {
  step McpGrafanaQueryLokiLogsSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for mcp/grafana/get_annotations: metadata for polymorphic Open selection.
class McpGrafanaGetAnnotationsToolCard {
  tool_name "mcp/grafana/get_annotations"
  description "Fetch Grafana annotations using filters such as dashboard UID, time range and tags."
  session_policy "Strict"
  input_summary "{ alertUid?: string, dashboardUid?: string, from?: integer, limit?: integer, matchAny?: boolean, panelId?: integer, tags?: string[], to?: integer, type?: string, userId?: integer }"
  tags string[] @description("Tool tags: mcp, mcp:grafana")
}

class McpGrafanaGetAnnotationsOpenStep {
  op "Open"
  tool_name "mcp/grafana/get_annotations" @description("Tool to open")
}

class McpGrafanaGetAnnotationsSendStep {
  op "Send"
  input McpGrafanaGetAnnotationsInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. Two namespaces (do not mix syntax): (1) History = transcript line numbers in this session; cite as #N only. Do not use @ on history refs; do not add :line to # (#N:L is invalid for history). (2) Archive = tool-result register; cite the handle from a prior Send as @N. Bare @N names the archive only, not text inside it; cite archive line content only as @N:L or @N:L1-L2 after PageRead or SearchRead returned those lines. Prefix ! on an entry for counter-evidence.")
}

class McpGrafanaGetAnnotationsSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("SearchRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a filtered window: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class McpGrafanaGetAnnotationsPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("PageRead step: archive_ref must be an @ handle (@1, @2, …) from a prior Send only — never #N or any # syntax (# indexes transcript lines, not archives). Read a contiguous window: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class McpGrafanaGetAnnotationsFinishStep {
  op "Finish"
}

class McpGrafanaGetAnnotationsAbortStep {
  op "Abort"
}

type McpGrafanaGetAnnotationsSessionStep = McpGrafanaGetAnnotationsOpenStep | McpGrafanaGetAnnotationsSendStep | McpGrafanaGetAnnotationsSearchReadStep | McpGrafanaGetAnnotationsPageReadStep | McpGrafanaGetAnnotationsFinishStep | McpGrafanaGetAnnotationsAbortStep

class McpGrafanaGetAnnotationsSessionPlan {
  step McpGrafanaGetAnnotationsSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. @N beside a prior Send is an archive handle for PageRead/SearchRead (archive_ref is @ only). Do not use #N as an archive handle — #N indexes transcript lines only. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: polymorphic session unions (from IR) ──

/// Polymorphic Open step for DecideInvestigationAction: selects a tool and opens a session.
/// See McpGrafanaGetAnnotationsToolCard, McpGrafanaListDatasourcesToolCard, McpGrafanaQueryLokiLogsToolCard, McpGrafanaQueryPrometheusToolCard for tool capabilities.
class DecideInvestigationActionOpenStep {
  op "Open"
  tool_name ("mcp/grafana/get_annotations" | "mcp/grafana/list_datasources" | "mcp/grafana/query_loki_logs" | "mcp/grafana/query_prometheus") @description("Which tool to open. See ToolCard classes for capabilities.")
}

/// Session plan for DecideInvestigationAction: polymorphic tool selection via Open.
class DecideInvestigationActionSessionPlan {
  step DecideInvestigationActionOpenStep @description("Select a tool and emit Open. After this, the session auto-narrows to the selected tool's step executor.")
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Entry step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function DecideInvestigationAction__entry(
  incident_brief: string,
  investigation_goal: string,
  session_context: SessionContext
) -> ArchivePageReadStep | ArchiveSearchReadStep | McpGrafanaGetAnnotationsOpenStep | McpGrafanaListDatasourcesOpenStep | McpGrafanaQueryLokiLogsOpenStep | McpGrafanaQueryPrometheusOpenStep | ReadOnlyFinishStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
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
4. Grafana trace annotations over the incident window.
5. One reactive follow-up only when the prior archives suggest a sharper
   query (e.g. error_rate metric, narrower LogQL with a term you spotted,
   a second annotation pull).

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
force a Loki session anyway, wasting a hop. Trace annotations are optional
for this demo; do not skip Loki because traces are missing. Never re-issue
an identical query. Never run a query whose answer is already visible in
the transcript.

Tool input recipes:
- Use the well-known datasource UIDs `"prometheus"` and `"loki"` directly;
  do NOT call list_datasources unless the transcript shows those UIDs are
  invalid.
- Incident window: `startsAt - 30s … endsAt + 90s`, `stepSeconds=15`.
  If endsAt is the zero placeholder (`"0001-01-01T00:00:00Z"` or empty),
  use `endsAt = startsAt + 5m`.
- Baseline window: ≈10 minutes ending ≥1 minute before startsAt; same PromQL.
- Loki: try `{service="<svc>"} | json` first, same incident window,
  `limit≤40`, `direction="backward"`. If the response has empty
  `data.result`, retry with `{app="<svc>"} | json` then
  `{service_name="<svc>"} | json` before giving up on logs. Label keys
  vary by deployment so do not Finish the investigation declaring "no
  logs" until all three label variants returned empty.
- Annotations: `tags=["agentium-demo", "kind=trace", "service=<svc>"]`,
  `matchAny=false`, `from`/`to` in epoch-ms, `limit≤20`.

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

Return exactly one JSON object of type `ArchivePageReadStep | ArchiveSearchReadStep | McpGrafanaGetAnnotationsOpenStep | McpGrafanaListDatasourcesOpenStep | McpGrafanaQueryLokiLogsOpenStep | McpGrafanaQueryPrometheusOpenStep | ReadOnlyFinishStep`.

Choose the matching object type.
- `ArchivePageReadStep` uses fields like `input`
- `ArchiveSearchReadStep` uses fields like `input`
- `McpGrafanaGetAnnotationsOpenStep`
- `McpGrafanaListDatasourcesOpenStep`
- `McpGrafanaQueryLokiLogsOpenStep`
- `McpGrafanaQueryPrometheusOpenStep`
- `ReadOnlyFinishStep` uses fields like `reply`
Do not mix fields from different object shapes.
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this entry return union excludes `Send`, `Finish`, and `Abort`.
"#

}

/// Active step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function DecideInvestigationAction__active__mcp_grafana_get_annotations(
  incident_brief: string,
  investigation_goal: string,
  session_context: SessionContext
) -> McpGrafanaGetAnnotationsSendStep | McpGrafanaGetAnnotationsSearchReadStep | McpGrafanaGetAnnotationsPageReadStep | McpGrafanaGetAnnotationsFinishStep | McpGrafanaGetAnnotationsAbortStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
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
4. Grafana trace annotations over the incident window.
5. One reactive follow-up only when the prior archives suggest a sharper
   query (e.g. error_rate metric, narrower LogQL with a term you spotted,
   a second annotation pull).

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
force a Loki session anyway, wasting a hop. Trace annotations are optional
for this demo; do not skip Loki because traces are missing. Never re-issue
an identical query. Never run a query whose answer is already visible in
the transcript.

Tool input recipes:
- Use the well-known datasource UIDs `"prometheus"` and `"loki"` directly;
  do NOT call list_datasources unless the transcript shows those UIDs are
  invalid.
- Incident window: `startsAt - 30s … endsAt + 90s`, `stepSeconds=15`.
  If endsAt is the zero placeholder (`"0001-01-01T00:00:00Z"` or empty),
  use `endsAt = startsAt + 5m`.
- Baseline window: ≈10 minutes ending ≥1 minute before startsAt; same PromQL.
- Loki: try `{service="<svc>"} | json` first, same incident window,
  `limit≤40`, `direction="backward"`. If the response has empty
  `data.result`, retry with `{app="<svc>"} | json` then
  `{service_name="<svc>"} | json` before giving up on logs. Label keys
  vary by deployment so do not Finish the investigation declaring "no
  logs" until all three label variants returned empty.
- Annotations: `tags=["agentium-demo", "kind=trace", "service=<svc>"]`,
  `matchAny=false`, `from`/`to` in epoch-ms, `limit≤20`.

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

Return exactly one JSON object of type `McpGrafanaGetAnnotationsAbortStep | McpGrafanaGetAnnotationsFinishStep | McpGrafanaGetAnnotationsPageReadStep | McpGrafanaGetAnnotationsSearchReadStep | McpGrafanaGetAnnotationsSendStep`.

Use `op` as the discriminator: "Abort" | "Finish" | "PageRead" | "SearchRead" | "Send".
Scalar leaves: use JSON primitives only where the schema names a string, number, enum, or boolean — never substitute an object or array for a scalar.
Calculator Send: set input.expression.operation to exactly one JSON string token: "+", "-", "*", "/", or Add|Subtract|Multiply|Divide — never an object or sentence.
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this active return union has no `Open` variant.
"#

}

/// Active step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function DecideInvestigationAction__active__mcp_grafana_list_datasources(
  incident_brief: string,
  investigation_goal: string,
  session_context: SessionContext
) -> McpGrafanaListDatasourcesSendStep | McpGrafanaListDatasourcesSearchReadStep | McpGrafanaListDatasourcesPageReadStep | McpGrafanaListDatasourcesFinishStep | McpGrafanaListDatasourcesAbortStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
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
4. Grafana trace annotations over the incident window.
5. One reactive follow-up only when the prior archives suggest a sharper
   query (e.g. error_rate metric, narrower LogQL with a term you spotted,
   a second annotation pull).

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
force a Loki session anyway, wasting a hop. Trace annotations are optional
for this demo; do not skip Loki because traces are missing. Never re-issue
an identical query. Never run a query whose answer is already visible in
the transcript.

Tool input recipes:
- Use the well-known datasource UIDs `"prometheus"` and `"loki"` directly;
  do NOT call list_datasources unless the transcript shows those UIDs are
  invalid.
- Incident window: `startsAt - 30s … endsAt + 90s`, `stepSeconds=15`.
  If endsAt is the zero placeholder (`"0001-01-01T00:00:00Z"` or empty),
  use `endsAt = startsAt + 5m`.
- Baseline window: ≈10 minutes ending ≥1 minute before startsAt; same PromQL.
- Loki: try `{service="<svc>"} | json` first, same incident window,
  `limit≤40`, `direction="backward"`. If the response has empty
  `data.result`, retry with `{app="<svc>"} | json` then
  `{service_name="<svc>"} | json` before giving up on logs. Label keys
  vary by deployment so do not Finish the investigation declaring "no
  logs" until all three label variants returned empty.
- Annotations: `tags=["agentium-demo", "kind=trace", "service=<svc>"]`,
  `matchAny=false`, `from`/`to` in epoch-ms, `limit≤20`.

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

Return exactly one JSON object of type `McpGrafanaListDatasourcesAbortStep | McpGrafanaListDatasourcesFinishStep | McpGrafanaListDatasourcesPageReadStep | McpGrafanaListDatasourcesSearchReadStep | McpGrafanaListDatasourcesSendStep`.

Use `op` as the discriminator: "Abort" | "Finish" | "PageRead" | "SearchRead" | "Send".
Scalar leaves: use JSON primitives only where the schema names a string, number, enum, or boolean — never substitute an object or array for a scalar.
Calculator Send: set input.expression.operation to exactly one JSON string token: "+", "-", "*", "/", or Add|Subtract|Multiply|Divide — never an object or sentence.
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this active return union has no `Open` variant.
"#

}

/// Active step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function DecideInvestigationAction__active__mcp_grafana_query_loki_logs(
  incident_brief: string,
  investigation_goal: string,
  session_context: SessionContext
) -> McpGrafanaQueryLokiLogsSendStep | McpGrafanaQueryLokiLogsSearchReadStep | McpGrafanaQueryLokiLogsPageReadStep | McpGrafanaQueryLokiLogsFinishStep | McpGrafanaQueryLokiLogsAbortStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
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
4. Grafana trace annotations over the incident window.
5. One reactive follow-up only when the prior archives suggest a sharper
   query (e.g. error_rate metric, narrower LogQL with a term you spotted,
   a second annotation pull).

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
force a Loki session anyway, wasting a hop. Trace annotations are optional
for this demo; do not skip Loki because traces are missing. Never re-issue
an identical query. Never run a query whose answer is already visible in
the transcript.

Tool input recipes:
- Use the well-known datasource UIDs `"prometheus"` and `"loki"` directly;
  do NOT call list_datasources unless the transcript shows those UIDs are
  invalid.
- Incident window: `startsAt - 30s … endsAt + 90s`, `stepSeconds=15`.
  If endsAt is the zero placeholder (`"0001-01-01T00:00:00Z"` or empty),
  use `endsAt = startsAt + 5m`.
- Baseline window: ≈10 minutes ending ≥1 minute before startsAt; same PromQL.
- Loki: try `{service="<svc>"} | json` first, same incident window,
  `limit≤40`, `direction="backward"`. If the response has empty
  `data.result`, retry with `{app="<svc>"} | json` then
  `{service_name="<svc>"} | json` before giving up on logs. Label keys
  vary by deployment so do not Finish the investigation declaring "no
  logs" until all three label variants returned empty.
- Annotations: `tags=["agentium-demo", "kind=trace", "service=<svc>"]`,
  `matchAny=false`, `from`/`to` in epoch-ms, `limit≤20`.

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

Return exactly one JSON object of type `McpGrafanaQueryLokiLogsAbortStep | McpGrafanaQueryLokiLogsFinishStep | McpGrafanaQueryLokiLogsPageReadStep | McpGrafanaQueryLokiLogsSearchReadStep | McpGrafanaQueryLokiLogsSendStep`.

Use `op` as the discriminator: "Abort" | "Finish" | "PageRead" | "SearchRead" | "Send".
Scalar leaves: use JSON primitives only where the schema names a string, number, enum, or boolean — never substitute an object or array for a scalar.
Calculator Send: set input.expression.operation to exactly one JSON string token: "+", "-", "*", "/", or Add|Subtract|Multiply|Divide — never an object or sentence.
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this active return union has no `Open` variant.
"#

}

/// Active step executor: stable prefix, transcript, task body, compact contract, and phase policy.
function DecideInvestigationAction__active__mcp_grafana_query_prometheus(
  incident_brief: string,
  investigation_goal: string,
  session_context: SessionContext
) -> McpGrafanaQueryPrometheusSendStep | McpGrafanaQueryPrometheusSearchReadStep | McpGrafanaQueryPrometheusPageReadStep | McpGrafanaQueryPrometheusFinishStep | McpGrafanaQueryPrometheusAbortStep {

  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
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
4. Grafana trace annotations over the incident window.
5. One reactive follow-up only when the prior archives suggest a sharper
   query (e.g. error_rate metric, narrower LogQL with a term you spotted,
   a second annotation pull).

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
force a Loki session anyway, wasting a hop. Trace annotations are optional
for this demo; do not skip Loki because traces are missing. Never re-issue
an identical query. Never run a query whose answer is already visible in
the transcript.

Tool input recipes:
- Use the well-known datasource UIDs `"prometheus"` and `"loki"` directly;
  do NOT call list_datasources unless the transcript shows those UIDs are
  invalid.
- Incident window: `startsAt - 30s … endsAt + 90s`, `stepSeconds=15`.
  If endsAt is the zero placeholder (`"0001-01-01T00:00:00Z"` or empty),
  use `endsAt = startsAt + 5m`.
- Baseline window: ≈10 minutes ending ≥1 minute before startsAt; same PromQL.
- Loki: try `{service="<svc>"} | json` first, same incident window,
  `limit≤40`, `direction="backward"`. If the response has empty
  `data.result`, retry with `{app="<svc>"} | json` then
  `{service_name="<svc>"} | json` before giving up on logs. Label keys
  vary by deployment so do not Finish the investigation declaring "no
  logs" until all three label variants returned empty.
- Annotations: `tags=["agentium-demo", "kind=trace", "service=<svc>"]`,
  `matchAny=false`, `from`/`to` in epoch-ms, `limit≤20`.

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

Return exactly one JSON object of type `McpGrafanaQueryPrometheusAbortStep | McpGrafanaQueryPrometheusFinishStep | McpGrafanaQueryPrometheusPageReadStep | McpGrafanaQueryPrometheusSearchReadStep | McpGrafanaQueryPrometheusSendStep`.

Use `op` as the discriminator: "Abort" | "Finish" | "PageRead" | "SearchRead" | "Send".
Scalar leaves: use JSON primitives only where the schema names a string, number, enum, or boolean — never substitute an object or array for a scalar.
Calculator Send: set input.expression.operation to exactly one JSON string token: "+", "-", "*", "/", or Add|Subtract|Multiply|Divide — never an object or sentence.
Do not add text before or after the JSON object.

Phase policy:
- Derived state rule: this active return union has no `Open` variant.
"#

}


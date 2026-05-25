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


// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually
//
// Host tools use a session FSM. Which ops are valid on a given model hop is defined only by the
// narrowed BAML return type for that function, not by duplicated prose. Step classes (Open/Send/…)
// and `*SendInput` / `*OpenInput` give field names and @description; see the shared prelude
// and each session-plan `prompt` order: stable types, task, transcript, then ctx.output_format last.

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
  archive_ref string @description("Which archive to open: an existing @N from conversation (Send output). This field names the handle; use PageRead/SearchRead to read the lines behind it. Do not invent refs. SearchRead/PageRead are legal with a real visible @N without Open or an active session.")
  grep string @description("Non-empty filter over rendered archive lines. SearchRead is for locating lines that match; use when you have a concrete term. Shape: substring or regex per host (e.g. name, -i error). For full contiguous slices without filtering, use PageRead.")
  offset int? @description("0-based offset over grep matches (after the filter). First window: offset=0; next: previous next_offset while matches remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("Which archive to open: an existing @N from conversation (Send output). This field names the handle; use PageRead/SearchRead to read the lines behind it. Do not invent refs. SearchRead/PageRead are legal with a real visible @N without Open or an active session.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). First window: offset=0; next: previous next_offset while lines remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Global line-filtered archive read step. Legal before Open, during a session, or after a Send.
class ArchiveSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

/// Global contiguous archive paging step. Legal before Open, during a session, or after a Send.
class ArchivePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}
// Domain types generated from JSON schemas
class AgentEventSubscriptionDto {
  schema_versions string[]
  source_kinds string[]
  source_keys string[]
  source_key_prefixes string[]
}

class AgentCardDto {
  name string
  version string
  content_hash string | null?
  repository_version int | null?
  agent_package string
  agent_instance_id string
  tools string[]
  description string | null?
  capabilities string[]
  tags string[]
  subscriptions AgentEventSubscriptionDto[]
}

enum HistoryContextV1Op {
  SearchRead
  PageRead
}

enum HistoryContextV1Status {
  Done
    @alias("done")
  Streaming
    @alias("streaming")
  Suspended
    @alias("suspended")
  Error
    @alias("error")
}

class HistoryContextV1 {
  hop int
  op HistoryContextV1Op
  status HistoryContextV1Status
  truncated bool
  cursor string | null?
  payload map<string, OpaqueJson> | null?
}

class DiscoverAgentsNextOutput {
  agents AgentCardDto[]
  done bool
  history_context HistoryContextV1 | null?
}

class DiscoverAgentsOpenInput {
  reason string | null?
}

class DiscoverAgentsSendInput {
  query string | null?
  required_capabilities string[] | null?
  required_schema_versions string[] | null?
  required_source_kinds string[] | null?
  limit int | null?
  offset int | null?
}

enum SessionReadResultDtoMode {
  RetrieveRef
    @alias("retrieve_ref")
}

enum SessionReadResultDtoProjection {
  Identity
    @alias("identity")
  Summary
    @alias("summary")
  Detail
    @alias("detail")
}

class ProvenanceArchiveSummaryDto {
  archive_ref string
  payload_count int
  payload_sources string[]
}

class ProvenancePayloadLlmCallDto {
  payload_ref string
  activity_ref string
  prompt_json string
  source string
}

class ProvenancePayloadLlmResultDto {
  payload_ref string
  activity_ref string
  result_json string
  source string
}

class ProvenancePayloadToolCallDto {
  payload_ref string
  activity_ref string
  tool_name string | null?
  phase string | null?
  args_json string
  source string
}

class ProvenancePayloadToolResultDto {
  payload_ref string
  activity_ref string
  result_json string
  source string
}

class ProvenanceArchiveRecordDto {
  archive_ref string
  payloads ProvenancePayloadLlmCallDto | ProvenancePayloadLlmResultDto | ProvenancePayloadToolCallDto | ProvenancePayloadToolResultDto[]
}

class SessionReadResultDto {
  mode SessionReadResultDtoMode
  ref_id string
  projection SessionReadResultDtoProjection
  refs string[]
  archive_summary ProvenanceArchiveSummaryDto | null?
  archive_record ProvenanceArchiveRecordDto | null?
}

class ProvenanceRetrievalBudgetDto {
  calls_used int
  calls_cap int
  bytes_used int
  bytes_cap int
  items_used int
  items_cap int
}

class ProvenanceQueryNextOutput {
  payload_json string | null?
  read_result SessionReadResultDto | null?
  retrieval_budget ProvenanceRetrievalBudgetDto | null?
  budget_exhausted bool | null?
  done bool
  history_context HistoryContextV1 | null?
}

class ProvenanceQueryOpenInput {
  reason string | null?
}

enum SessionReadEnvelopeMode {
  RetrieveRef
    @alias("retrieve_ref")
}

class SessionReadEnvelope {
  mode SessionReadEnvelopeMode | null?
  ref_id string
  projection string | null?
  budget_hint int | null?
}

class ProvenanceQuerySendInput {
  read SessionReadEnvelope | null?
  resource string
  context_id string | null?
  task_id string | null?
  agent_id string | null?
  provider string | null?
  model string | null?
  tool_name string | null?
  baml_prompt string | null?
  payload_text string | null?
  group_by string[] | null?
  sort_by string | null?
  sort_dir string | null?
  outcome string | null?
  page_size int | null?
  cursor string | null?
  top_k int | null?
}

/// Tool card for system/extrospection: metadata for polymorphic Open selection.
class SystemExtrospectionToolCard {
  tool_name "system/extrospection"
  description "Queries provenance rows across contexts and agents with compact token-aware output. Open session, send filters/grouping, then read() returns one result page."
  session_policy "Strict"
  input_summary "{ agent_id?: anyOf(string | null), baml_prompt?: anyOf(string | null), context_id?: anyOf(string | null), cursor?: anyOf(string | null), group_by?: anyOf(string[] | null), model?: anyOf(string | null), outcome?: anyOf(string | null), page_size?: anyOf(integer | null), payload_text?: anyOf(string | null), provider?: anyOf(string | null), read?: anyOf({ budget_hint?: anyOf(integer | null), mode?: anyOf(enum(\"retrieve_ref\") | null), projection?: anyOf(string | null), ref_id: string } | null), resource: string, sort_by?: anyOf(string | null), sort_dir?: anyOf(string | null), task_id?: anyOf(string | null), tool_name?: anyOf(string | null), top_k?: anyOf(integer | null) }"
  tags string[] @description("Tool tags: system, discovery")
}

class SystemExtrospectionOpenStep {
  op "Open"
  tool_name "system/extrospection" @description("Tool to open")
  initial_input ProvenanceQueryOpenInput? @description("Optional open payload.")
}

class SystemExtrospectionSendStep {
  op "Send"
  input ProvenanceQuerySendInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class SystemExtrospectionSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class SystemExtrospectionPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SystemExtrospectionFinishStep {
  op "Finish"
}

class SystemExtrospectionAbortStep {
  op "Abort"
}

type SystemExtrospectionSessionStep = SystemExtrospectionOpenStep | SystemExtrospectionSendStep | SystemExtrospectionSearchReadStep | SystemExtrospectionPageReadStep | SystemExtrospectionFinishStep | SystemExtrospectionAbortStep

class SystemExtrospectionSessionPlan {
  step SystemExtrospectionSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. A @N in history is a handle until you read it. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for system/discover_agents: metadata for polymorphic Open selection.
class SystemDiscoverAgentsToolCard {
  tool_name "system/discover_agents"
  description "Browse available agents. You can optionally filter by query, requiredCapabilities, or matching event subscriptions. Omit filters to list all agents."
  session_policy "Strict"
  input_summary "{ limit?: anyOf(integer | null), offset?: anyOf(integer | null), query?: anyOf(string | null), required_capabilities?: anyOf(string[] | null), required_schema_versions?: anyOf(string[] | null), required_source_kinds?: anyOf(string[] | null) }"
  tags string[] @description("Tool tags: system, discovery")
}

class SystemDiscoverAgentsOpenStep {
  op "Open"
  tool_name "system/discover_agents" @description("Tool to open")
  initial_input DiscoverAgentsOpenInput? @description("Optional open payload.")
}

class SystemDiscoverAgentsSendStep {
  op "Send"
  input DiscoverAgentsSendInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class SystemDiscoverAgentsSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class SystemDiscoverAgentsPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SystemDiscoverAgentsFinishStep {
  op "Finish"
}

class SystemDiscoverAgentsAbortStep {
  op "Abort"
}

type SystemDiscoverAgentsSessionStep = SystemDiscoverAgentsOpenStep | SystemDiscoverAgentsSendStep | SystemDiscoverAgentsSearchReadStep | SystemDiscoverAgentsPageReadStep | SystemDiscoverAgentsFinishStep | SystemDiscoverAgentsAbortStep

class SystemDiscoverAgentsSessionPlan {
  step SystemDiscoverAgentsSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. A @N in history is a handle until you read it. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function BuildExtrospectionPlan__select(
  intent: QueryIntent,
  selected_agent: SelectedAgent?,
  session_context: SessionContext
) -> SystemExtrospectionOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: system/extrospection.\n\nQuery provenance data matching the intent. Fill `ProvenanceQuerySendInput` (and other step payloads) from the field `@@description` on those types and this hop’s narrowed return union — do not duplicate the JSON field list in prose. Do not set `agentId` from `selected_agent` unless the user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send, SearchRead, PageRead, or Abort (system/extrospection).
function BuildExtrospectionPlan__act__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?,
  session_context: SessionContext
) -> SystemExtrospectionSendStep | SystemExtrospectionSearchReadStep | SystemExtrospectionPageReadStep | SystemExtrospectionAbortStep {

  client DefaultClient
  prompt #"[ACT] A system/extrospection session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.\n\nQuery provenance data matching the intent. Fill `ProvenanceQuerySendInput` (and other step payloads) from the field `@@description` on those types and this hop’s narrowed return union — do not duplicate the JSON field list in prose. Do not set `agentId` from `selected_agent` unless the user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (act — Send, Abort, or archive read): The JSON root must be exactly one Send, SearchRead, PageRead, or Abort step. op must be exactly the string Send, SearchRead, PageRead, or Abort — never Read (Read is not a legal op). Include input and citations per schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N).
"#

}

/// Phase: continue — SearchRead/PageRead, send again, finish, or abort system/extrospection.
function BuildExtrospectionPlan__continue__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?,
  session_context: SessionContext
) -> SystemExtrospectionSendStep | SystemExtrospectionSearchReadStep | SystemExtrospectionPageReadStep | SystemExtrospectionFinishStep | SystemExtrospectionAbortStep {

  client DefaultClient
  prompt #"[CONTINUE] system/extrospection result is archived.\nCheck session history:\n- See \"@N system/extrospection\" followed by numbered lines → content is inline; emit Finish\n- See \"@N system/extrospection\" with \"more lines\" indicator → emit SearchRead or PageRead to paginate\n- See \"@N system/extrospection\" with no content yet → emit SearchRead or PageRead with archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern\n\nQuery provenance data matching the intent. Fill `ProvenanceQuerySendInput` (and other step payloads) from the field `@@description` on those types and this hop’s narrowed return union — do not duplicate the JSON field list in prose. Do not set `agentId` from `selected_agent` unless the user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, SearchRead, PageRead, Finish, or Abort step. op must be Send, SearchRead, PageRead, Finish, or Abort — never Read. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N).
"#

}

/// Phase: select — open a tool session.
function GetDiscoverAgentsPlan__select(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscoverAgentsOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: system/discover_agents.\n\nDiscover available agents relevant to the user's request.
Send input shape and empty-payload cases are defined on `DiscoverAgentsSendInput` in the merged runtime — do not restate those fields here.

Context: {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send, SearchRead, PageRead, or Abort (system/discover_agents).
function GetDiscoverAgentsPlan__act__system_discover_agents(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscoverAgentsSendStep | SystemDiscoverAgentsSearchReadStep | SystemDiscoverAgentsPageReadStep | SystemDiscoverAgentsAbortStep {

  client DefaultClient
  prompt #"[ACT] A system/discover_agents session is open. Emit Send for a new query, or SearchRead/PageRead an existing @N archive from history when listing output is already fetched — do not re-Send the same discover_agents listing without trying SearchRead/PageRead pagination first.\n\nDISCOVERY INPUT RULE: For broad listing and routing, set only the `query` field (free-text match on name, package, description). Leave `required_capabilities`, `required_schema_versions`, and `required_source_kinds` null or omit them unless the user explicitly asked to filter by capability or event subscription. Do not add filters to narrow a vague intent — that often yields zero agents.

PAGING BEFORE RE-SEND: When a prior Send for this tool already archived an agents listing at @N, prefer SearchRead or PageRead on that @N (grep/limit/offset) to page or narrow — do not re-Send discover_agents with the same broad query as a substitute for pagination.

Discover available agents relevant to the user's request.
Send input shape and empty-payload cases are defined on `DiscoverAgentsSendInput` in the merged runtime — do not restate those fields here.

Context: {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (act — Send, Abort, or archive read): The JSON root must be exactly one Send, SearchRead, PageRead, or Abort step. op must be exactly the string Send, SearchRead, PageRead, or Abort — never Read (Read is not a legal op). Include input and citations per schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N).
"#

}

/// Phase: continue — SearchRead/PageRead, send again, finish, or abort system/discover_agents.
function GetDiscoverAgentsPlan__continue__system_discover_agents(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscoverAgentsSendStep | SystemDiscoverAgentsSearchReadStep | SystemDiscoverAgentsPageReadStep | SystemDiscoverAgentsFinishStep | SystemDiscoverAgentsAbortStep {

  client DefaultClient
  prompt #"[CONTINUE] system/discover_agents result is archived.\nCheck session history:\n- See \"@N system/discover_agents\" followed by numbered lines → content is inline; emit Finish\n- See \"@N system/discover_agents\" with \"more lines\" indicator → emit SearchRead or PageRead to paginate\n- See \"@N system/discover_agents\" with no content yet → emit SearchRead or PageRead with archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern\n\nDISCOVERY INPUT RULE: For broad listing and routing, set only the `query` field (free-text match on name, package, description). Leave `required_capabilities`, `required_schema_versions`, and `required_source_kinds` null or omit them unless the user explicitly asked to filter by capability or event subscription. Do not add filters to narrow a vague intent — that often yields zero agents.

PAGING BEFORE RE-SEND: When a prior Send for this tool already archived an agents listing at @N, prefer SearchRead or PageRead on that @N (grep/limit/offset) to page or narrow — do not re-Send discover_agents with the same broad query as a substitute for pagination.

Discover available agents relevant to the user's request.
Send input shape and empty-payload cases are defined on `DiscoverAgentsSendInput` in the merged runtime — do not restate those fields here.

Context: {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, SearchRead, PageRead, Finish, or Abort step. op must be Send, SearchRead, PageRead, Finish, or Abort — never Read. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N).
"#

}

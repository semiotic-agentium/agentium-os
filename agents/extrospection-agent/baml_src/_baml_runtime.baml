// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. SearchRead: line-filtered archive read (grep required). Page with offset/limit over matches.
// 4. PageRead: contiguous archive paging (no grep). Page with offset/limit over full rendered lines.
// 5. Finish: Closes the session gracefully
// 6. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - SearchRead/PageRead need a real @N from a Send for this tool (may be earlier in history). Large @N: SearchRead with grep first, then PageRead for detail — do not dump whole archives with PageRead alone.
// - Always Finish or Abort to close the session

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
  archive_ref string @description("Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every SearchRead/PageRead must use a real @N; do not invent refs.")
  grep string @description("Required non-empty line filter (substring or regex per host), e.g. deploy or -i deploy. Use SearchRead to locate lines; follow with PageRead when you need contiguous context.")
  offset int? @description("0-based offset counting lines that matched grep (after filter). Page with limit: first window offset=0; next offset = previous next_offset while matches remain.")
  limit int? @description("Max lines in this window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every SearchRead/PageRead must use a real @N; do not invent refs.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). Page with limit: first window offset=0; next offset = previous next_offset while lines remain.")
  limit int? @description("Max lines in this window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded).")
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
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SystemExtrospectionSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("archive_ref and grep required. Large body: small limit; page matches with offset. Do not use SearchRead when you need contiguous unfiltered lines — use PageRead.")
}

class SystemExtrospectionPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("archive_ref required; omit grep. Contiguous paging over rendered archive lines. Use after SearchRead when you need surrounding detail.")
}

class SystemExtrospectionFinishStep {
  op "Finish"
}

class SystemExtrospectionAbortStep {
  op "Abort"
}

type SystemExtrospectionSessionStep = SystemExtrospectionOpenStep | SystemExtrospectionSendStep | SystemExtrospectionSearchReadStep | SystemExtrospectionPageReadStep | SystemExtrospectionFinishStep | SystemExtrospectionAbortStep

class SystemExtrospectionSessionPlan {
  step SystemExtrospectionSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send for new work or SearchRead/PageRead @N when the archive exists; after Send (@N archived) → Finish, SearchRead, PageRead, or Send again.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for system/discover_agents: metadata for polymorphic Open selection.
class SystemDiscover_agentsToolCard {
  tool_name "system/discover_agents"
  description "Browse available agents. You can optionally filter by query, requiredCapabilities, or matching event subscriptions. Omit filters to list all agents."
  session_policy "Strict"
  input_summary "{ limit?: anyOf(integer | null), offset?: anyOf(integer | null), query?: anyOf(string | null), required_capabilities?: anyOf(string[] | null), required_schema_versions?: anyOf(string[] | null), required_source_kinds?: anyOf(string[] | null) }"
  tags string[] @description("Tool tags: system, discovery")
}

class SystemDiscover_agentsOpenStep {
  op "Open"
  tool_name "system/discover_agents" @description("Tool to open")
  initial_input DiscoverAgentsOpenInput? @description("Optional open payload.")
}

class SystemDiscover_agentsSendStep {
  op "Send"
  input DiscoverAgentsSendInput @description("Payload for this step.")
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SystemDiscover_agentsSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("archive_ref and grep required. Large body: small limit; page matches with offset. Do not use SearchRead when you need contiguous unfiltered lines — use PageRead.")
}

class SystemDiscover_agentsPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("archive_ref required; omit grep. Contiguous paging over rendered archive lines. Use after SearchRead when you need surrounding detail.")
}

class SystemDiscover_agentsFinishStep {
  op "Finish"
}

class SystemDiscover_agentsAbortStep {
  op "Abort"
}

type SystemDiscover_agentsSessionStep = SystemDiscover_agentsOpenStep | SystemDiscover_agentsSendStep | SystemDiscover_agentsSearchReadStep | SystemDiscover_agentsPageReadStep | SystemDiscover_agentsFinishStep | SystemDiscover_agentsAbortStep

class SystemDiscover_agentsSessionPlan {
  step SystemDiscover_agentsSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send for new work or SearchRead/PageRead @N when the archive exists; after Send (@N archived) → Finish, SearchRead, PageRead, or Send again.")
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
  prompt #"[OPEN] Open a session with: system/extrospection.\n\nQuery provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send, SearchRead, or PageRead (system/extrospection).
function BuildExtrospectionPlan__act__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?,
  session_context: SessionContext
) -> SystemExtrospectionSendStep | SystemExtrospectionSearchReadStep | SystemExtrospectionPageReadStep {

  client DefaultClient
  prompt #"[ACT] A system/extrospection session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.\n\nQuery provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (act — Send or archive read): The JSON root must be exactly one Send, SearchRead, or PageRead step. op must be exactly the string Send, SearchRead, or PageRead — never Read (Read is not a legal op). Include input and citations per schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N).
"#

}

/// Phase: continue — SearchRead/PageRead, send again, or finish system/extrospection.
function BuildExtrospectionPlan__continue__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?,
  session_context: SessionContext
) -> SystemExtrospectionSendStep | SystemExtrospectionSearchReadStep | SystemExtrospectionPageReadStep | SystemExtrospectionFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] system/extrospection result is archived.\nCheck session history:\n- See \"@N system/extrospection\" followed by numbered lines → content is inline; emit Finish\n- See \"@N system/extrospection\" with \"more lines\" indicator → emit SearchRead or PageRead to paginate\n- See \"@N system/extrospection\" with no content yet → emit SearchRead or PageRead with archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern\n\nQuery provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, SearchRead, PageRead, or Finish step. op must be Send, SearchRead, PageRead, or Finish — never Read. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N).
"#

}

/// Phase: select — open a tool session.
function GetDiscoverAgentsPlan__select(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscover_agentsOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: system/discover_agents.\n\nDiscover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send, SearchRead, or PageRead (system/discover_agents).
function GetDiscoverAgentsPlan__act__system_discover_agents(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscover_agentsSendStep | SystemDiscover_agentsSearchReadStep | SystemDiscover_agentsPageReadStep {

  client DefaultClient
  prompt #"[ACT] A system/discover_agents session is open. Emit Send for a new query, or SearchRead/PageRead an existing @N archive from history when listing output is already fetched — do not re-Send the same discover_agents listing without trying SearchRead/PageRead pagination first.\n\nDISCOVERY INPUT RULE: For broad listing and routing, set only the `query` field (free-text match on name, package, description). Leave `required_capabilities`, `required_schema_versions`, and `required_source_kinds` null or omit them unless the user explicitly asked to filter by capability or event subscription. Do not add filters to narrow a vague intent — that often yields zero agents.

PAGING BEFORE RE-SEND: When a prior Send for this tool already archived an agents listing at @N, prefer SearchRead or PageRead on that @N (grep/limit/offset) to page or narrow — do not re-Send discover_agents with the same broad query as a substitute for pagination.

Discover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (act — Send or archive read): The JSON root must be exactly one Send, SearchRead, or PageRead step. op must be exactly the string Send, SearchRead, or PageRead — never Read (Read is not a legal op). Include input and citations per schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N).
"#

}

/// Phase: continue — SearchRead/PageRead, send again, or finish system/discover_agents.
function GetDiscoverAgentsPlan__continue__system_discover_agents(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscover_agentsSendStep | SystemDiscover_agentsSearchReadStep | SystemDiscover_agentsPageReadStep | SystemDiscover_agentsFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] system/discover_agents result is archived.\nCheck session history:\n- See \"@N system/discover_agents\" followed by numbered lines → content is inline; emit Finish\n- See \"@N system/discover_agents\" with \"more lines\" indicator → emit SearchRead or PageRead to paginate\n- See \"@N system/discover_agents\" with no content yet → emit SearchRead or PageRead with archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern\n\nDISCOVERY INPUT RULE: For broad listing and routing, set only the `query` field (free-text match on name, package, description). Leave `required_capabilities`, `required_schema_versions`, and `required_source_kinds` null or omit them unless the user explicitly asked to filter by capability or event subscription. Do not add filters to narrow a vague intent — that often yields zero agents.

PAGING BEFORE RE-SEND: When a prior Send for this tool already archived an agents listing at @N, prefer SearchRead or PageRead on that @N (grep/limit/offset) to page or narrow — do not re-Send discover_agents with the same broad query as a substitute for pagination.

Discover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, SearchRead, PageRead, or Finish step. op must be Send, SearchRead, PageRead, or Finish — never Read. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N).
"#

}

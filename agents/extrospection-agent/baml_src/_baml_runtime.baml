// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. Read: Deref a prior Send result. Requires archive_ref (e.g. '@1'). Supports grep/paginate.
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - Read requires archive_ref from a prior Send. Use it to paginate or grep the output.
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
  citations string[]? @description("Optional checked refs (#N, @N) grounding this plan for observability; omit or use [] when none.")
}

class HistoryContext {
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload string?
}

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
  allowed_ops string[]? @description("Deprecated: per-phase step-executor functions narrow the output schema. Host may omit.")
  selected_tool string?
  status_token string?
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
  citations string[] @description("History refs this reply was derived from. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}

/// Archive deref input for Read steps.
/// archive_ref is required: use the @N ref from the Send result.
class ArchiveReadInput {
  archive_ref string @description("Required archive ref e.g. '@1'. Use the ref returned by the preceding Send step.")
  offset int? @description("Line offset for pagination (0-based). Omit to start from the beginning.")
  limit int? @description("Maximum lines to return. Omit for default page size.")
  grep string? @description("Optional grep pattern e.g. 'deploy' or '-i deploy' for case-insensitive.")
}
// Domain types generated from JSON schemas
class AgentCardDto {
  name string
  version string
  agentPackage string
  agentInstanceId string
  tools string[]
  description string?
  capabilities string[]
  subscriptions AgentEventSubscriptionDto[]?
}

class AgentEventSubscriptionDto {
  schemaVersions string[]?
  sourceKinds string[]?
  sourceKeys string[]?
  sourceKeyPrefixes string[]?
}

class DiscoverAgentsNextOutput {
  agents AgentCardDto[]
  done bool
  historyContext HistoryContextV1 | null?
}

class DiscoverAgentsOpenInput {
  reason string?
}

class DiscoverAgentsSendInput {
  query string? @description("Optional text filter over agent name, package, or description.")
  requiredCapabilities string[]? @description("Only return agents that declare every listed capability.")
  requiredSchemaVersions string[]? @description("Only return agents with a matching event subscription for any listed schema version.")
  requiredSourceKinds string[]? @description("Only return agents with a matching event subscription for any listed source kind.")
  limit int?
  offset int?
}

class HistoryContextV1 {
  hop int
  op string
  status string
  truncated bool?
  cursor string?
  payload string?
}

class ProvenanceArchivePayloadDtoVariant1 {
  payload_ref string
  activity_ref string
  prompt_json string
  source string
}

class ProvenanceArchivePayloadDtoVariant2 {
  payload_ref string
  activity_ref string
  result_json string
  source string
}

class ProvenanceArchivePayloadDtoVariant3 {
  payload_ref string
  activity_ref string
  tool_name string?
  phase string?
  args_json string
  source string
}

class ProvenanceArchivePayloadDtoVariant4 {
  payload_ref string
  activity_ref string
  result_json string
  source string
}

type ProvenanceArchivePayloadDto = ProvenanceArchivePayloadDtoVariant1 | ProvenanceArchivePayloadDtoVariant2 | ProvenanceArchivePayloadDtoVariant3 | ProvenanceArchivePayloadDtoVariant4

class ProvenanceArchiveRecordDto {
  archiveRef string
  payloads ProvenanceArchivePayloadDto[]
}

class ProvenanceArchiveSummaryDto {
  archiveRef string
  payloadCount int
  payloadSources string[]
}

class ProvenanceQueryNextOutput {
  payloadJson string?
  readResult SessionReadResultDto | null?
  retrievalBudget ProvenanceRetrievalBudgetDto | null?
  budgetExhausted bool?
  done bool
  historyContext HistoryContextV1 | null?
}

class ProvenanceQueryOpenInput {
  reason string?
}

class ProvenanceQuerySendInput {
  read SessionReadEnvelope | null? @description("Optional runtime-general explicit read request envelope.\nWhen set, the tool resolves it and ignores resource/filter fields.")
  resource string
  contextId string?
  taskId string?
  agentId string?
  provider string?
  model string?
  toolName string?
  bamlPrompt string?
  payloadText string?
  groupBy string[]?
  sortBy string?
  sortDir string?
  outcome string?
  pageSize int?
  cursor string?
  topK int?
}

enum ProvenanceReadProjectionDto {
  Identity
    @alias("identity")
  Summary
    @alias("summary")
  Detail
    @alias("detail")
}

class ProvenanceRetrievalBudgetDto {
  callsUsed int
  callsCap int
  bytesUsed int
  bytesCap int
  itemsUsed int
  itemsCap int
}

class SessionReadEnvelope {
  mode SessionReadMode | null?
  refId string
  projection string?
  budgetHint int?
}

enum SessionReadMode {
  RetrieveRef
    @alias("retrieve_ref")
}

class SessionReadResultDto {
  mode SessionReadMode
  refId string
  projection ProvenanceReadProjectionDto
  refs string[]
  archiveSummary ProvenanceArchiveSummaryDto | null?
  archiveRecord ProvenanceArchiveRecordDto | null?
}

/// Tool card for system/extrospection: metadata for polymorphic Open selection.
class SystemExtrospectionToolCard {
  tool_name "system/extrospection"
  description "Queries provenance rows across contexts and agents with compact token-aware output. Open session, send filters/grouping, then read() returns one result page."
  session_policy "Strict"
  input_summary "{ agentId?: any, bamlPrompt?: any, contextId?: any, cursor?: any, groupBy?: any, model?: any, outcome?: any, pageSize?: any, payloadText?: any, provider?: any, read?: any, resource: string, sortBy?: any, sortDir?: any, taskId?: any, toolName?: any, topK?: any }"
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
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
}

class SystemExtrospectionReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SystemExtrospectionFinishStep {
  op "Finish"
}

class SystemExtrospectionAbortStep {
  op "Abort"
}

type SystemExtrospectionSessionStep = SystemExtrospectionOpenStep | SystemExtrospectionSendStep | SystemExtrospectionReadStep | SystemExtrospectionFinishStep | SystemExtrospectionAbortStep

class SystemExtrospectionSessionPlan {
  step SystemExtrospectionSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again.")
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}

/// Tool card for system/discover_agents: metadata for polymorphic Open selection.
class SystemDiscover_agentsToolCard {
  tool_name "system/discover_agents"
  description "Browse available agents. You can optionally filter by query, requiredCapabilities, or matching event subscriptions. Omit filters to list all agents."
  session_policy "Strict"
  input_summary "{ limit?: any, offset?: any, query?: any, requiredCapabilities?: any, requiredSchemaVersions?: any, requiredSourceKinds?: any }"
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
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
}

class SystemDiscover_agentsReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SystemDiscover_agentsFinishStep {
  op "Finish"
}

class SystemDiscover_agentsAbortStep {
  op "Abort"
}

type SystemDiscover_agentsSessionStep = SystemDiscover_agentsOpenStep | SystemDiscover_agentsSendStep | SystemDiscover_agentsReadStep | SystemDiscover_agentsFinishStep | SystemDiscover_agentsAbortStep

class SystemDiscover_agentsSessionPlan {
  step SystemDiscover_agentsSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again.")
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
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

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to system/extrospection.
function BuildExtrospectionPlan__act__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?,
  session_context: SessionContext
) -> SystemExtrospectionSendStep {

  client DefaultClient
  prompt #"[SEND] A system/extrospection session is open. Emit Send with your query.\n\nQuery provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish system/extrospection.
function BuildExtrospectionPlan__continue__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?,
  session_context: SessionContext
) -> SystemExtrospectionSendStep | SystemExtrospectionReadStep | SystemExtrospectionFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] system/extrospection result is archived.\nCheck session history:\n- See \"@N system/extrospection\" followed by numbered lines → content is inline; emit Finish\n- See \"@N system/extrospection\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N system/extrospection\" with no content yet → emit Read archive_ref=\"@N\"\n\nQuery provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

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

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to system/discover_agents.
function GetDiscoverAgentsPlan__act__system_discover_agents(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscover_agentsSendStep {

  client DefaultClient
  prompt #"[SEND] A system/discover_agents session is open. Emit Send with your query.\n\nDiscover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish system/discover_agents.
function GetDiscoverAgentsPlan__continue__system_discover_agents(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscover_agentsSendStep | SystemDiscover_agentsReadStep | SystemDiscover_agentsFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] system/discover_agents result is archived.\nCheck session history:\n- See \"@N system/discover_agents\" followed by numbered lines → content is inline; emit Finish\n- See \"@N system/discover_agents\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N system/discover_agents\" with no content yet → emit Read archive_ref=\"@N\"\n\nDiscover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

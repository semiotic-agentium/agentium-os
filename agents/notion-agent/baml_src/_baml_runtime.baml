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

/// Step row for LLM-authored plans committed via execution-session `submitPlan`.
/// `step_id` is the session-scoped alias the agent passes to `startStep` / `completeStep`.
/// `depends_on` lists prerequisite `step_id` values (DAG). Omit or use [] when none; use `order` for display and tie-breaking.
class ProvenancePlanStep {
  step_id string @description("Short slug for this step; reuse in startStep/completeStep.")
  description string @description("What this step does.")
  order int @description("Ordinal for display and tie-breaking.")
  depends_on string[]? @description("Prerequisite step_ids; omit or [] if none (e.g. strictly linear — agent may still default chain in TS).")
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
// Domain types generated from #[derive(BamlType)]
/// Search Notion pages by keyword. Returns a list of page titles and IDs.
/// Does NOT return page content — use GetPageBlocks to read actual content.
class NotionSearchPagesInput {
  query string? @description("Search keyword. Omit to list all pages.")
  start_cursor string? @description("Pagination cursor from a previous search result.")
  page_size int? @description("Max pages to return per call (default 100). Use start_cursor to paginate.")
}

/// Fetch page METADATA only: title, URL, last-edited timestamp.
/// Returns NO page content (text, headings, bullets, etc.).
/// To read the actual content of a page, use GetPageBlocks with the same ID.
class NotionGetPageInput {
  page_id string @description("Notion page UUID from a SearchPages result, e.g. '238cff78-8181-80c8-8273-cc5fbdd8c7da'.")
}

/// Fetch the actual TEXT CONTENT of a Notion page as structured blocks
/// (headings, paragraphs, bullet lists, etc.).
/// block_id is the same UUID as the page id returned by SearchPages.
/// Use this — not GetPage — when you need to read what a page says.
class NotionGetPageBlocksInput {
  block_id string @description("Page UUID to read blocks from — same value as the page id from SearchPages.")
  start_cursor string? @description("Pagination cursor for large pages.")
  page_size int?
  raw_blocks BlockRenderMode? @description("Block render mode (omit for default enriched rendering).")
  max_depth int? @description("Max child block depth to expand (omit to expand all children).")
}

/// Controls how Notion blocks are rendered in the output.
/// Enriched (default): adds Notable Lines summaries and Missing Info hints for tables.
/// Raw: returns blocks verbatim without post-processing or hints.
enum BlockRenderMode {
  Raw
  Enriched
}

/// Notion tool input — three mutually exclusive operations:
/// SearchPages: find pages by keyword (returns titles+IDs, no content).
/// GetPage: fetch page metadata only (title, URL, timestamps — NOT content).
/// GetPageBlocks: read the actual text content of a page. Use this to understand what a page says.
type NotionInput = NotionSearchPagesInput | NotionGetPageInput | NotionGetPageBlocksInput

class NotionPageSummary {
  id string
  title string
  url string
  last_edited_time string?
}

class NotionBlockSummary {
  id string
  block_type string
  text string?
  has_children bool
}

class NotionSource {
  page_id string
  url string
}

class NotionOutput {
  pages NotionPageSummary[]
  blocks NotionBlockSummary[]
  next_cursor string?
  has_more bool
  sources NotionSource[]
  message string
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

class ConversationChunk {
  message ConversationMessage | null?
  task string?
  statusUpdate string?
  artifactUpdate string?
}

class ConversationMessage {
  role string?
  parts ConversationPart[]
}

class ConversationPart {
  text string?
  raw string?
  url string?
  filename string?
  mediaType string?
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

class DiscoverToolsNextOutput {
  tools ToolDiscoveryRecordDto[]
  done bool
  historyContext HistoryContextV1 | null?
}

class DiscoverToolsOpenInput {
  reason string?
}

class DiscoverToolsSendInput {
  query string? @description("Optional case-insensitive filter over tool name, bundle, or description.\nOmit or null to list all discoverable tools.")
  limit int? @description("Optional maximum number of tools to return for this Send query.\nUse a small value when you want compact, token-efficient results.")
}

class HistoryContextV1 {
  hop int
  op string
  status string
  truncated bool?
  cursor string?
  payload string?
}

enum InternalA2aCompletion {
  DONE
  INPUTREQUIRED
    @alias("INPUT_REQUIRED")
  FAILED
}

class InternalA2aNextOutput {
  chunks ConversationChunk[]
  completion InternalA2aCompletion | null? @description("Present when the delegated agent paused and needs more input.")
  historyContext HistoryContextV1 | null?
}

class InternalA2aOpenInput {
  target InternalA2aTarget
}

class InternalA2aSendInput {
  parts ConversationPart[] @description("Message parts. Provide at least one with a non-empty text field.")
}

class InternalA2aTarget {
  agent_package string
  agent_instance_id string
}

class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
  eventSources string[]?
}

/// Tool card for support/notion: metadata for polymorphic Open selection.
class SupportNotionToolCard {
  tool_name "support/notion"
  description "Read-only Notion access (search pages, get page, get page blocks)."
  session_policy "MultiSend"
  input_summary "{}"
  tags string[] @description("Tool tags: support, notion, read")
}

class SupportNotionOpenStep {
  op "Open"
  tool_name "support/notion" @description("Tool to open")
}

class SupportNotionSendStep {
  op "Send"
  input NotionInput @description("Payload for this step.")
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
}

class SupportNotionReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SupportNotionFinishStep {
  op "Finish"
}

class SupportNotionAbortStep {
  op "Abort"
}

type SupportNotionSessionStep = SupportNotionOpenStep | SupportNotionSendStep | SupportNotionReadStep | SupportNotionFinishStep | SupportNotionAbortStep

class SupportNotionSessionPlan {
  step SupportNotionSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again. Access: read.")
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

/// Tool card for system/discover_tools: metadata for polymorphic Open selection.
class SystemDiscover_toolsToolCard {
  tool_name "system/discover_tools"
  description "Searches registered tools by lexical rank. Open session, then send query/limit; read() returns results. Send can be called multiple times."
  session_policy "Strict"
  input_summary "{ limit?: any, query?: any }"
  tags string[] @description("Tool tags: system, discovery")
}

class SystemDiscover_toolsOpenStep {
  op "Open"
  tool_name "system/discover_tools" @description("Tool to open")
  initial_input DiscoverToolsOpenInput? @description("Optional open payload.")
}

class SystemDiscover_toolsSendStep {
  op "Send"
  input DiscoverToolsSendInput @description("Payload for this step.")
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
}

class SystemDiscover_toolsReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SystemDiscover_toolsFinishStep {
  op "Finish"
}

class SystemDiscover_toolsAbortStep {
  op "Abort"
}

type SystemDiscover_toolsSessionStep = SystemDiscover_toolsOpenStep | SystemDiscover_toolsSendStep | SystemDiscover_toolsReadStep | SystemDiscover_toolsFinishStep | SystemDiscover_toolsAbortStep

class SystemDiscover_toolsSessionPlan {
  step SystemDiscover_toolsSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again.")
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}

/// Tool card for system/internal_a2a: metadata for polymorphic Open selection.
class SystemInternal_a2aToolCard {
  tool_name "system/internal_a2a"
  description "Opens a conversational session to another agent. Send a message (text or parts), Read the response, then Send follow-ups or Finish. Multiple Send/Read rounds are allowed within one session — use this for multi-turn conversations with the delegated agent."
  session_policy "MultiSend"
  input_summary "{ parts: array }"
  tags string[] @description("Tool tags: system, a2a")
}

class SystemInternal_a2aOpenStep {
  op "Open"
  tool_name "system/internal_a2a" @description("Tool to open")
  initial_input InternalA2aOpenInput @description("Required open payload.")
}

class SystemInternal_a2aSendStep {
  op "Send"
  input InternalA2aSendInput @description("Conversational message payload. Set text to a non-empty string. Never null, never omit text.")
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
}

class SystemInternal_a2aReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SystemInternal_a2aFinishStep {
  op "Finish"
}

class SystemInternal_a2aAbortStep {
  op "Abort"
}

type SystemInternal_a2aSessionStep = SystemInternal_a2aOpenStep | SystemInternal_a2aSendStep | SystemInternal_a2aReadStep | SystemInternal_a2aFinishStep | SystemInternal_a2aAbortStep

class SystemInternal_a2aSessionPlan {
  step SystemInternal_a2aSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send (input.text MUST be non-empty); Send done → Finish or Read @N.")
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseNotionAction__select(
  goal: string,
  step_description: string,
  session_context: SessionContext
) -> SupportNotionOpenStep {

  client OpenRouterGPT
  prompt #"[OPEN] Open a session with: support/notion.\n\nYou are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to support/notion.
function ChooseNotionAction__act__support_notion(
  goal: string,
  step_description: string,
  session_context: SessionContext
) -> SupportNotionSendStep {

  client OpenRouterGPT
  prompt #"[SEND] A support/notion session is open. Emit Send with your query.\n\nYou are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish support/notion.
function ChooseNotionAction__continue__support_notion(
  goal: string,
  step_description: string,
  session_context: SessionContext
) -> SupportNotionSendStep | SupportNotionReadStep | SupportNotionFinishStep {

  client OpenRouterGPT
  prompt #"[CONTINUE] support/notion result is archived.\nCheck session history:\n- See \"@N support/notion\" followed by numbered lines → content is inline; emit Finish\n- See \"@N support/notion\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N support/notion\" with no content yet → emit Read archive_ref=\"@N\"\n\nYou are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. Read: Prior Send via archive_ref @N. Large body: set grep; small limit; offset to page.
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - Read needs a prior Send ref. Large @N: grep first, then limit/offset — do not dump whole archives.
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

/// Archive deref input for Read steps.
/// archive_ref is required: use the @N ref from the Send result.
class ArchiveReadInput {
  archive_ref string @description("Required: @N from the preceding Send (e.g. '@1'). Every Read must use a real @N; do not invent refs.")
  offset int? @description("0-based line offset after grep. Page with limit: first window offset=0; next offset += previous limit while lines or matches remain.")
  limit int? @description("Max lines in this Read window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded). Set grep before large windows.")
  grep string? @description("Large @N: set this first — line filter (substring or regex per host), e.g. deploy or -i deploy. Then page with offset/limit. Do not issue repeated wide Reads without a pattern.")
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

class HistoryContextV1 {
  hop int
  op string
  status string
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

class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
  event_sources string[]
}

class DiscoverToolsNextOutput {
  tools ToolDiscoveryRecordDto[]
  done bool
  history_context HistoryContextV1 | null?
}

class DiscoverToolsOpenInput {
  reason string | null?
}

class DiscoverToolsSendInput {
  query string | null?
  limit int | null?
}

class ConversationPart {
  text string | null?
  raw string | null?
  url string | null?
  filename string | null?
  media_type string | null?
}

class ConversationMessage {
  role string | null?
  parts ConversationPart[]
}

class ConversationChunk {
  message ConversationMessage | null?
  task string | null?
  status_update string | null?
  artifact_update string | null?
}

class InternalA2aNextOutput {
  chunks ConversationChunk[]
  completion string | null?
  history_context HistoryContextV1 | null?
}

class InternalA2aTarget {
  agent_package string
  agent_instance_id string
}

class InternalA2aOpenInput {
  target InternalA2aTarget
}

class InternalA2aSendInput {
  parts ConversationPart[]
}

/// Tool card for support/notion: metadata for polymorphic Open selection.
class SupportNotionToolCard {
  tool_name "support/notion"
  description "Read-only Notion access (search pages, get page, get page blocks)."
  session_policy "Strict"
  input_summary "anyOf({ page_size?: anyOf(integer | null), query?: anyOf(string | null), start_cursor?: anyOf(string | null) } | { page_id: string } | { block_id: string, max_depth?: anyOf(integer | null), page_size?: anyOf(integer | null), raw_blocks?: anyOf(enum(\"Enriched\" | \"Raw\") | null), start_cursor?: anyOf(string | null) })"
  tags string[] @description("Tool tags: support, notion, read")
}

class SupportNotionOpenStep {
  op "Open"
  tool_name "support/notion" @description("Tool to open")
}

class SupportNotionSendStep {
  op "Send"
  input NotionInput @description("Payload for this step.")
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SupportNotionReadStep {
  op "Read"
  input ArchiveReadInput @description("Prior Send archive: archive_ref required. Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.")
}

class SupportNotionFinishStep {
  op "Finish"
}

class SupportNotionAbortStep {
  op "Abort"
}

type SupportNotionSessionStep = SupportNotionOpenStep | SupportNotionSendStep | SupportNotionReadStep | SupportNotionFinishStep | SupportNotionAbortStep

class SupportNotionSessionPlan {
  step SupportNotionSessionStep @description("Emit one FSM step. From history: no session → Open; open, no Send → Send; after Send (@N archived) → Finish, Read @N (large body: set grep, small limit, offset), or Send again. Access: read.")
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

class SystemDiscover_agentsReadStep {
  op "Read"
  input ArchiveReadInput @description("Prior Send archive: archive_ref required. Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.")
}

class SystemDiscover_agentsFinishStep {
  op "Finish"
}

class SystemDiscover_agentsAbortStep {
  op "Abort"
}

type SystemDiscover_agentsSessionStep = SystemDiscover_agentsOpenStep | SystemDiscover_agentsSendStep | SystemDiscover_agentsReadStep | SystemDiscover_agentsFinishStep | SystemDiscover_agentsAbortStep

class SystemDiscover_agentsSessionPlan {
  step SystemDiscover_agentsSessionStep @description("Emit one FSM step. From history: no session → Open; open, no Send → Send; after Send (@N archived) → Finish, Read @N (large body: set grep, small limit, offset), or Send again.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for system/discover_tools: metadata for polymorphic Open selection.
class SystemDiscover_toolsToolCard {
  tool_name "system/discover_tools"
  description "Searches registered tools by lexical rank. Open session, then send query/limit; read() returns results. Send can be called multiple times."
  session_policy "Strict"
  input_summary "{ limit?: anyOf(integer | null), query?: anyOf(string | null) }"
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
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SystemDiscover_toolsReadStep {
  op "Read"
  input ArchiveReadInput @description("Prior Send archive: archive_ref required. Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.")
}

class SystemDiscover_toolsFinishStep {
  op "Finish"
}

class SystemDiscover_toolsAbortStep {
  op "Abort"
}

type SystemDiscover_toolsSessionStep = SystemDiscover_toolsOpenStep | SystemDiscover_toolsSendStep | SystemDiscover_toolsReadStep | SystemDiscover_toolsFinishStep | SystemDiscover_toolsAbortStep

class SystemDiscover_toolsSessionPlan {
  step SystemDiscover_toolsSessionStep @description("Emit one FSM step. From history: no session → Open; open, no Send → Send; after Send (@N archived) → Finish, Read @N (large body: set grep, small limit, offset), or Send again.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for system/internal_a2a: metadata for polymorphic Open selection.
class SystemInternal_a2aToolCard {
  tool_name "system/internal_a2a"
  description "Opens a conversational session to another agent. Send a message (text or parts), Read the response, then Send follow-ups or Finish. Multiple Send/Read rounds are allowed within one session — use this for multi-turn conversations with the delegated agent."
  session_policy "MultiSend"
  input_summary "{ parts: { filename?: anyOf(string | null), media_type?: anyOf(string | null), raw?: anyOf(string | null), text?: anyOf(string | null), url?: anyOf(string | null) }[] }"
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
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SystemInternal_a2aReadStep {
  op "Read"
  input ArchiveReadInput @description("Prior Send archive: archive_ref required. Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.")
}

class SystemInternal_a2aFinishStep {
  op "Finish"
}

class SystemInternal_a2aAbortStep {
  op "Abort"
}

type SystemInternal_a2aSessionStep = SystemInternal_a2aOpenStep | SystemInternal_a2aSendStep | SystemInternal_a2aReadStep | SystemInternal_a2aFinishStep | SystemInternal_a2aAbortStep

class SystemInternal_a2aSessionPlan {
  step SystemInternal_a2aSessionStep @description("Emit one FSM step. From history: no session → Open; open, no Send → Send (input.text must be non-empty); after Send → Read @N (large archive: set grep, small limit, offset to page), Finish, or Send again.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
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

Phase lock — the system text above this block begins with a bracket tag. Match that tag only; output MUST parse as the schema below (no extra keys, no alternate op names).

- [OPEN] … Open a session …: Emit ONLY Open for this tool. Exact shape: {"op":"Open","tool_name":"support/notion"}. In this hop do NOT emit Send, Read, CallTool, Call, Finish, block_id, params, args, or any Notion API / dotted method name — those belong to Send after the session exists.
- [SEND] … session is open …: Emit ONLY Send with op "Send", NotionInput in input, and citations.
- [CONTINUE] … result is archived …: Emit exactly one of Send, Read, or Finish as the schema allows; follow the preamble bullets. If cat -n @N for support/notion already lists blocks and has_more is false, emit Finish (do not Send the same block_id again). If has_more is true, the next Send must use start_cursor from that archive, not repeat the first page.

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

Phase lock — the system text above this block begins with a bracket tag. Match that tag only; output MUST parse as the schema below (no extra keys, no alternate op names).

- [OPEN] … Open a session …: Emit ONLY Open for this tool. Exact shape: {"op":"Open","tool_name":"support/notion"}. In this hop do NOT emit Send, Read, CallTool, Call, Finish, block_id, params, args, or any Notion API / dotted method name — those belong to Send after the session exists.
- [SEND] … session is open …: Emit ONLY Send with op "Send", NotionInput in input, and citations.
- [CONTINUE] … result is archived …: Emit exactly one of Send, Read, or Finish as the schema allows; follow the preamble bullets. If cat -n @N for support/notion already lists blocks and has_more is false, emit Finish (do not Send the same block_id again). If has_more is true, the next Send must use start_cursor from that archive, not repeat the first page.

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (Send only): The JSON root must be exactly one Send step: op must be the string Send, plus input and citations fields. Do NOT return Report, AskUser, Open, Read, or Finish. Do NOT wrap the Send object under a step property.
"#

}

/// Phase: continue — read, send again, or finish support/notion.
function ChooseNotionAction__continue__support_notion(
  goal: string,
  step_description: string,
  session_context: SessionContext
) -> SupportNotionSendStep | SupportNotionReadStep | SupportNotionFinishStep {

  client OpenRouterGPT
  prompt #"[CONTINUE] support/notion result is archived.\nCheck session history:\n- See \"@N support/notion\" followed by numbered lines → content is inline; emit Finish\n- See \"@N support/notion\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N support/notion\" with no content yet → emit Read archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\n\nYou are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

Phase lock — the system text above this block begins with a bracket tag. Match that tag only; output MUST parse as the schema below (no extra keys, no alternate op names).

- [OPEN] … Open a session …: Emit ONLY Open for this tool. Exact shape: {"op":"Open","tool_name":"support/notion"}. In this hop do NOT emit Send, Read, CallTool, Call, Finish, block_id, params, args, or any Notion API / dotted method name — those belong to Send after the session exists.
- [SEND] … session is open …: Emit ONLY Send with op "Send", NotionInput in input, and citations.
- [CONTINUE] … result is archived …: Emit exactly one of Send, Read, or Finish as the schema allows; follow the preamble bullets. If cat -n @N for support/notion already lists blocks and has_more is false, emit Finish (do not Send the same block_id again). If has_more is true, the next Send must use start_cursor from that archive, not repeat the first page.

{% if ctx.tags['conversation_history'] %}
Session history:
{% for msg in ctx.tags['conversation_history'] %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, Read, or Finish step. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For Read, follow ArchiveReadInput in the prelude (grep, limit, offset).
"#

}


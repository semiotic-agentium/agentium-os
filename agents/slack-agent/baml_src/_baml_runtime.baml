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

/// Terminal hop without a host tool session: answer from visible conversation archives only.
/// Do not emit when you need a fresh Open/Send — use ENTRY Open or ACTIVE Send instead.
class ReadOnlyFinishStep {
  op "ReadOnlyFinish"
  reply StructuredReply @description("User-visible answer; citations must include every @N archive line evidence relied on — omitting citations when grounding on archives is invalid.")
}
// Domain types generated from #[derive(BamlType)]
/// Which Slack API token to use.
/// Auto selects bot for most calls, user token for search (required in most orgs).
/// Bot forces the bot token (xoxb-...). User forces the user token (xoxp-...).
enum SlackAuthPreference {
  Auto @alias("auto")
  Bot @alias("bot")
  User @alias("user")
}

/// Slack conversation type.
/// Im = direct message (1:1 DM). Mpim = multi-person DM (group DM).
enum SlackConversationKind {
  PublicChannel @alias("public_channel")
  PrivateChannel @alias("private_channel")
  Im @alias("im") @description("Direct message (1:1 DM).")
  Mpim @alias("mpim") @description("Multi-person direct message (group DM).")
}

/// Message sort order. LatestFirst is the default (newest messages first).
enum SlackHistoryOrder {
  LatestFirst @alias("latest_first")
  OldestFirst @alias("oldest_first")
}

/// Controls whether Slack user IDs in results are resolved to display names.
/// ResolveUsers (default) fetches profiles for each unique user_id found.
/// None skips resolution — user_id only, no display name lookup.
enum SlackUserResolutionMode {
  None @alias("none")
  ResolveUsers @alias("resolve_users")
}

/// Sort field for search results. Score ranks by relevance; Timestamp by time.
enum SlackSearchSort {
  Score @alias("score")
  Timestamp @alias("timestamp")
}

/// Sort direction for search results.
enum SlackSearchDirection {
  Asc @alias("asc")
  Desc @alias("desc")
}

/// List channels and DMs available to the token.
/// Returns channel_id values to use in GetConversationHistory and GetThreadReplies.
class ListConversationsInput {
  kinds SlackConversationKind[] @description("Conversation types to include. Pass empty list to include all types.")
  cursor string? @description("Pagination cursor from a previous response's next_cursor.")
  limit int? @description("Max conversations to return (1–1000, default 100).")
  exclude_archived bool? @description("Exclude archived conversations (default true).")
  include_num_members bool? @description("Include member count in results.")
  auth SlackAuthPreference? @description("Token: `auto`, `bot`, or `user` (snake_case). PascalCase also accepted.")
}

/// Retrieve messages from a channel's history.
/// Use ListConversations to find the channel_id.
/// Timestamps use Slack's float format: '1609459200.000001' (Unix seconds with microseconds).
class GetConversationHistoryInput {
  channel_id string @description("Slack channel ID (e.g. C01234ABCDE) — obtain from ListConversations.")
  cursor string? @description("Pagination cursor from a previous call's next_cursor.")
  limit int? @description("Max messages to return (1–1000, default 100).")
  oldest string? @description("Return messages after this timestamp (Slack format: '1609459200.000001').")
  latest string? @description("Return messages before this timestamp (Slack format: '1609459200.000001').")
  inclusive bool? @description("Include messages exactly at oldest/latest boundaries.")
  order SlackHistoryOrder? @description("Sort order: JSON string `latest_first` or `oldest_first` (snake_case). `LatestFirst` / `OldestFirst` also accepted.")
  resolve_users SlackUserResolutionMode? @description("User enrichment: prefer snake_case `none` or `resolve_users`; PascalCase `None` / `ResolveUsers` also accepted.")
  auth SlackAuthPreference? @description("Token: prefer snake_case `auto`, `bot`, or `user`; PascalCase also accepted.")
}

/// Retrieve all replies in a thread.
/// thread_ts is the timestamp of the root message (found in message.thread_ts or message.ts).
/// Timestamps use Slack's float format: '1609459200.000001' (Unix seconds with microseconds).
class GetThreadRepliesInput {
  channel_id string @description("Slack channel ID containing the thread — obtain from ListConversations.")
  thread_ts string @description("Timestamp of the root message of the thread (Slack format: '1609459200.000001'). Use message.thread_ts or message.ts from a history/search result.")
  cursor string? @description("Pagination cursor from a previous call's next_cursor.")
  limit int? @description("Max replies to return (1–1000, default 100).")
  oldest string? @description("Return replies after this timestamp (Slack format: '1609459200.000001').")
  latest string? @description("Return replies before this timestamp (Slack format: '1609459200.000001').")
  inclusive bool? @description("Include messages exactly at oldest/latest boundaries.")
  order SlackHistoryOrder? @description("Sort order: JSON string `latest_first` or `oldest_first` (snake_case). `LatestFirst` / `OldestFirst` also accepted.")
  resolve_users SlackUserResolutionMode? @description("User enrichment: `none` or `resolve_users` (snake_case). Prefer snake_case; PascalCase (`None`, `ResolveUsers`) is tolerated.")
  auth SlackAuthPreference? @description("Token: `auto`, `bot`, or `user` (snake_case). PascalCase also accepted.")
}

/// Resolve a list of Slack user IDs to display names and profile data.
class ResolveUsersInput {
  user_ids string[] @description("List of Slack user IDs to resolve (e.g. ['U01234ABCDE']).")
  auth SlackAuthPreference? @description("Token: `auto`, `bot`, or `user` (snake_case). PascalCase also accepted.")
}

/// Search Slack messages across all conversations.
/// Requires a user token (SLACK_USER_TOKEN) in most orgs — use auth: User.
/// Supports Slack search modifiers: in:#channel, from:@user, before:YYYY-MM-DD, after:YYYY-MM-DD.
class SearchMessagesInput {
  query string @description("Search query. Supports modifiers: in:#channel, from:@user, before:YYYY-MM-DD, after:YYYY-MM-DD. Example: 'deploy in:#eng-releases after:2026-01-01'.")
  count int? @description("Results per page (1–100, default 20).")
  page int? @description("Page number (1-based).")
  sort SlackSearchSort? @description("`score` or `timestamp` (snake_case). `Score` / `Timestamp` also accepted.")
  direction SlackSearchDirection? @description("`asc` or `desc` (snake_case). `Asc` / `Desc` also accepted.")
  resolve_users SlackUserResolutionMode? @description("`none` or `resolve_users` (snake_case). PascalCase also accepted.")
  auth SlackAuthPreference? @description("Token preference — most orgs require User for search.")
}

/// Slack tool — five operations for reading workspace conversations.
/// ListConversations → find channel_ids.
/// GetConversationHistory → read channel messages.
/// GetThreadReplies → read a thread (requires thread_ts from a message).
/// SearchMessages → full-text search (requires user token in most orgs).
/// ResolveUsers → look up user display names from IDs.
type SlackInput = GetThreadRepliesInput | GetConversationHistoryInput | SearchMessagesInput | ResolveUsersInput | ListConversationsInput

enum SlackSourceKind {
  Conversation @alias("conversation")
  Message @alias("message")
  ThreadReply @alias("thread_reply")
  SearchMatch @alias("search_match")
  User @alias("user")
}

class SlackSource {
  kind SlackSourceKind
  reference string
  permalink string?
  channel_id string?
  message_ts string?
  thread_ts string?
}

class SlackConversationSummary {
  id string
  name string?
  kind SlackConversationKind
  is_archived bool
  is_member bool?
  num_members int?
}

class SlackUserSummary {
  id string
  name string?
  display_name string?
  real_name string?
  is_bot bool
  is_deleted bool
  email string?
}

class SlackMessageSummary {
  channel_id string
  ts string
  thread_ts string?
  user_id string?
  user_name string?
  text string
  subtype string?
  source_ref string
  permalink string?
}

class SlackOutput {
  conversations SlackConversationSummary[]
  messages SlackMessageSummary[]
  users SlackUserSummary[]
  next_cursor string?
  has_more bool
  sources SlackSource[]
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

enum InternalA2aNextOutputCompletion {
  DONE
  INPUTREQUIRED
    @alias("INPUT_REQUIRED")
  FAILED
}

class InternalA2aNextOutput {
  chunks ConversationChunk[]
  completion InternalA2aNextOutputCompletion | null?
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

/// Tool card for support/slack: metadata for polymorphic Open selection.
class SupportSlackToolCard {
  tool_name "support/slack"
  description "Read-only Slack integration for conversation retrieval and source-backed analysis."
  session_policy "Strict"
  input_summary "anyOf({ auth?: anyOf(enum(\"auto\" | \"bot\" | \"user\") | null), channel_id: string, cursor?: anyOf(string | null), inclusive?: anyOf(boolean | null), latest?: anyOf(string | null), limit?: anyOf(integer | null), oldest?: anyOf(string | null), order?: anyOf(enum(\"latest_first\" | \"oldest_first\") | null), resolve_users?: anyOf(enum(\"none\" | \"resolve_users\") | null), thread_ts: string } | { auth?: anyOf(enum(\"auto\" | \"bot\" | \"user\") | null), channel_id: string, cursor?: anyOf(string | null), inclusive?: anyOf(boolean | null), latest?: anyOf(string | null), limit?: anyOf(integer | null), oldest?: anyOf(string | null), order?: anyOf(enum(\"latest_first\" | \"oldest_first\") | null), resolve_users?: anyOf(enum(\"none\" | \"resolve_users\") | null) } | { auth?: anyOf(enum(\"auto\" | \"bot\" | \"user\") | null), count?: anyOf(integer | null), direction?: anyOf(enum(\"asc\" | \"desc\") | null), page?: anyOf(integer | null), query: string, resolve_users?: anyOf(enum(\"none\" | \"resolve_users\") | null), sort?: anyOf(enum(\"score\" | \"timestamp\") | null) } | { auth?: anyOf(enum(\"auto\" | \"bot\" | \"user\") | null), user_ids: string[] } | { auth?: anyOf(enum(\"auto\" | \"bot\" | \"user\") | null), cursor?: anyOf(string | null), exclude_archived?: anyOf(boolean | null), include_num_members?: anyOf(boolean | null), kinds: enum(\"im\" | \"mpim\" | \"private_channel\" | \"public_channel\")[], limit?: anyOf(integer | null) })"
  tags string[] @description("Tool tags: support, slack, read")
}

class SupportSlackOpenStep {
  op "Open"
  tool_name "support/slack" @description("Tool to open")
}

class SupportSlackSendStep {
  op "Send"
  input SlackInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class SupportSlackSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class SupportSlackPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SupportSlackFinishStep {
  op "Finish"
}

class SupportSlackAbortStep {
  op "Abort"
}

type SupportSlackSessionStep = SupportSlackOpenStep | SupportSlackSendStep | SupportSlackSearchReadStep | SupportSlackPageReadStep | SupportSlackFinishStep | SupportSlackAbortStep

class SupportSlackSessionPlan {
  step SupportSlackSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. A @N in history is a handle until you read it. Access: read.")
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

/// Tool card for system/discover_tools: metadata for polymorphic Open selection.
class SystemDiscoverToolsToolCard {
  tool_name "system/discover_tools"
  description "Searches registered tools by lexical rank. Open session, then send query/limit; read() returns results. Send can be called multiple times."
  session_policy "Strict"
  input_summary "{ limit?: anyOf(integer | null), query?: anyOf(string | null) }"
  tags string[] @description("Tool tags: system, discovery")
}

class SystemDiscoverToolsOpenStep {
  op "Open"
  tool_name "system/discover_tools" @description("Tool to open")
  initial_input DiscoverToolsOpenInput? @description("Optional open payload.")
}

class SystemDiscoverToolsSendStep {
  op "Send"
  input DiscoverToolsSendInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class SystemDiscoverToolsSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class SystemDiscoverToolsPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SystemDiscoverToolsFinishStep {
  op "Finish"
}

class SystemDiscoverToolsAbortStep {
  op "Abort"
}

type SystemDiscoverToolsSessionStep = SystemDiscoverToolsOpenStep | SystemDiscoverToolsSendStep | SystemDiscoverToolsSearchReadStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsAbortStep

class SystemDiscoverToolsSessionPlan {
  step SystemDiscoverToolsSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. A @N in history is a handle until you read it. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for system/internal_a2a: metadata for polymorphic Open selection.
class SystemInternalA2aToolCard {
  tool_name "system/internal_a2a"
  description "Opens a conversational session to another agent. Send a message (text or parts), Read the response, then Send follow-ups or Finish. Multiple Send/Read rounds are allowed within one session — use this for multi-turn conversations with the delegated agent."
  session_policy "MultiSend"
  input_summary "{ parts: { filename?: anyOf(string | null), media_type?: anyOf(string | null), raw?: anyOf(string | null), text?: anyOf(string | null), url?: anyOf(string | null) }[] }"
  tags string[] @description("Tool tags: system, a2a")
}

class SystemInternalA2aOpenStep {
  op "Open"
  tool_name "system/internal_a2a" @description("Tool to open")
  initial_input InternalA2aOpenInput @description("Required open payload.")
}

class SystemInternalA2aSendStep {
  op "Send"
  input InternalA2aSendInput @description("Conversational message payload. Set text to a non-empty string. Never null, never omit text.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class SystemInternalA2aSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class SystemInternalA2aPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SystemInternalA2aFinishStep {
  op "Finish"
}

class SystemInternalA2aAbortStep {
  op "Abort"
}

type SystemInternalA2aSessionStep = SystemInternalA2aOpenStep | SystemInternalA2aSendStep | SystemInternalA2aSearchReadStep | SystemInternalA2aPageReadStep | SystemInternalA2aFinishStep | SystemInternalA2aAbortStep

class SystemInternalA2aSessionPlan {
  step SystemInternalA2aSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes above — do not paraphrase them in prose. A @N in history is a handle until you read it. For A2A Send, `input.text` is required when sending. Access: write.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: entry — archive reuse, read-only finish, or Open a tool session.
function ChooseSlackAction__entry(
  goal: string,
  step_description: string,
  session_context: SessionContext
) -> ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | ReadOnlyResponse | SupportSlackOpenStep {

  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}Phase: ENTRY — reuse a visible archive (@N), ReadOnlyFinish without Open, or Open a tool session; legal ops only as in the narrowed union below.

ENTRY: reuse a visible tool archive (@N) via SearchRead/PageRead, answer with ReadOnlyFinish when no new Open is needed, or Open a session with: support/slack.

You execute ONE retrieve step of a read-only Slack plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

When emitting a session step: respect session history for channel_ids, cursors, and `thread_ts` (Slack API form: <unix>.<frac>; not permalink `p…` forms).
When this retrieve step is complete without further tool work, return ReadOnlyResponse instead of a session step.

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

---
Narrowed return union for this hop only:
- ArchivePageReadStep
- ArchiveSearchReadStep
- ReadOnlyFinishStep
- ReadOnlyResponse
- SupportSlackOpenStep

Emit exactly one JSON object matching one of the named types above, using the field shapes defined in the Tool and session-step types catalog at the top of this prompt. No markdown, no prose.


PHASE CONSTRAINT (entry): The JSON root must match ONLY this hop: Report, AskUser, ReadOnlyFinish (StructuredReply), ArchiveSearchReadStep, ArchivePageReadStep, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for session plans on the full Choose* function, not this narrowed hop. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N). ReadOnlyFinish is terminal — no host tool session; cite every @N used.
"#

}

/// Phase: active — Send, archive reads, Finish, or Abort (support/slack).
function ChooseSlackAction__active__support_slack(
  goal: string,
  step_description: string,
  session_context: SessionContext
) -> SupportSlackSendStep | SupportSlackSearchReadStep | SupportSlackPageReadStep | SupportSlackFinishStep | SupportSlackAbortStep {

  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}Phase: ACTIVE — for support/slack: Send, SearchRead, PageRead, Finish (after Done Send only), or Abort; legal ops only as below.

A support/slack session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.
support/slack result is archived.
Check session history:
- See "@N support/slack" followed by numbered lines → content is inline; emit Finish
- See "@N support/slack" with "more lines" indicator → emit SearchRead or PageRead to paginate
- See "@N support/slack" with no content yet → emit SearchRead or PageRead with archive_ref="@N"
- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern

You execute ONE retrieve step of a read-only Slack plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

When emitting a session step: respect session history for channel_ids, cursors, and `thread_ts` (Slack API form: <unix>.<frac>; not permalink `p…` forms).
When this retrieve step is complete without further tool work, return ReadOnlyResponse instead of a session step.

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

---
Narrowed return union for this hop only:
- SupportSlackSendStep
- SupportSlackSearchReadStep
- SupportSlackPageReadStep
- SupportSlackFinishStep
- SupportSlackAbortStep

Emit exactly one JSON object matching one of the named types above, using the field shapes defined in the Tool and session-step types catalog at the top of this prompt. No markdown, no prose.


PHASE CONSTRAINT (active): The JSON root must be exactly one Send, SearchRead, PageRead, Finish, or Abort step. op must be Send, SearchRead, PageRead, Finish, or Abort — never Read. Include input and citations per schema. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N). Finish is legal only after the host has a Done result from Send for this session (the runtime enforces this).
"#

}



// ── builder: agent-wide tool schema catalog (from IR) ──

/// Auto-generated agent-wide tool schema catalog.
/// Rendered once at build time into _baml_tool_schema_catalog.txt and loaded as ctx.tags['tool_schema_prelude'].
/// Never invoked against an LLM — exists only to drive BAML's ctx.output_format renderer.
function AgentToolSchemaCatalog__bamlrt() -> ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | SupportSlackAbortStep | SupportSlackFinishStep | SupportSlackOpenStep | SupportSlackSendStep | SystemDiscoverAgentsAbortStep | SystemDiscoverAgentsFinishStep | SystemDiscoverAgentsOpenStep | SystemDiscoverAgentsSendStep | SystemDiscoverToolsAbortStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsOpenStep | SystemDiscoverToolsSendStep | SystemInternalA2aAbortStep | SystemInternalA2aFinishStep | SystemInternalA2aOpenStep | SystemInternalA2aSendStep {
  client DefaultClient
  prompt #"
{{ ctx.output_format }}
"#
}

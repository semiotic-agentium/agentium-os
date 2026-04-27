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
// - SearchRead and PageRead require a real @N from a prior Send for this tool (per step field @description; see conversation_history when a windowed archive lists offset=).
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
  grep string @description("Non-empty line filter (substring or regex per host), e.g. name or -i error. Prefer SearchRead to narrow large or windowed archives before long PageRead scans. For unfiltered contiguous slices, use PageRead, not SearchRead.")
  offset int? @description("0-based offset counting lines that matched grep (after filter). Page with limit: first window offset=0; next offset = previous next_offset while matches remain.")
  limit int? @description("Max lines in this window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every SearchRead/PageRead must use a real @N; do not invent refs.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). Page with limit: first window offset=0; next offset = previous next_offset while lines remain.")
  limit int? @description("Max lines in this window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded).")
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
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SupportSlackSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Required: archive_ref and non-empty grep. If the visible @N in conversation is only a window, emit SearchRead with the same @N, grep, and the offset= value from the host line. Do not use SearchRead for contiguous unfiltered text — use PageRead.")
}

class SupportSlackPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Required: archive_ref; omit grep. Paging over full rendered lines. Use for contiguous unfiltered text or the next unfiltered window; the host line gives offset= for the next step. For locating sparse lines, use SearchRead with grep first, not long PageRead scans.")
}

class SupportSlackFinishStep {
  op "Finish"
}

class SupportSlackAbortStep {
  op "Abort"
}

type SupportSlackSessionStep = SupportSlackOpenStep | SupportSlackSendStep | SupportSlackSearchReadStep | SupportSlackPageReadStep | SupportSlackFinishStep | SupportSlackAbortStep

class SupportSlackSessionPlan {
  step SupportSlackSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send with the tool’s input for new work, or SearchRead/PageRead @N when that tool’s archive already exists; after Send (@N archived) → SearchRead (non-empty grep) to find lines, PageRead for contiguous unfiltered text, re-Send, or Finish. When history shows a partial or windowed @N, emit SearchRead or PageRead (offsets in the host line in conversation) before Finish if the task still needs unread lines. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

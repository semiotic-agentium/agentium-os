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
  agent_package string
  agent_instance_id string
  sub_message string
}

class StandardStructuredPlan {
  intent_description string
  objective string
  plan_steps StandardAgentPlanStep[]
}

class HistoryContext {
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload string?
}

/// FSM session context injected by the host at each step.
class SessionContext {
  contract_version string
  session_open bool
  allowed_ops string[]
  scope_ref string?
  output_ref string?
  evidence_ref string?
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
enum SlackAuthPreference {
  Auto
  Bot
  User
}

enum SlackConversationKind {
  PublicChannel
  PrivateChannel
  Im
  Mpim
}

enum SlackHistoryOrder {
  LatestFirst
  OldestFirst
}

enum SlackUserResolutionMode {
  None
  ResolveUsers
}

enum SlackSearchSort {
  Score
  Timestamp
}

enum SlackSearchDirection {
  Asc
  Desc
}

/// List conversations available to the authorized token.
class ListConversationsInput {
  kinds SlackConversationKind[]
  cursor string?
  limit int?
  exclude_archived bool?
  include_num_members bool?
  auth SlackAuthPreference?
}

/// Retrieve messages from a channel conversation history.
class GetConversationHistoryInput {
  channel_id string
  cursor string?
  limit int?
  oldest string?
  latest string?
  inclusive bool?
  order SlackHistoryOrder?
  resolve_users SlackUserResolutionMode?
  auth SlackAuthPreference?
}

/// Retrieve replies in a thread.
class GetThreadRepliesInput {
  channel_id string
  thread_ts string
  cursor string?
  limit int?
  oldest string?
  latest string?
  inclusive bool?
  order SlackHistoryOrder?
  resolve_users SlackUserResolutionMode?
  auth SlackAuthPreference?
}

/// Resolve specific Slack user IDs to display data.
class ResolveUsersInput {
  user_ids string[]
  auth SlackAuthPreference?
}

/// Search Slack messages.
///
/// Requires a user token (`SLACK_USER_TOKEN`) in most Slack orgs.
class SearchMessagesInput {
  query string
  count int?
  page int?
  sort SlackSearchSort?
  direction SlackSearchDirection?
  resolve_users SlackUserResolutionMode?
  auth SlackAuthPreference?
}

/// Typed action union for `support/slack`.
type SlackInput = GetThreadRepliesInput | GetConversationHistoryInput | SearchMessagesInput | ResolveUsersInput | ListConversationsInput

enum SlackSourceKind {
  Conversation
  Message
  ThreadReply
  SearchMatch
  User
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
  input_summary "{}"
  tags string[] @description("Tool tags: support, slack, read")
}

class SupportSlackOpenStep {
  op "Open"
  tool_name "support/slack" @description("Tool to open")
}

class SupportSlackSendStep {
  op "Send"
  input SlackInput @description("Payload for this step.")
}

class SupportSlackReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SupportSlackFinishStep {
  op "Finish"
}

class SupportSlackAbortStep {
  op "Abort"
}

type SupportSlackSessionStep = SupportSlackOpenStep | SupportSlackSendStep | SupportSlackReadStep | SupportSlackFinishStep | SupportSlackAbortStep

class SupportSlackSessionPlan {
  step SupportSlackSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again. Access: read.")
}


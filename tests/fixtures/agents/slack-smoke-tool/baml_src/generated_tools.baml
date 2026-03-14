// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Can occur AFTER Open, including multiple times across a session
// 3. Read: Retrieves output/progress using explicit read input
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before any Send step
// - Open may include 'initial_input' only when the step schema defines that field
// - Send uses 'input' field (for subsequent inputs)
// - Use Read to fetch pending output/chunks; Read may repeat between Send steps
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

class SessionContext {
  contract_version string
  session_open bool
  allowed_ops string[]
  scope_ref string?
  output_ref string?
  evidence_ref string?
}

class HistoryContext {
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload string?
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

class SupportSlackOpenStep {
  op "Open"
  reason string? @description("Optional reason.")
}

class SupportSlackSendStep {
  op "Send"
  input SlackInput @description("Payload for this step.")
  reason string? @description("Optional reason.")
}

class SupportSlackReadStep {
  op "Read"
  input SlackInput @description("Read payload for this step.")
  reason string? @description("Optional reason.")
}

class SupportSlackFinishStep {
  op "Finish"
  reason string? @description("Optional reason.")
}

class SupportSlackAbortStep {
  op "Abort"
  reason string? @description("Optional reason.")
}

type SupportSlackSessionStep = SupportSlackOpenStep | SupportSlackSendStep | SupportSlackReadStep | SupportSlackFinishStep | SupportSlackAbortStep

class SupportSlackSessionPlan {
  step SupportSlackSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. After Send, prefer Read before further Send/Finish so you consume tool output and status. Access: read.")
}

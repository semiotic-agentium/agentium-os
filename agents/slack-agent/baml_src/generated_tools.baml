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
}


class SupportSlackOpenStep {
  op "Open"
}

class SupportSlackSendStep {
  op "Send"
  input SlackInput @description("Payload for this step.")
}

class SupportSlackReadStep {
  op "Read"
  input SlackInput @description("Read payload for this step.")
}

class SupportSlackFinishStep {
  op "Finish"
}

class SupportSlackAbortStep {
  op "Abort"
}

type SupportSlackSessionStep = SupportSlackOpenStep | SupportSlackSendStep | SupportSlackReadStep | SupportSlackFinishStep | SupportSlackAbortStep

class SupportSlackSessionPlan {
  step SupportSlackSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. After Send, prefer Read before further Send/Finish so you consume tool output and status. Access: read.")
}

class SystemDiscover_agentsOpenStep {
  op "Open"
  initial_input DiscoverAgentsOpenInput? @description("Optional open payload.")
}

class SystemDiscover_agentsSendStep {
  op "Send"
  input DiscoverAgentsSendInput @description("Payload for this step.")
}

class SystemDiscover_agentsReadStep {
  op "Read"
  input DiscoverAgentsSendInput @description("Read payload for this step.")
}

class SystemDiscover_agentsFinishStep {
  op "Finish"
}

class SystemDiscover_agentsAbortStep {
  op "Abort"
}

type SystemDiscover_agentsSessionStep = SystemDiscover_agentsOpenStep | SystemDiscover_agentsSendStep | SystemDiscover_agentsReadStep | SystemDiscover_agentsFinishStep | SystemDiscover_agentsAbortStep

class SystemDiscover_agentsSessionPlan {
  step SystemDiscover_agentsSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. After Send, prefer Read before further Send/Finish so you consume tool output and status.")
}

class SystemDiscover_toolsOpenStep {
  op "Open"
  initial_input DiscoverToolsOpenInput? @description("Optional open payload.")
}

class SystemDiscover_toolsSendStep {
  op "Send"
  input DiscoverToolsSendInput @description("Payload for this step.")
}

class SystemDiscover_toolsReadStep {
  op "Read"
  input DiscoverToolsSendInput @description("Read payload for this step.")
}

class SystemDiscover_toolsFinishStep {
  op "Finish"
}

class SystemDiscover_toolsAbortStep {
  op "Abort"
}

type SystemDiscover_toolsSessionStep = SystemDiscover_toolsOpenStep | SystemDiscover_toolsSendStep | SystemDiscover_toolsReadStep | SystemDiscover_toolsFinishStep | SystemDiscover_toolsAbortStep

class SystemDiscover_toolsSessionPlan {
  step SystemDiscover_toolsSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. After Send, prefer Read before further Send/Finish so you consume tool output and status.")
}

class SystemInternal_a2aOpenStep {
  op "Open"
  initial_input InternalA2aOpenInput @description("Required open payload.")
}

class SystemInternal_a2aSendStep {
  op "Send"
  input InternalA2aSendInput @description("Conversational message payload. Set text to a non-empty string. Never null, never omit text.")
}

class SystemInternal_a2aReadStep {
  op "Read"
  input InternalA2aSendInput @description("Read payload for this step.")
}

class SystemInternal_a2aFinishStep {
  op "Finish"
}

class SystemInternal_a2aAbortStep {
  op "Abort"
}

type SystemInternal_a2aSessionStep = SystemInternal_a2aOpenStep | SystemInternal_a2aSendStep | SystemInternal_a2aReadStep | SystemInternal_a2aFinishStep | SystemInternal_a2aAbortStep

class SystemInternal_a2aSessionPlan {
  step SystemInternal_a2aSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. For Send, input.text MUST be a non-empty string message — never null. After Send, use Read to receive the response before sending again or finishing.")
}


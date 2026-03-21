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
  scope_ref string?
  output_ref string?
  evidence_ref string?
}

/// Read dereferences a prior Send result by archive ref.
/// grep -n / cat -n interface over archived output. Standard grep CLI flags apply.
/// IMPORTANT: always supply a grep pattern for archives larger than ~50 lines.
/// Bare cat on a large archive wastes context — grep to find relevant sections first.
class ArchiveReadInput {
  archive_ref string @description("Archive ref from the preceding Send, e.g. '@1'.")
  offset int? @description("Start line for pagination (0-based). To read the next page, use the offset value shown in the trailing footer of the previous read. Do NOT re-read from offset 0 with a larger limit.")
  limit int? @description("Maximum lines to return (default 40). Increase explicitly if you need more — e.g. limit=100 for large result sets.")
  grep string? @description("grep pattern with optional CLI flags: -i (case-insensitive), -E (extended regex). Examples: 'title', '-i agentium', \"-iE 'agent|platform'\". Use this instead of bare cat on large archives.")
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
}

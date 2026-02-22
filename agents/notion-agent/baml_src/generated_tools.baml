// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Can only occur AFTER Open - sends input to the session
// 3. Next: Retrieves output from the session (after Send)
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before any Send step
// - Open may include 'initial_input' only when the step schema defines that field
// - Send uses 'input' field (for subsequent inputs)
// - After Send, call Next to retrieve results
// - Always Finish or Abort to close the session

enum ToolSessionOp {
  Open @description("Open a new tool session. MUST be the first step in any plan. Include 'initial_input' only when the step schema defines that field.")
  Send @description("Send input to an already-open session. Can ONLY be used after an Open step. Use 'input' field (NOT 'initial_input').")
  Next @description("Retrieve the next output from the session. Call this after Send to get results. For streaming tools, call Next multiple times until Done.")
  Finish @description("Close the session gracefully after completing all operations. Always call Finish when done with a tool session.")
  Abort @description("Close the session with an error. Use this if something went wrong and you need to terminate the session.")
}

// Domain types generated from #[derive(BamlType)]
class NotionSearchPagesInput {
  query string?
  start_cursor string?
  page_size int?
}

class NotionGetPageInput {
  page_id string
}

class NotionGetPageBlocksInput {
  block_id string
  start_cursor string?
  page_size int?
  raw_blocks BlockRenderMode?
  max_depth int?
}

enum BlockRenderMode {
  Raw
  Enriched
}

/// Input for the Notion tool as per-action typed variants.
///
/// `deny_unknown_fields` on each variant ensures untagged routing rejects
/// mismatched fields. An empty object `{}` resolves to `SearchPages` (all
/// fields optional), which is intentional: a bare search is a safe default.
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
}

class DiscoverAgentsOpenInput {
  reason string?
}

class DiscoverAgentsSendInput {
  query string? @description("Optional filter: only agents whose name, agent_package, or description contain this string. Omit or null to return all agents (e.g. when user asks \"who is available?\" or \"list agents\").")
  limit int?
  offset int?
}

class DiscoverToolsNextOutput {
  tools ToolDiscoveryRecordDto[]
  done bool
}

class DiscoverToolsOpenInput {
  reason string? @description("Optional short justification for choosing to use discover_tools (e.g. \"user asked what tools are available\").")
}

class DiscoverToolsSendInput {
  query string?
  limit int?
}

enum InternalA2aCompletion {
  DONE
  INPUTREQUIRED
    @alias("INPUT_REQUIRED")
}

class InternalA2aNextOutput {
  chunks ConversationChunk[]
  completion InternalA2aCompletion | null? @description("Set to INPUT_REQUIRED when the delegated agent yielded TASK_STATE_INPUT_REQUIRED.")
}

class InternalA2aOpenInput {
  target InternalA2aTarget
}

class InternalA2aSendInput {
  parts ConversationPart[]?
  text string?
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


class SupportNotionOpenStep {
  op "Open"
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionSendStep {
  op "Send"
  input NotionInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SupportNotionSessionStep = SupportNotionOpenStep | SupportNotionSendStep | SupportNotionNextStep | SupportNotionFinishStep | SupportNotionAbortStep

class SupportNotionSessionPlan {
  steps SupportNotionSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}]. Access: read.")
}

class SystemDiscover_agentsOpenStep {
  op "Open"
  initial_input DiscoverAgentsOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsSendStep {
  op "Send"
  input DiscoverAgentsSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SystemDiscover_agentsSessionStep = SystemDiscover_agentsOpenStep | SystemDiscover_agentsSendStep | SystemDiscover_agentsNextStep | SystemDiscover_agentsFinishStep | SystemDiscover_agentsAbortStep

class SystemDiscover_agentsSessionPlan {
  steps SystemDiscover_agentsSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}].")
}

class SystemDiscover_toolsOpenStep {
  op "Open"
  initial_input DiscoverToolsOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_toolsSendStep {
  op "Send"
  input DiscoverToolsSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_toolsNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_toolsFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_toolsAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SystemDiscover_toolsSessionStep = SystemDiscover_toolsOpenStep | SystemDiscover_toolsSendStep | SystemDiscover_toolsNextStep | SystemDiscover_toolsFinishStep | SystemDiscover_toolsAbortStep

class SystemDiscover_toolsSessionPlan {
  steps SystemDiscover_toolsSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}].")
}

class SystemInternal_a2aOpenStep {
  op "Open"
  initial_input InternalA2aOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class SystemInternal_a2aSendStep {
  op "Send"
  input InternalA2aSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SystemInternal_a2aNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SystemInternal_a2aFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SystemInternal_a2aAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SystemInternal_a2aSessionStep = SystemInternal_a2aOpenStep | SystemInternal_a2aSendStep | SystemInternal_a2aNextStep | SystemInternal_a2aFinishStep | SystemInternal_a2aAbortStep

class SystemInternal_a2aSessionPlan {
  steps SystemInternal_a2aSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}].")
}


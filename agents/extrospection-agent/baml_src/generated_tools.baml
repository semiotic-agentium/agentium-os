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

/// Read dereferences a prior Send result by archive ref.
/// Think of it as grep -n / cat -n over the archived output. Standard grep flags apply.
class ArchiveReadInput {
  archive_ref string @description("Archive ref from the preceding Send, e.g. '@1'.")
  offset int? @description("Line offset for pagination (0-based).")
  limit int? @description("Maximum lines to return.")
  grep string? @description("grep pattern with optional CLI flags: -i (case-insensitive), -E (extended regex). Examples: 'title', '-i agentium', \"-iE 'agent|platform'\"")
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


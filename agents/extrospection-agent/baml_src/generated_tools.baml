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

class DiscoverAgentsNextOutput {
  agents AgentCardDto[]
  done bool
  historyContext HistoryContextV1 | null?
}

class DiscoverAgentsOpenInput {
  reason string?
}

class DiscoverAgentsSendInput {
  query string? @description("Optional filter: only agents whose name, agent_package, or description contain this string. Omit or null to return all agents (e.g. when user asks \"who is available?\" or \"list agents\").")
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


class SystemExtrospectionOpenStep {
  op "Open"
  initial_input ProvenanceQueryOpenInput? @description("Optional open payload.")
  reason string? @description("Optional reason.")
}

class SystemExtrospectionSendStep {
  op "Send"
  input ProvenanceQuerySendInput @description("Payload for this step.")
  reason string? @description("Optional reason.")
}

class SystemExtrospectionReadStep {
  op "Read"
  input ProvenanceQuerySendInput @description("Read payload for this step.")
  reason string? @description("Optional reason.")
}

class SystemExtrospectionFinishStep {
  op "Finish"
  reason string? @description("Optional reason.")
}

class SystemExtrospectionAbortStep {
  op "Abort"
  reason string? @description("Optional reason.")
}

type SystemExtrospectionSessionStep = SystemExtrospectionOpenStep | SystemExtrospectionSendStep | SystemExtrospectionReadStep | SystemExtrospectionFinishStep | SystemExtrospectionAbortStep

class SystemExtrospectionSessionPlan {
  step SystemExtrospectionSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. After Send, prefer Read before further Send/Finish so you consume tool output and status.")
}

class SystemDiscover_agentsOpenStep {
  op "Open"
  initial_input DiscoverAgentsOpenInput? @description("Optional open payload.")
  reason string? @description("Optional reason.")
}

class SystemDiscover_agentsSendStep {
  op "Send"
  input DiscoverAgentsSendInput @description("Payload for this step.")
  reason string? @description("Optional reason.")
}

class SystemDiscover_agentsReadStep {
  op "Read"
  input DiscoverAgentsSendInput @description("Read payload for this step.")
  reason string? @description("Optional reason.")
}

class SystemDiscover_agentsFinishStep {
  op "Finish"
  reason string? @description("Optional reason.")
}

class SystemDiscover_agentsAbortStep {
  op "Abort"
  reason string? @description("Optional reason.")
}

type SystemDiscover_agentsSessionStep = SystemDiscover_agentsOpenStep | SystemDiscover_agentsSendStep | SystemDiscover_agentsReadStep | SystemDiscover_agentsFinishStep | SystemDiscover_agentsAbortStep

class SystemDiscover_agentsSessionPlan {
  step SystemDiscover_agentsSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. After Send, prefer Read before further Send/Finish so you consume tool output and status.")
}

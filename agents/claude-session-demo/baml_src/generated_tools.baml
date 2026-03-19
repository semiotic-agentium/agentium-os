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
enum ClaudeCompletion {
  DONE
  INPUTREQUIRED
    @alias("INPUT_REQUIRED")
  INTERRUPTED
}

class ClaudeEventDtoVariant1 {
  text string
  kind string
}

class ClaudeEventDtoVariant2 {
  thinking string
  kind string
}

class ClaudeEventDtoVariant3 {
  id string
  name string
  input string
  kind string
}

class ClaudeEventDtoVariant4 {
  tool_use_id string
  content string?
  is_error bool?
  kind string
}

class ClaudeEventDtoVariant5 {
  subtype string
  cwd string
  model string
  data string?
  kind string
}

class ClaudeEventDtoVariant6 {
  event string
  kind string
}

class ClaudeEventDtoVariant7 {
  subtype string
  is_error bool
  num_turns int
  total_cost_usd float?
  result string?
  kind string
}

type ClaudeEventDto = ClaudeEventDtoVariant1 | ClaudeEventDtoVariant2 | ClaudeEventDtoVariant3 | ClaudeEventDtoVariant4 | ClaudeEventDtoVariant5 | ClaudeEventDtoVariant6 | ClaudeEventDtoVariant7

class ClaudeToolNextOutput {
  events ClaudeEventDto[]
  completion ClaudeCompletion | null?
  historyContext HistoryContextV1 | null?
}

class ClaudeToolOpenInput {
  workspace string?
}

class ClaudeToolSendInput {
  prompt string?
  content ClaudeUserContentBlockDto[]?
}

class ClaudeUserContentBlockDtoVariant1 {
  text string
  kind string
}

class ClaudeUserContentBlockDtoVariant2 {
  url string
  kind string
}

class ClaudeUserContentBlockDtoVariant3 {
  media_type string
  data string
  kind string
}

type ClaudeUserContentBlockDto = ClaudeUserContentBlockDtoVariant1 | ClaudeUserContentBlockDtoVariant2 | ClaudeUserContentBlockDtoVariant3

class HistoryContextV1 {
  hop int
  op string
  status string
  truncated bool?
  cursor string?
  payload string?
}


class ClaudeDevOpenStep {
  op "Open"
  initial_input ClaudeToolOpenInput? @description("Optional open payload.")
  reason string? @description("Optional reason.")
}

class ClaudeDevSendStep {
  op "Send"
  input ClaudeToolSendInput @description("Payload for this step.")
  reason string? @description("Optional reason.")
}

class ClaudeDevReadStep {
  op "Read"
  input ClaudeToolSendInput @description("Read payload for this step.")
  reason string? @description("Optional reason.")
}

class ClaudeDevFinishStep {
  op "Finish"
  reason string? @description("Optional reason.")
}

class ClaudeDevAbortStep {
  op "Abort"
  reason string? @description("Optional reason.")
}

type ClaudeDevSessionStep = ClaudeDevOpenStep | ClaudeDevSendStep | ClaudeDevReadStep | ClaudeDevFinishStep | ClaudeDevAbortStep

class ClaudeDevSessionPlan {
  step ClaudeDevSessionStep @description("Emit exactly one FSM step. Choose op from allowed_ops. After Send, prefer Read before further Send/Finish so you consume tool output and status. Access: write.")
}

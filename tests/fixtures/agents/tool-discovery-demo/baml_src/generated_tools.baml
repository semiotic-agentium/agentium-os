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

class HistoryContext {
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload string?
}

// Domain types generated from #[derive(BamlType)]
class Expression {
  left int
  operation MathOperation
  right int
}

enum MathOperation {
  Add @alias("+")
  Subtract @alias("-")
  Multiply @alias("*")
  Divide @alias("/")
}

class CalculatorInput {
  expression Expression
}

class CalculatorOutput {
  expression string
  result float
  formatted string
}

// Domain types generated from JSON schemas
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

class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
  eventSources string[]?
}


class SystemDiscover_toolsOpenStep {
  op "Open"
  initial_input DiscoverToolsOpenInput? @description("Optional open payload.")
  reason string? @description("Optional reason.")
}

class SystemDiscover_toolsSendStep {
  op "Send"
  input DiscoverToolsSendInput @description("Payload for this step.")
  reason string? @description("Optional reason.")
}

class SystemDiscover_toolsReadStep {
  op "Read"
  input DiscoverToolsSendInput @description("Read payload for this step.")
  reason string? @description("Optional reason.")
}

class SystemDiscover_toolsFinishStep {
  op "Finish"
  reason string? @description("Optional reason.")
}

class SystemDiscover_toolsAbortStep {
  op "Abort"
  reason string? @description("Optional reason.")
}

type SystemDiscover_toolsSessionStep = SystemDiscover_toolsOpenStep | SystemDiscover_toolsSendStep | SystemDiscover_toolsReadStep | SystemDiscover_toolsFinishStep | SystemDiscover_toolsAbortStep

class SystemDiscover_toolsSessionPlan {
  step SystemDiscover_toolsSessionStep @description("Emit exactly one FSM step. Check conversation history: no session → Open; session open, no Send → Send; Send done → Finish or Read @N to inspect result.")
}

class SupportCalculateOpenStep {
  op "Open"
  reason string? @description("Optional reason.")
}

class SupportCalculateSendStep {
  op "Send"
  input CalculatorInput @description("Payload for this step.")
  reason string? @description("Optional reason.")
}

class SupportCalculateReadStep {
  op "Read"
  input CalculatorInput @description("Read payload for this step.")
  reason string? @description("Optional reason.")
}

class SupportCalculateFinishStep {
  op "Finish"
  reason string? @description("Optional reason.")
}

class SupportCalculateAbortStep {
  op "Abort"
  reason string? @description("Optional reason.")
}

type SupportCalculateSessionStep = SupportCalculateOpenStep | SupportCalculateSendStep | SupportCalculateReadStep | SupportCalculateFinishStep | SupportCalculateAbortStep

class SupportCalculateSessionPlan {
  step SupportCalculateSessionStep @description("Emit exactly one FSM step. Check conversation history: no session → Open; session open, no Send → Send; Send done → Finish or Read @N to inspect result.")
}

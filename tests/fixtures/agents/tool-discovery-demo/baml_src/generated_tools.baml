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
}

class DiscoverToolsOpenInput {
  reason string? @description("Optional short justification for choosing to use discover_tools (e.g. \"user asked what tools are available\").")
}

class DiscoverToolsSendInput {
  query string?
  limit int?
}

class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
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

class SupportCalculateOpenStep {
  op "Open"
  reason string? @description("Optional explanation for this step.")
}

class SupportCalculateSendStep {
  op "Send"
  input CalculatorInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SupportCalculateNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SupportCalculateFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SupportCalculateAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SupportCalculateSessionStep = SupportCalculateOpenStep | SupportCalculateSendStep | SupportCalculateNextStep | SupportCalculateFinishStep | SupportCalculateAbortStep

class SupportCalculateSessionPlan {
  steps SupportCalculateSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}].")
}


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

// Domain types generated from #[derive(BamlType)]
/// A binary arithmetic expression: left OP right.
/// Provide integer operands and choose one of the four arithmetic operations.
class Expression {
  left int @description("Left-hand integer operand.")
  operation MathOperation @description("Arithmetic operation to apply.")
  right int @description("Right-hand integer operand. Must not be 0 when operation is Divide.")
}

/// Arithmetic operation for a binary expression.
/// Accepted as the variant name (e.g. 'Add') or its symbolic alias ('+', '-', '*', '/').
enum MathOperation {
  Add @alias("+")
  Subtract @alias("-")
  Multiply @alias("*")
  Divide @alias("/")
}

/// Input to the calculator tool. Construct an Expression and submit it to evaluate.
class CalculatorInput {
  expression Expression @description("The binary expression to evaluate.")
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

/// Tool card for support/calculate: metadata for polymorphic Open selection.
class SupportCalculateToolCard {
  tool_name "support/calculate"
  description "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division."
  session_policy "Strict"
  input_summary "{ expression: any }"
  tags string[] @description("Tool tags: support, calculate")
}

class SupportCalculateOpenStep {
  op "Open"
  tool_name "support/calculate" @description("Tool to open")
}

class SupportCalculateSendStep {
  op "Send"
  input CalculatorInput @description("Payload for this step.")
}

class SupportCalculateReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SupportCalculateFinishStep {
  op "Finish"
}

class SupportCalculateAbortStep {
  op "Abort"
}

type SupportCalculateSessionStep = SupportCalculateOpenStep | SupportCalculateSendStep | SupportCalculateReadStep | SupportCalculateFinishStep | SupportCalculateAbortStep

class SupportCalculateSessionPlan {
  step SupportCalculateSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again.")
}

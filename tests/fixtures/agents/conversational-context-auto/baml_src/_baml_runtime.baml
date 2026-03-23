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
  agent_package string @description("Exact agent_package from discovery results (e.g. 'extrospection-agent'). Copy verbatim — do not truncate or reformat.")
  agent_instance_id string @description("Agent instance ID, usually 'default'. Copy from discovery results.")
  sub_message string
}

class StandardStructuredPlan {
  intent_description string
  objective string
  plan_steps StandardAgentPlanStep[]
  citations string[]? @description("Optional checked refs (#N, @N) grounding this plan for observability; omit or use [] when none.")
}

class HistoryContext {
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload string?
}

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
  allowed_ops string[]? @description("Deprecated: per-phase step-executor functions narrow the output schema. Host may omit.")
  selected_tool string?
  status_token string?
}

// Structured reply types for synthesis functions
enum ReplyMediaType {
  TextPlain @alias("text/plain")
  TextMarkdown @alias("text/markdown")
  ApplicationJson @alias("application/json")
  TextCsv @alias("text/csv")
}

class TextPart {
  type "text"
  text string
}

class DataPart {
  type "data"
  data string @description("Serialised content (e.g. JSON string, CSV string).")
  media_type ReplyMediaType @description("MIME type of the data field.")
}

type ReplyPart = TextPart | DataPart

// Canonical synthesis + wire shape: same type in BAML and TypeScript (SessionResult.message, emitters).
class StructuredReply {
  parts ReplyPart[] @description("Ordered parts: start with TextPart (type \"text\") — set media_type TextMarkdown for rich answers (see TextPart); optionally append DataPart (type \"data\", media_type ApplicationJson) with a JSON string for machine-readable UI payloads.")
  citations string[] @description("History refs this reply was derived from. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}

/// Archive deref input for Read steps.
/// archive_ref is required: use the @N ref from the Send result.
class ArchiveReadInput {
  archive_ref string @description("Required archive ref e.g. '@1'. Use the ref returned by the preceding Send step.")
  offset int? @description("Line offset for pagination (0-based). Omit to start from the beginning.")
  limit int? @description("Maximum lines to return. Omit for default page size.")
  grep string? @description("Optional grep pattern e.g. 'deploy' or '-i deploy' for case-insensitive.")
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
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
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
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseCalcTool__select(
  user_message: string,
  session_context: SessionContext
) -> SupportCalculateOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: support/calculate.\n\nThe user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
IMPORTANT: Return a top-level JSON object with a `step` field (and optional `reason`), never a bare JSON array.
Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
{{ ctx.output_format }}
{{ _.role('user') }}
{{ user_message }}"#

}

/// Phase: act — send query to support/calculate.
function ChooseCalcTool__act__support_calculate(
  user_message: string,
  session_context: SessionContext
) -> SupportCalculateSendStep {

  client DefaultClient
  prompt #"[SEND] A support/calculate session is open. Emit Send with your query.\n\nThe user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
IMPORTANT: Return a top-level JSON object with a `step` field (and optional `reason`), never a bare JSON array.
Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
{{ ctx.output_format }}
{{ _.role('user') }}
{{ user_message }}"#

}

/// Phase: continue — read, send again, or finish support/calculate.
function ChooseCalcTool__continue__support_calculate(
  user_message: string,
  session_context: SessionContext
) -> SupportCalculateSendStep | SupportCalculateReadStep | SupportCalculateFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] support/calculate result is archived.\nCheck session history:\n- See \"@N support/calculate\" followed by numbered lines → content is inline; emit Finish\n- See \"@N support/calculate\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N support/calculate\" with no content yet → emit Read archive_ref=\"@N\"\n\nThe user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
IMPORTANT: Return a top-level JSON object with a `step` field (and optional `reason`), never a bare JSON array.
Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
{{ ctx.output_format }}
{{ _.role('user') }}
{{ user_message }}"#

}

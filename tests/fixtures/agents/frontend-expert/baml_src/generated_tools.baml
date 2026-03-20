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

/// Archive deref input for Read steps.
/// archive_ref is required: use the @N ref from the Send result.
class ArchiveReadInput {
  archive_ref string @description("Required archive ref e.g. '@1'. Use the ref returned by the preceding Send step.")
  offset int? @description("Line offset for pagination (0-based). Omit to start from the beginning.")
  limit int? @description("Maximum lines to return. Omit for default page size.")
  grep string? @description("Optional grep pattern e.g. 'deploy' or '-i deploy' for case-insensitive.")
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


/// Tool card for claude/dev: metadata for polymorphic Open selection.
class ClaudeDevToolCard {
  tool_name "claude/dev"
  description "Host-managed Claude streaming session. Open once, send prompt/content, then call next() until completion is DONE/INTERRUPTED or INPUT_REQUIRED for resume."
  session_policy "Strict"
  input_summary "{ content?: any, prompt?: any }"
  tags string[] @description("Tool tags: claude, stream, session")
}

class ClaudeDevOpenStep {
  op "Open"
  tool_name "claude/dev" @description("Tool to open")
  initial_input ClaudeToolOpenInput? @description("Optional open payload.")
}

class ClaudeDevSendStep {
  op "Send"
  input ClaudeToolSendInput @description("Conversational message payload. Set text to a non-empty string. Never null, never omit text.")
}

class ClaudeDevReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class ClaudeDevFinishStep {
  op "Finish"
}

class ClaudeDevAbortStep {
  op "Abort"
}

type ClaudeDevSessionStep = ClaudeDevOpenStep | ClaudeDevSendStep | ClaudeDevReadStep | ClaudeDevFinishStep | ClaudeDevAbortStep

class ClaudeDevSessionPlan {
  step ClaudeDevSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send (input.text MUST be non-empty); Send done → Finish or Read @N. Access: write.")
}


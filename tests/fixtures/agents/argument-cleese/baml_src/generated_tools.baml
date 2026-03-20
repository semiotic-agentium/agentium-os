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


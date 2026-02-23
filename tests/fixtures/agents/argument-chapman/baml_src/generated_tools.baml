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


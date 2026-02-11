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
// - Open uses 'initial_input' field (for first input when opening)
// - Send uses 'input' field (for subsequent inputs)
// - After Send, call Next to retrieve results
// - Always Finish or Abort to close the session

enum ToolSessionOp {
  Open @description("Open a new tool session. MUST be the first step in any plan. Use 'initial_input' field to provide the first input when opening the session.")
  Send @description("Send input to an already-open session. Can ONLY be used after an Open step. Use 'input' field (NOT 'initial_input').")
  Next @description("Retrieve the next output from the session. Call this after Send to get results. For streaming tools, call Next multiple times until Done.")
  Finish @description("Close the session gracefully after completing all operations. Always call Finish when done with a tool session.")
  Abort @description("Close the session with an error. Use this if something went wrong and you need to terminate the session.")
}

// Domain types generated from JSON schemas
enum NotionAction {
  SearchPages
  GetPage
  GetPageBlocks
}

class NotionBlockSummary {
  id string
  block_type string
  text string?
  has_children bool
}

class NotionInput {
  action NotionAction
  query string?
  start_cursor string?
  page_size int?
  page_id string?
  block_id string?
}

class NotionOutput {
  pages NotionPageSummary[]
  blocks NotionBlockSummary[]
  next_cursor string?
  has_more bool
  sources NotionSource[]
  message string
}

class NotionPageSummary {
  id string
  title string
  url string
  last_edited_time string?
}

class NotionSource {
  page_id string
  url string
}


class SupportNotionOpenStep {
  op "Open"
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionSendStep {
  op "Send"
  input NotionInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SupportNotionAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SupportNotionSessionStep = SupportNotionOpenStep | SupportNotionSendStep | SupportNotionNextStep | SupportNotionFinishStep | SupportNotionAbortStep

class SupportNotionSessionPlan {
  steps SupportNotionSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (with optional initial_input), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Example valid plan: [{op: 'Open', initial_input: {...}}, {op: 'Next'}, {op: 'Finish'}]. Tool identity is inferred from the input schema (no tool_name field). Access: read.")
}


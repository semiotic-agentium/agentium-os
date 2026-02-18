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
/// Which ClickUp action to perform.
enum ClickUpAction {
  ListTeams
  ListSpaces
  ListLists
  ListTasks
  GetTask
  CreateTask
  UpdateTask
}

/// Input for the ClickUp tool.
///
/// Uses a flat struct with an `action` discriminator instead of a Rust enum
/// so that BAML (which lacks sum types) can represent it as a single class
/// with an enum field. Per-action field requirements are validated at runtime
/// in [`ClickUpTool::execute`].
class ClickUpInput {
  action ClickUpAction
  team_id string? @description("Required for ListSpaces.")
  space_id string? @description("Required for ListLists.")
  list_id string? @description("Required for ListTasks and CreateTask.")
  task_id string? @description("Required for GetTask and UpdateTask.")
  name string? @description("Required for CreateTask.")
  description string?
  priority int? @description("ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")
  status string? @description("Task status string. Used by UpdateTask.")
}

/// Summary of a single ClickUp task, projected from the API response.
class ClickUpTaskSummary {
  id string
  name string
  status string
  description string?
  url string
  assignees string[]
  priority string?
  due_date string?
}

/// A generic item returned by navigation actions (teams, spaces, lists).
class ClickUpItem {
  id string
  name string
  kind string @description("The kind of resource: team, space, or list.")
}

/// Output returned by every ClickUp tool action.
///
/// Task actions populate `tasks`; navigation actions populate `items`.
class ClickUpOutput {
  tasks ClickUpTaskSummary[]
  items ClickUpItem[] @description("Teams, spaces, or lists returned by navigation actions.")
  message string
}

class SupportClickupOpenStep {
  op "Open"
  reason string? @description("Optional explanation for this step.")
}

class SupportClickupSendStep {
  op "Send"
  input ClickUpInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SupportClickupNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SupportClickupFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SupportClickupAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SupportClickupSessionStep = SupportClickupOpenStep | SupportClickupSendStep | SupportClickupNextStep | SupportClickupFinishStep | SupportClickupAbortStep

class SupportClickupSessionPlan {
  steps SupportClickupSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Example: [{op: 'Open'}, {op: 'Send', input: {...}}, {op: 'Next'}, {op: 'Finish'}].")
}


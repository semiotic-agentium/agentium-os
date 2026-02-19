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
/// List all teams/workspaces — no parameters needed.
class ListTeamsInput {
}

/// List spaces in a team.
class ListSpacesInput {
  team_id string
}

/// List task-lists in a space.
class ListListsInput {
  space_id string
}

/// List tasks in a list.
class ListTasksInput {
  list_id string
}

/// Get details of a specific task.
class GetTaskInput {
  task_id string
}

/// Create a new task in a list.
class CreateTaskInput {
  list_id string
  name string
  description string?
  priority int? @description("ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")
}

/// Update an existing task's status, description, or priority.
class UpdateTaskInput {
  task_id string
  status string? @description("Task status string (e.g. in progress).")
  description string?
  priority int? @description("ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")
}

/// Delete a task from the workspace.
class DeleteTaskInput {
  task_id string
  confirm_delete bool
}

/// Union of all ClickUp action inputs.
///
/// The LLM picks the appropriate variant based on the desired action.
/// In generated BAML this becomes:
/// `type ClickUpInput = ListTeamsInput | ListSpacesInput | … | DeleteTaskInput`
///
/// **Variant order matters**: `serde(untagged)` tries variants top-down and
/// takes the first successful match. Ordering invariants:
/// - `GetTask` must precede `UpdateTask` so that `{ "task_id": "..." }`
/// (without optional update fields) resolves to `GetTask`, not `UpdateTask`.
/// - `DeleteTask` has `confirm_delete: bool` which disambiguates it from
/// `GetTask`, so its position is flexible.
type ClickUpInput = ListTeamsInput | ListSpacesInput | ListListsInput | ListTasksInput | GetTaskInput | CreateTaskInput | UpdateTaskInput | DeleteTaskInput

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

// Domain types generated from JSON schemas
class AgentCardDto {
  name string
  version string
  agentPackage string
  agentInstanceId string
  tools string[]
  description string?
  capabilities string[]
}

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

class DiscoverAgentsNextOutput {
  agents AgentCardDto[]
  done bool
}

class DiscoverAgentsOpenInput {
  reason string?
}

class DiscoverAgentsSendInput {
  query string? @description("Optional filter: only agents whose name, agent_package, or description contain this string. Omit or null to return all agents (e.g. when user asks \"who is available?\" or \"list agents\").")
  limit int?
  offset int?
}

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

class InternalA2aNextOutput {
  chunks ConversationChunk[]
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

class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
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
  steps SupportClickupSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Optionally include __type on the plan (e.g. \"SupportClickupSessionPlan\") or on each step (e.g. \"SupportClickupOpenStep\") for tool resolution when the runtime has no manifest. Example: [{\"__type\": \"SupportClickupOpenStep\", op: \"Open\"}, {\"__type\": \"SupportClickupSendStep\", op: \"Send\", input: {...}}, {\"__type\": \"SupportClickupNextStep\", op: \"Next\"}, {\"__type\": \"SupportClickupFinishStep\", op: \"Finish\"}].")
}

class SystemDiscover_agentsOpenStep {
  op "Open"
  initial_input DiscoverAgentsOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsSendStep {
  op "Send"
  input DiscoverAgentsSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class SystemDiscover_agentsAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type SystemDiscover_agentsSessionStep = SystemDiscover_agentsOpenStep | SystemDiscover_agentsSendStep | SystemDiscover_agentsNextStep | SystemDiscover_agentsFinishStep | SystemDiscover_agentsAbortStep

class SystemDiscover_agentsSessionPlan {
  steps SystemDiscover_agentsSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Optionally include __type on the plan (e.g. \"SystemDiscover_agentsSessionPlan\") or on each step (e.g. \"SystemDiscover_agentsOpenStep\") for tool resolution when the runtime has no manifest. Example: [{\"__type\": \"SystemDiscover_agentsOpenStep\", op: \"Open\"}, {\"__type\": \"SystemDiscover_agentsSendStep\", op: \"Send\", input: {...}}, {\"__type\": \"SystemDiscover_agentsNextStep\", op: \"Next\"}, {\"__type\": \"SystemDiscover_agentsFinishStep\", op: \"Finish\"}].")
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
  steps SystemDiscover_toolsSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Optionally include __type on the plan (e.g. \"SystemDiscover_toolsSessionPlan\") or on each step (e.g. \"SystemDiscover_toolsOpenStep\") for tool resolution when the runtime has no manifest. Example: [{\"__type\": \"SystemDiscover_toolsOpenStep\", op: \"Open\"}, {\"__type\": \"SystemDiscover_toolsSendStep\", op: \"Send\", input: {...}}, {\"__type\": \"SystemDiscover_toolsNextStep\", op: \"Next\"}, {\"__type\": \"SystemDiscover_toolsFinishStep\", op: \"Finish\"}].")
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
  steps SystemInternal_a2aSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Optionally include __type on the plan (e.g. \"SystemInternal_a2aSessionPlan\") or on each step (e.g. \"SystemInternal_a2aOpenStep\") for tool resolution when the runtime has no manifest. Example: [{\"__type\": \"SystemInternal_a2aOpenStep\", op: \"Open\"}, {\"__type\": \"SystemInternal_a2aSendStep\", op: \"Send\", input: {...}}, {\"__type\": \"SystemInternal_a2aNextStep\", op: \"Next\"}, {\"__type\": \"SystemInternal_a2aFinishStep\", op: \"Finish\"}].")
}

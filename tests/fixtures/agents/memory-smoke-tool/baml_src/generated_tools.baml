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
class MemoryAddNextOutput {
  nodeIds int[] @description("IDs of newly created nodes (in the same order as input events).")
  edgeCount int @description("Number of edges created.")
  done bool
}

class MemoryAddOpenInput {
  reason string?
}

class MemoryAddSendInput {
  events MemoryEventInput[] @description("Cognitive events to store.")
  edges MemoryEdgeInput[]? @description("Edges to create. May reference existing nodes and (for advanced callers) predicted IDs\nof nodes created in this same batch.\nPrefer `memory/link` after `memory/add` returns node IDs when you do not know IDs upfront.")
}

class MemoryEdgeInput {
  source int @description("Source node ID.")
  target int @description("Target node ID.")
  edgeType MemoryEdgeType @description("Edge type.")
  weight float? @description("Edge weight (0.0 to 1.0, default 1.0).")
}

enum MemoryEdgeType {
  CausedBy
    @alias("caused_by")
  Supports
    @alias("supports")
  Contradicts
    @alias("contradicts")
  Supersedes
    @alias("supersedes")
  RelatedTo
    @alias("related_to")
  PartOf
    @alias("part_of")
  TemporalNext
    @alias("temporal_next")
}

class MemoryEventInput {
  eventType MemoryEventType @description("Event type.")
  content string @description("The content of this cognitive event.")
  sessionId int? @description("Session identifier (groups related events).")
  confidence float? @description("Confidence level (0.0 to 1.0, default 1.0).")
}

enum MemoryEventType {
  Fact
    @alias("fact")
  Decision
    @alias("decision")
  Inference
    @alias("inference")
  Correction
    @alias("correction")
  Skill
    @alias("skill")
  Episode
    @alias("episode")
}

enum MemoryHealthStatus {
  Pass
    @alias("pass")
  Warn
    @alias("warn")
  Fail
    @alias("fail")
}

class MemoryLinkNextOutput {
  edgesCreated int
  done bool
}

class MemoryLinkOpenInput {
  reason string?
}

class MemoryLinkSendInput {
  edges MemoryEdgeInput[] @description("Edges to create.")
}

class MemorySearchMatch {
  id int
  score float
  content string
  eventType MemoryEventType
  sessionId int? @description("Session ID when present; omitted if the node was written without session attribution.")
  confidence float
}

class MemorySearchNextOutput {
  matches MemorySearchMatch[]
  done bool
}

class MemorySearchOpenInput {
  reason string?
}

class MemorySearchSendInput {
  query string @description("Text query (BM25 ranked).")
  types MemoryEventType[]? @description("Filter by event types.")
  sessions int[]? @description("Filter by session IDs.")
  max int? @description("Maximum number of results (default 10).")
}

class MemoryStatsNextOutput {
  status MemoryHealthStatus @description("Overall health.")
  nodeCount int
  edgeCount int
  contradictionEdges int
  supersedesEdges int
  lowConfidenceCount int
  staleCount int
  orphanCount int
  unsupportedDecisions int
  filePath string
  done bool
}

class MemoryStatsOpenInput {
  reason string?
}

class MemoryStatsSendInput {
  reserved bool? @description("Reserved optional field: the BAML schema generator currently rejects\nempty classes, so this keeps the wire input effectively `{}`.")
}

enum MemoryTraversalDirection {
  Forward
    @alias("forward")
  Backward
    @alias("backward")
  Both
    @alias("both")
}

class MemoryTraverseNextOutput {
  nodes TraversalNode[]
  edges TraversalEdge[]
  done bool
}

class MemoryTraverseOpenInput {
  reason string?
}

class MemoryTraverseSendInput {
  startId int @description("Starting node ID.")
  edgeTypes MemoryEdgeType[]? @description("Edge types to follow (default: all).")
  direction MemoryTraversalDirection | null? @description("Traversal direction (default: `forward`).")
  depth int? @description("Maximum traversal depth (default: 3).")
}

class TraversalEdge {
  source int
  target int
  edgeType MemoryEdgeType
  weight float
}

class TraversalNode {
  id int
  content string
  eventType MemoryEventType
  confidence float
  depth int
}


class MemoryAddOpenStep {
  op "Open"
  initial_input MemoryAddOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryAddSendStep {
  op "Send"
  input MemoryAddSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryAddNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class MemoryAddFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class MemoryAddAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type MemoryAddSessionStep = MemoryAddOpenStep | MemoryAddSendStep | MemoryAddNextStep | MemoryAddFinishStep | MemoryAddAbortStep

class MemoryAddSessionPlan {
  steps MemoryAddSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}]. Access: write.")
}

class MemorySearchOpenStep {
  op "Open"
  initial_input MemorySearchOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class MemorySearchSendStep {
  op "Send"
  input MemorySearchSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class MemorySearchNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class MemorySearchFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class MemorySearchAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type MemorySearchSessionStep = MemorySearchOpenStep | MemorySearchSendStep | MemorySearchNextStep | MemorySearchFinishStep | MemorySearchAbortStep

class MemorySearchSessionPlan {
  steps MemorySearchSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}]. Access: read.")
}

class MemoryLinkOpenStep {
  op "Open"
  initial_input MemoryLinkOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryLinkSendStep {
  op "Send"
  input MemoryLinkSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryLinkNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class MemoryLinkFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class MemoryLinkAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type MemoryLinkSessionStep = MemoryLinkOpenStep | MemoryLinkSendStep | MemoryLinkNextStep | MemoryLinkFinishStep | MemoryLinkAbortStep

class MemoryLinkSessionPlan {
  steps MemoryLinkSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}]. Access: write.")
}

class MemoryTraverseOpenStep {
  op "Open"
  initial_input MemoryTraverseOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryTraverseSendStep {
  op "Send"
  input MemoryTraverseSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryTraverseNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class MemoryTraverseFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class MemoryTraverseAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type MemoryTraverseSessionStep = MemoryTraverseOpenStep | MemoryTraverseSendStep | MemoryTraverseNextStep | MemoryTraverseFinishStep | MemoryTraverseAbortStep

class MemoryTraverseSessionPlan {
  steps MemoryTraverseSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}]. Access: read.")
}

class MemoryStatsOpenStep {
  op "Open"
  initial_input MemoryStatsOpenInput? @description("Optional open-time configuration payload for the session. This is used only by Open and is not auto-sent as a Send input.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryStatsSendStep {
  op "Send"
  input MemoryStatsSendInput @description("Input to send to the already-open session.")
  reason string? @description("Optional explanation for this step.")
}

class MemoryStatsNextStep {
  op "Next"
  reason string? @description("Optional explanation for this step.")
}

class MemoryStatsFinishStep {
  op "Finish"
  reason string? @description("Optional explanation for this step.")
}

class MemoryStatsAbortStep {
  op "Abort"
  reason string? @description("Optional explanation for aborting the session.")
}

type MemoryStatsSessionStep = MemoryStatsOpenStep | MemoryStatsSendStep | MemoryStatsNextStep | MemoryStatsFinishStep | MemoryStatsAbortStep

class MemoryStatsSessionPlan {
  steps MemoryStatsSessionStep[] @description("Array of FSM steps. MUST follow this strict order: 1) Open (include initial_input only when the schema defines it), 2) Send (with input), 3) Next (to retrieve results), 4) Finish or Abort (to close). Runtime resolves this plan to a tool via builder-generated function->plan mapping (session_plan_functions.json). Example: [{op: \"Open\"}, {op: \"Send\", input: {...}}, {op: \"Next\"}, {op: \"Finish\"}]. Access: read.")
}


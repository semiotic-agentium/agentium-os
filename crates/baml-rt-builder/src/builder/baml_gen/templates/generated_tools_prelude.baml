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

/// Step row for LLM-authored plans committed via execution-session `submitPlan`.
/// `step_id` is a plan-local alias for `startStep` / `completeStep` — not a global provenance identifier;
/// the host derives canonical step entity ids from task scope + plan + this slug.
/// `depends_on` lists prerequisite `step_id` values (DAG). Omit or use [] when none; use `order` for display and tie-breaking.
class ProvenancePlanStep {
  step_id string @description("Plan-local slug (may be LLM-authored); not globally unique — host compounds with task/plan for canonical ids. Reuse in startStep/completeStep.")
  description string @description("What this step does.")
  order int @description("Ordinal for display and tie-breaking.")
  depends_on string[]? @description("Prerequisite step_ids (same alias namespace as step_id); omit or [] if none.")
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

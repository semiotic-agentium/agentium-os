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
// Domain types generated from JSON schemas
class CallbackToolInputVariant1 {
  after_ms int
  source_key string
  payload string
  dedupe_key string | null?
  continuation string | null?
}

class CallbackToolInputVariant2 {
  callback_id string | null?
  source_key string | null?
  dedupe_key string | null?
}

type CallbackToolInput = CallbackToolInputVariant1 | CallbackToolInputVariant2

class CallbackToolOutputVariant1 {
  callback_id string
  source_key string
  scheduled_for_unix_ms int
  deduped bool
}

class CallbackToolOutputVariant2 {
  cancelled bool
  callback_id string | null?
  source_key string | null?
  dedupe_key string | null?
}

type CallbackToolOutput = CallbackToolOutputVariant1 | CallbackToolOutputVariant2

class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
  event_sources string[]
}

class HistoryContextV1 {
  hop int
  op string
  status string
  truncated bool
  cursor string | null?
  payload string | null?
}

class DiscoverToolsNextOutput {
  tools ToolDiscoveryRecordDto[]
  done bool
  history_context HistoryContextV1 | null?
}

class DiscoverToolsOpenInput {
  reason string | null?
}

class DiscoverToolsSendInput {
  query string | null?
  limit int | null?
}

/// Tool card for system/callback: metadata for polymorphic Open selection.
class SystemCallbackToolCard {
  tool_name "system/callback"
  description "Schedules or cancels durable host-side callback events. Use this to ask the host to emit a future `system.callback.v1` dispatch on a stable source key through the normal event-delivery path."
  session_policy "Strict"
  input_summary "{}"
  tags string[] @description("Tool tags: system, callback, scheduler")
}

class SystemCallbackOpenStep {
  op "Open"
  tool_name "system/callback" @description("Tool to open")
}

class SystemCallbackSendStep {
  op "Send"
  input CallbackToolInput @description("Payload for this step.")
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
}

class SystemCallbackReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SystemCallbackFinishStep {
  op "Finish"
}

class SystemCallbackAbortStep {
  op "Abort"
}

type SystemCallbackSessionStep = SystemCallbackOpenStep | SystemCallbackSendStep | SystemCallbackReadStep | SystemCallbackFinishStep | SystemCallbackAbortStep

class SystemCallbackSessionPlan {
  step SystemCallbackSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again.")
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
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
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
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
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
}

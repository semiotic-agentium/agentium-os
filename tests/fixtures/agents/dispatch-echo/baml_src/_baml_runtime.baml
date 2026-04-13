// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. Read: archive_ref @N from a Send for this tool (same session or conversation history). Large body: set grep; small limit; offset to page.
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - Read needs a real @N from a Send for this tool (may be earlier in history). Large @N: grep first, then limit/offset — do not dump whole archives.
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
  citations string[]? @description("Optional refs (#N, @N) for observability; use @N:L when a step depends on specific tool lines; omit or [].")
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

/// Opaque JSON transport wrapper for host-managed tools and event payloads.
/// Generated BAML callers pass serialized JSON in `__baml_opaque_json`.
/// Direct host-side callers may still send arbitrary raw JSON.
class OpaqueJson {
  opaque_json string @alias("__baml_opaque_json") @description("Serialized JSON payload.")
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
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Archive deref input for Read steps.
/// archive_ref is required: use the @N ref from the Send result.
class ArchiveReadInput {
  archive_ref string @description("Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every Read must use a real @N; do not invent refs.")
  offset int? @description("0-based line offset after grep. Page with limit: first window offset=0; next offset += previous limit while lines or matches remain.")
  limit int? @description("Max lines in this Read window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded). Set grep before large windows.")
  grep string? @description("Large @N: set this first — line filter (substring or regex per host), e.g. deploy or -i deploy. Then page with offset/limit. Do not issue repeated wide Reads without a pattern.")
}
// Domain types generated from JSON schemas
class CallbackToolInputVariant1 {
  after_ms int
  source_key string
  payload OpaqueJson @description("Opaque JSON transport value. Direct host callers may send raw JSON; generated BAML callers should use the OpaqueJson wrapper.")
  dedupe_key string | null?
  continuation string | null?
  op string
}

class CallbackToolInputVariant2 {
  callback_id string | null?
  source_key string | null?
  dedupe_key string | null?
  op string
}

type CallbackToolInput = CallbackToolInputVariant1 | CallbackToolInputVariant2

class CallbackToolOutputVariant1 {
  callback_id string
  source_key string
  scheduled_for_unix_ms int
  deduped bool
  dispatch_context_id string | null?
  dispatch_task_id string | null?
  scheduling_context_id string | null?
  scheduling_task_id string | null?
  outcome string
}

class CallbackToolOutputVariant2 {
  cancelled bool
  callback_id string | null?
  source_key string | null?
  dedupe_key string | null?
  outcome string
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
  payload map<string, OpaqueJson> | null?
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
  input_summary "oneOf({ after_ms: integer, continuation?: anyOf(enum(\"Detached\" | \"ResumeCurrentTask\") | null), dedupe_key?: anyOf(string | null), op: \"schedule\", payload: OpaqueJson, source_key: string } | { callback_id?: anyOf(string | null), dedupe_key?: anyOf(string | null), op: \"cancel\", source_key?: anyOf(string | null) })"
  tags string[] @description("Tool tags: system, callback, scheduler")
}

class SystemCallbackOpenStep {
  op "Open"
  tool_name "system/callback" @description("Tool to open")
}

class SystemCallbackSendStep {
  op "Send"
  input CallbackToolInput @description("Payload for this step.")
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SystemCallbackReadStep {
  op "Read"
  input ArchiveReadInput @description("archive_ref required (@N from a Send for this tool, including earlier history). Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.")
}

class SystemCallbackFinishStep {
  op "Finish"
}

class SystemCallbackAbortStep {
  op "Abort"
}

type SystemCallbackSessionStep = SystemCallbackOpenStep | SystemCallbackSendStep | SystemCallbackReadStep | SystemCallbackFinishStep | SystemCallbackAbortStep

class SystemCallbackSessionPlan {
  step SystemCallbackSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send for new work or Read @N when the archive already exists in history; after Send (@N archived) → Finish, Read @N (large body: set grep, small limit, offset), or Send again.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for system/discover_tools: metadata for polymorphic Open selection.
class SystemDiscover_toolsToolCard {
  tool_name "system/discover_tools"
  description "Searches registered tools by lexical rank. Open session, then send query/limit; read() returns results. Send can be called multiple times."
  session_policy "Strict"
  input_summary "{ limit?: anyOf(integer | null), query?: anyOf(string | null) }"
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
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class SystemDiscover_toolsReadStep {
  op "Read"
  input ArchiveReadInput @description("archive_ref required (@N from a Send for this tool, including earlier history). Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.")
}

class SystemDiscover_toolsFinishStep {
  op "Finish"
}

class SystemDiscover_toolsAbortStep {
  op "Abort"
}

type SystemDiscover_toolsSessionStep = SystemDiscover_toolsOpenStep | SystemDiscover_toolsSendStep | SystemDiscover_toolsReadStep | SystemDiscover_toolsFinishStep | SystemDiscover_toolsAbortStep

class SystemDiscover_toolsSessionPlan {
  step SystemDiscover_toolsSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send for new work or Read @N when the archive already exists in history; after Send (@N archived) → Finish, Read @N (large body: set grep, small limit, offset), or Send again.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

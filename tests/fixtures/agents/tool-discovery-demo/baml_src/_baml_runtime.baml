// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually
//
// Host tools use a session FSM. Which ops are valid on a given model hop is defined only by the
// narrowed BAML return type for that function, not by duplicated prose. Step classes (Open/Send/…)
// and `*SendInput` / `*OpenInput` give field names and @description; see the shared prelude
// and each session-plan `prompt` order: stable types, task, transcript, then ctx.output_format last.

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

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
  status string @description("FSM status: awaiting_open | just_opened | done")
  last_step_op string? @description("Previous FSM step op when known: open | send | read | finish | abort")
  last_step_status string? @description("Previous FSM step status when known: open | done | finished | aborted")
  last_archive_ref string? @description("Archive ref from the previous Send when available, e.g. @4")
  last_output_header string? @description("Compact archive header from the previous Send/Read when available")
  last_completion string? @description("Tool-specific terminal completion marker when available, e.g. DONE | INPUT_REQUIRED | INTERRUPTED")
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

/// Line-filtered archive read (emit op SearchRead).
class ArchiveSearchReadInput {
  archive_ref string @description("Which archive to open: an existing @N from conversation (Send output). This field names the handle; use PageRead/SearchRead to read the lines behind it. Do not invent refs. SearchRead/PageRead are legal with a real visible @N without Open or an active session.")
  grep string @description("Non-empty filter over rendered archive lines. SearchRead is for locating lines that match; use when you have a concrete term. Shape: substring or regex per host (e.g. name, -i error). For full contiguous slices without filtering, use PageRead.")
  offset int? @description("0-based offset over grep matches (after the filter). First window: offset=0; next: previous next_offset while matches remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("Which archive to open: an existing @N from conversation (Send output). This field names the handle; use PageRead/SearchRead to read the lines behind it. Do not invent refs. SearchRead/PageRead are legal with a real visible @N without Open or an active session.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). First window: offset=0; next: previous next_offset while lines remain.")
  limit int? @description("Max lines in this read window. Prefer a small explicit value when exploring. Omitting limit uses the host default (bounded).")
}

/// Global line-filtered archive read step. Legal before Open, during a session, or after a Send.
class ArchiveSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

/// Global contiguous archive paging step. Legal before Open, during a session, or after a Send.
class ArchivePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}
// Domain types generated from #[derive(BamlType)]
/// A binary arithmetic expression: left OP right.
/// Provide integer operands and choose one of the four arithmetic operations.
class Expression {
  left int @description("Left-hand integer operand.")
  operation MathOperation @description("Arithmetic operation to apply.")
  right int @description("Right-hand integer operand. Must not be 0 when operation is Divide.")
}

/// Arithmetic operation for a binary expression.
/// Accepted as the variant name (e.g. 'Add') or its symbolic alias ('+', '-', '*', '/').
enum MathOperation {
  Add @alias("+")
  Subtract @alias("-")
  Multiply @alias("*")
  Divide @alias("/")
}

/// Input to the calculator tool. Construct an Expression and submit it to evaluate.
class CalculatorInput {
  expression Expression @description("The binary expression to evaluate.")
}

class CalculatorOutput {
  expression string
  result float
  formatted string
}
// Domain types generated from JSON schemas
class ToolDiscoveryRecordDto {
  name string
  bundle string
  description string
  tags string[]
  event_sources string[]
}

enum HistoryContextV1Op {
  SearchRead
  PageRead
}

enum HistoryContextV1Status {
  Done
    @alias("done")
  Streaming
    @alias("streaming")
  Suspended
    @alias("suspended")
  Error
    @alias("error")
}

class HistoryContextV1 {
  hop int
  op HistoryContextV1Op
  status HistoryContextV1Status
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

/// Tool card for system/discover_tools: metadata for polymorphic Open selection.
class SystemDiscoverToolsToolCard {
  tool_name "system/discover_tools"
  description "Searches registered tools by lexical rank. Open session, then send query/limit; read() returns results. Send can be called multiple times."
  session_policy "Strict"
  input_summary "{ limit?: anyOf(integer | null), query?: anyOf(string | null) }"
  tags string[] @description("Tool tags: system, discovery")
}

class SystemDiscoverToolsOpenStep {
  op "Open"
  tool_name "system/discover_tools" @description("Tool to open")
  initial_input DiscoverToolsOpenInput? @description("Optional open payload.")
}

class SystemDiscoverToolsSendStep {
  op "Send"
  input DiscoverToolsSendInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class SystemDiscoverToolsSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class SystemDiscoverToolsPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SystemDiscoverToolsFinishStep {
  op "Finish"
}

class SystemDiscoverToolsAbortStep {
  op "Abort"
}

type SystemDiscoverToolsSessionStep = SystemDiscoverToolsOpenStep | SystemDiscoverToolsSendStep | SystemDiscoverToolsSearchReadStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsAbortStep

class SystemDiscoverToolsSessionPlan {
  step SystemDiscoverToolsSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. A @N in history is a handle until you read it. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}

/// Tool card for support/calculate: metadata for polymorphic Open selection.
class SupportCalculateToolCard {
  tool_name "support/calculate"
  description "Performs mathematical calculations. Can handle addition, subtraction, multiplication, and division."
  session_policy "Strict"
  input_summary "{ expression: { left: integer, operation: enum(\"*\" | \"+\" | \"-\" | \"/\"), right: integer } }"
  tags string[] @description("Tool tags: support, calculate")
}

class SupportCalculateOpenStep {
  op "Open"
  tool_name "support/calculate" @description("Tool to open")
}

class SupportCalculateSendStep {
  op "Send"
  input CalculatorInput @description("Payload for this step.")
  citations string[] @description("Cite what informed this Send. History: #N. For archive evidence, prefer @N:L (or @N:L1–L2) only after you have read those lines via PageRead/SearchRead. Do not use a bare @N as proof of archive content you have not read. Counter-evidence: ! prefix.")
}

class SupportCalculateSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("Read a filtered window of an existing archive: required archive_ref and non-empty grep. Why: the summary line is not the text; this step materializes matching lines. If the prior read shows offset=, continue with that offset. For contiguous unfiltered text, use PageRead instead.")
}

class SupportCalculatePageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("Read a contiguous window of an existing archive: required archive_ref; omit grep. Why: the summary line is not the body; this step materializes lines. The host line may show offset= for the next page. For line filtering, use SearchRead.")
}

class SupportCalculateFinishStep {
  op "Finish"
}

class SupportCalculateAbortStep {
  op "Abort"
}

type SupportCalculateSessionStep = SupportCalculateOpenStep | SupportCalculateSendStep | SupportCalculateSearchReadStep | SupportCalculatePageReadStep | SupportCalculateFinishStep | SupportCalculateAbortStep

class SupportCalculateSessionPlan {
  step SupportCalculateSessionStep @description("Emit one step. Legal `op` and field shapes: this return type and the `*OpenStep` / `*SendStep` / `SearchRead` / `PageRead` / `Finish` / `Abort` classes in this file — do not paraphrase JSON field lists in the prompt. A @N in history is a handle until you read it. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseDiscoverToolsQuery__select(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscoverToolsOpenStep {

  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

Phase: SELECT — read a visible archive (@N) or Open a tool session; legal ops only as in the narrowed union below.

Open a session with: system/discover_tools.

Find tools that can help with: {{ user_message }}
For Send: set input.query to a concise search term derived from the user message.

Session discipline: discover_tools returns matching tools in the Send (and optional Read) archive.
Once the tool list for the user's query is present in history, use the Continue hop to emit Finish and close the session.
Do not issue extra Reads or Sends unless the archive explicitly indicates more pages or incomplete results.

{% for message in ctx.tags['conversation_history'] %}
{{ _.role(message.role) }}
{{ message.content }}
{% endfor %}

---
Narrowed return union for this hop only:
- SystemDiscoverToolsOpenStep

{{ ctx.output_format }}


PHASE CONSTRAINT (select — open only): The JSON root must be exactly one bare Open step: op must be Open; set tool_name and initial_input per the narrowed return type for this hop. Do NOT emit Report or AskUser. Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send, SearchRead, PageRead, or Abort (system/discover_tools).
function ChooseDiscoverToolsQuery__act__system_discover_tools(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscoverToolsSendStep | SystemDiscoverToolsSearchReadStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsAbortStep {

  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

Phase: ACT — first post-Open hop for system/discover_tools: SearchRead, PageRead, or Send; legal ops only as below.

A system/discover_tools session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.

Find tools that can help with: {{ user_message }}
For Send: set input.query to a concise search term derived from the user message.

Session discipline: discover_tools returns matching tools in the Send (and optional Read) archive.
Once the tool list for the user's query is present in history, use the Continue hop to emit Finish and close the session.
Do not issue extra Reads or Sends unless the archive explicitly indicates more pages or incomplete results.

{% for message in ctx.tags['conversation_history'] %}
{{ _.role(message.role) }}
{{ message.content }}
{% endfor %}

---
Narrowed return union for this hop only:
- SystemDiscoverToolsSendStep
- SystemDiscoverToolsSearchReadStep
- SystemDiscoverToolsPageReadStep

{{ ctx.output_format }}


PHASE CONSTRAINT (act — Send, Abort, or archive read): The JSON root must be exactly one Send, SearchRead, PageRead, or Abort step. op must be exactly the string Send, SearchRead, PageRead, or Abort — never Read (Read is not a legal op). Include input and citations per schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N).
"#

}

/// Phase: continue — SearchRead/PageRead, send again, finish, or abort system/discover_tools.
function ChooseDiscoverToolsQuery__continue__system_discover_tools(
  user_message: string,
  session_context: SessionContext
) -> SystemDiscoverToolsSendStep | SystemDiscoverToolsSearchReadStep | SystemDiscoverToolsPageReadStep | SystemDiscoverToolsFinishStep | SystemDiscoverToolsAbortStep {

  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

Phase: CONTINUE — for system/discover_tools: SearchRead, PageRead, re-Send, or Finish; legal ops only as below.

system/discover_tools result is archived.
Check session history:
- See "@N system/discover_tools" followed by numbered lines → content is inline; emit Finish
- See "@N system/discover_tools" with "more lines" indicator → emit SearchRead or PageRead to paginate
- See "@N system/discover_tools" with no content yet → emit SearchRead or PageRead with archive_ref="@N"
- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern

Find tools that can help with: {{ user_message }}
For Send: set input.query to a concise search term derived from the user message.

Session discipline: discover_tools returns matching tools in the Send (and optional Read) archive.
Once the tool list for the user's query is present in history, use the Continue hop to emit Finish and close the session.
Do not issue extra Reads or Sends unless the archive explicitly indicates more pages or incomplete results.

{% for message in ctx.tags['conversation_history'] %}
{{ _.role(message.role) }}
{{ message.content }}
{% endfor %}

---
Narrowed return union for this hop only:
- SystemDiscoverToolsSendStep
- SystemDiscoverToolsSearchReadStep
- SystemDiscoverToolsPageReadStep
- SystemDiscoverToolsFinishStep

{{ ctx.output_format }}


PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, SearchRead, PageRead, Finish, or Abort step. op must be Send, SearchRead, PageRead, Finish, or Abort — never Read. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N).
"#

}

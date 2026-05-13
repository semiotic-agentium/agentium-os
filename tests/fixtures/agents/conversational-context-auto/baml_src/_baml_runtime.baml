// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually
//
// Host tools use a session FSM. Which ops are valid on a given model hop is defined only by the
// narrowed BAML return type for that function, not by duplicated prose. Step classes (Open/Send/…)
// and `*SendInput` / `*OpenInput` give field names and @description; see the shared prelude
// and each session-plan parent `prompt`: task + domain lines only; generated per-phase executors
// add archive prefix, optional tool_schema_prelude, narrowed union, transcript, and output binding.

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

/// Terminal hop without a host tool session: answer from visible conversation archives only.
/// Do not emit when you need a fresh Open/Send — use ENTRY Open or ACTIVE Send instead.
class ReadOnlyFinishStep {
  op "ReadOnlyFinish"
  reply StructuredReply @description("User-visible answer; citations must include every @N archive line evidence relied on — omitting citations when grounding on archives is invalid.")
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

/// Phase: entry — archive reuse, read-only finish, or Open a tool session.
function ChooseCalcTool__entry(
  user_message: string,
  session_context: SessionContext
) -> ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | SupportCalculateOpenStep {

  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}Phase: ENTRY — reuse a visible archive (@N), ReadOnlyFinish without Open, or Open a tool session; legal ops only as in the narrowed union below.

ENTRY: reuse a visible tool archive (@N) via SearchRead/PageRead, answer with ReadOnlyFinish when no new Open is needed, or Open a session with: support/calculate.

The user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
{{ _.role('user') }}
{{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

---
Narrowed return union for this hop only:
- ArchivePageReadStep
- ArchiveSearchReadStep
- ReadOnlyFinishStep
- SupportCalculateOpenStep

Emit exactly one JSON object matching one of the named types above, using the field shapes defined in the Tool and session-step types catalog at the top of this prompt. No markdown, no prose.


PHASE CONSTRAINT (entry): The JSON root must match ONLY this hop: Report, AskUser, ReadOnlyFinish (StructuredReply), ArchiveSearchReadStep, ArchivePageReadStep, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for session plans on the full Choose* function, not this narrowed hop. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N). ReadOnlyFinish is terminal — no host tool session; cite every @N used.
"#

}

/// Phase: active — Send, archive reads, Finish, or Abort (support/calculate).
function ChooseCalcTool__active__support_calculate(
  user_message: string,
  session_context: SessionContext
) -> SupportCalculateSendStep | SupportCalculateSearchReadStep | SupportCalculatePageReadStep | SupportCalculateFinishStep | SupportCalculateAbortStep {

  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}Phase: ACTIVE — for support/calculate: Send, SearchRead, PageRead, Finish (after Done Send only), or Abort; legal ops only as below.

A support/calculate session is open. Emit Send for new work, or SearchRead/PageRead an existing @N archive when tool output is already archived — do not re-Send the same listing.
support/calculate result is archived.
Check session history:
- See "@N support/calculate" followed by numbered lines → content is inline; emit Finish
- See "@N support/calculate" with "more lines" indicator → emit SearchRead or PageRead to paginate
- See "@N support/calculate" with no content yet → emit SearchRead or PageRead with archive_ref="@N"
- Large or unknown @N: set grep, small limit, offset to page; do not open wide PageRead windows without a pattern

The user says: {{ user_message }}
Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
{{ _.role('user') }}
{{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

---
Narrowed return union for this hop only:
- SupportCalculateSendStep
- SupportCalculateSearchReadStep
- SupportCalculatePageReadStep
- SupportCalculateFinishStep
- SupportCalculateAbortStep

Emit exactly one JSON object matching one of the named types above, using the field shapes defined in the Tool and session-step types catalog at the top of this prompt. No markdown, no prose.


PHASE CONSTRAINT (active): The JSON root must be exactly one Send, SearchRead, PageRead, Finish, or Abort step. op must be Send, SearchRead, PageRead, Finish, or Abort — never Read. Include input and citations per schema. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For SearchRead use ArchiveSearchReadInput; for PageRead use ArchivePageReadInput (archive_ref uses @N — never #N). Finish is legal only after the host has a Done result from Send for this session (the runtime enforces this).
"#

}



// ── builder: agent-wide tool schema catalog (from IR) ──

/// Auto-generated agent-wide tool schema catalog.
/// Rendered once at build time into _baml_tool_schema_catalog.txt and loaded as ctx.tags['tool_schema_prelude'].
/// Never invoked against an LLM — exists only to drive BAML's ctx.output_format renderer.
function AgentToolSchemaCatalog__bamlrt() -> ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | SupportCalculateAbortStep | SupportCalculateFinishStep | SupportCalculateOpenStep | SupportCalculateSendStep {
  client DefaultClient
  prompt #"
{{ ctx.output_format }}
"#
}

// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. Read: Prior Send via archive_ref @N. Large body: set grep; small limit; offset to page.
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - Read needs a prior Send ref. Large @N: grep first, then limit/offset — do not dump whole archives.
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
class ClaudeToolNextOutputEvents {
  kind string
  text string
}

class HistoryContextV1 {
  hop int
  op string
  status string
  truncated bool
  cursor string | null?
  payload map<string, OpaqueJson> | null?
}

class ClaudeToolNextOutput {
  events ClaudeToolNextOutputEvents[]
  completion string | null?
  history_context HistoryContextV1 | null?
}

class ClaudeToolOpenInput {
  workspace string | null?
}

class ClaudeToolSendInputContent {
  kind string
  text string
}

class ClaudeToolSendInput {
  prompt string | null?
  content (ClaudeToolSendInputContent | ClaudeToolSendInputContent[]) | null?
}

/// Tool card for claude/dev: metadata for polymorphic Open selection.
class ClaudeDevToolCard {
  tool_name "claude/dev"
  description "Host-managed Claude streaming session. Open once, send prompt/content, then call next() until completion is DONE/INTERRUPTED or INPUT_REQUIRED for resume."
  session_policy "Strict"
  input_summary "{ content?: anyOf(oneOf(oneOf({ kind: \"text\", text: string } | { kind: \"image_url\", url: string } | { data: string, kind: \"image_base64\", media_type: string }) | oneOf({ kind: \"text\", text: string } | { kind: \"image_url\", url: string } | { data: string, kind: \"image_base64\", media_type: string })[]) | null), prompt?: anyOf(string | null) }"
  tags string[] @description("Tool tags: claude, stream, session")
}

class ClaudeDevOpenStep {
  op "Open"
  tool_name "claude/dev" @description("Tool to open")
  initial_input ClaudeToolOpenInput? @description("Optional open payload.")
}

class ClaudeDevSendStep {
  op "Send"
  input ClaudeToolSendInput @description("Conversational message payload. Set text to a non-empty string. Never null, never omit text.")
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class ClaudeDevReadStep {
  op "Read"
  input ArchiveReadInput @description("Prior Send archive: archive_ref required. Large body: set grep; use small limit; page with offset. Avoid full-archive reads unless needed.")
}

class ClaudeDevFinishStep {
  op "Finish"
}

class ClaudeDevAbortStep {
  op "Abort"
}

type ClaudeDevSessionStep = ClaudeDevOpenStep | ClaudeDevSendStep | ClaudeDevReadStep | ClaudeDevFinishStep | ClaudeDevAbortStep

class ClaudeDevSessionPlan {
  step ClaudeDevSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send (input.text must be non-empty) for new work, or Read @N when that tool archive already exists in history; after Send (@N archived) → Read @N (large archive: set grep, small limit, offset to page), Finish, or Send again. Access: write.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: session coordination (tool crates) ──

// Auto-generated session coordination — do not edit manually
// Coordinates the claude/dev session tool (Open/Send/Next/Finish/Abort).
// ClaudeDevSessionPlan is defined in _baml_runtime.baml (builder prelude).

class ClaudeDevAskUser {
  action string @description("Always 'AskUser'.")
  prompt string @description("When you need the end user to supply information you do not have, state clearly what to provide; the user's reply will be sent to the Claude session as the next Send prompt.")
}

class ClaudeDevReport {
  action string @description("Always 'Report'.")
  message string @description("Final report to return to the user (requirements + spec + dev output + validation).")
}

function ChooseClaudeDevAction(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string @description("When the last user input was a tool approval: 'approved' or 'rejected'. Empty string when not applicable. Use this to return Send (approval input) + Next or Abort. Do not use or infer any request ids."),
  session_context: SessionContext?
) -> ClaudeDevReport | ClaudeDevAskUser | ClaudeDevSessionPlan {
  client DefaultClient
  prompt #"
    You control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

    RULES:
    0. CRITICAL: When emitting ClaudeDevSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, Read, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
    1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
    2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
    3. Initialization is two-hop under host FSM:
       - On the select/Open phase, emit Open (initial_input may be null/default).
       - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
         prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
    4. user_approval_intent:
       - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
       - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
    5. When completion is DONE or INTERRUPTED:
       - if validation has not been requested yet, Send validation prompt
       - otherwise emit Report with concise requirements/build/validation summary
    6. Non-terminal sessions: Read tool output before more Send or Finish when needed. Large @N: set grep, small limit, offset to page; do not read whole archives unless necessary.
    7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
    8. Never emit Open except on the Open/select phase (session not yet open).
    9. Never emit tool_call objects.

    SESSION PLAN FORMAT: For ClaudeDevSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

    OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or ClaudeDevSessionPlan with step plus citations as required by ctx.output_format.

    {{ ctx.output_format }}

    {% if ctx.tags['conversation_history'] %}
    Conversation history:
    {% for msg in ctx.tags['conversation_history'] %}
    {{ _.role(msg.role) }}
    {{ msg.content }}
    {% endfor %}
    {% endif %}

    spec_text:
    {{ spec_text }}

    validation_criteria_json:
    {{ validation_criteria_json }}

    last_tool_output:
    {{ last_tool_output }}

    user_approval_intent (approved | rejected | empty):
    {{ user_approval_intent }}

    Session open (host FSM): {{ session_context.session_open }}
  "#
}


// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseClaudeDevAction__select(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?
) -> ClaudeDevReport | ClaudeDevAskUser | ClaudeDevOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: claude/dev.\n\nYou control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: When emitting ClaudeDevSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, Read, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
3. Initialization is two-hop under host FSM:
   - On the select/Open phase, emit Open (initial_input may be null/default).
   - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
     prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
4. user_approval_intent:
   - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
   - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
5. When completion is DONE or INTERRUPTED:
   - if validation has not been requested yet, Send validation prompt
   - otherwise emit Report with concise requirements/build/validation summary
6. Non-terminal sessions: Read tool output before more Send or Finish when needed. Large @N: set grep, small limit, offset to page; do not read whole archives unless necessary.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: For ClaudeDevSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or ClaudeDevSessionPlan with step plus citations as required by ctx.output_format.

{{ ctx.output_format }}

{% if ctx.tags['conversation_history'] %}
Conversation history:
{% for msg in ctx.tags['conversation_history'] %}
{{ _.role(msg.role) }}
{{ msg.content }}
{% endfor %}
{% endif %}

spec_text:
{{ spec_text }}

validation_criteria_json:
{{ validation_criteria_json }}

last_tool_output:
{{ last_tool_output }}

user_approval_intent (approved | rejected | empty):
{{ user_approval_intent }}

Session open (host FSM): {{ session_context.session_open }}

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send or Read claude/dev.
function ChooseClaudeDevAction__act__claude_dev(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?
) -> ClaudeDevSendStep | ClaudeDevReadStep {

  client DefaultClient
  prompt #"[ACT] A claude/dev session is open. Emit Send for new work, or Read an existing @N archive from history (grep/limit/offset) when the data is already fetched — do not re-Send the same listing.\n\nYou control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: When emitting ClaudeDevSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, Read, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
3. Initialization is two-hop under host FSM:
   - On the select/Open phase, emit Open (initial_input may be null/default).
   - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
     prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
4. user_approval_intent:
   - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
   - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
5. When completion is DONE or INTERRUPTED:
   - if validation has not been requested yet, Send validation prompt
   - otherwise emit Report with concise requirements/build/validation summary
6. Non-terminal sessions: Read tool output before more Send or Finish when needed. Large @N: set grep, small limit, offset to page; do not read whole archives unless necessary.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: For ClaudeDevSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or ClaudeDevSessionPlan with step plus citations as required by ctx.output_format.

{{ ctx.output_format }}

{% if ctx.tags['conversation_history'] %}
Conversation history:
{% for msg in ctx.tags['conversation_history'] %}
{{ _.role(msg.role) }}
{{ msg.content }}
{% endfor %}
{% endif %}

spec_text:
{{ spec_text }}

validation_criteria_json:
{{ validation_criteria_json }}

last_tool_output:
{{ last_tool_output }}

user_approval_intent (approved | rejected | empty):
{{ user_approval_intent }}

Session open (host FSM): {{ session_context.session_open }}

PHASE CONSTRAINT (act — Send or Read): The JSON root must be exactly one Send or Read step: op must be the string Send or Read, plus input and citations fields as required by the schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For Read, follow ArchiveReadInput in the prelude (grep, limit, offset) — use this when an aligned @N archive already exists in history instead of re-Sending the same fetch.
"#

}

/// Phase: continue — read, send again, or finish claude/dev.
function ChooseClaudeDevAction__continue__claude_dev(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?
) -> ClaudeDevSendStep | ClaudeDevReadStep | ClaudeDevFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] claude/dev result is archived.\nCheck session history:\n- See \"@N claude/dev\" followed by numbered lines → content is inline; emit Finish\n- See \"@N claude/dev\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N claude/dev\" with no content yet → emit Read archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\n\nYou control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: When emitting ClaudeDevSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, Read, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
3. Initialization is two-hop under host FSM:
   - On the select/Open phase, emit Open (initial_input may be null/default).
   - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
     prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
4. user_approval_intent:
   - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
   - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
5. When completion is DONE or INTERRUPTED:
   - if validation has not been requested yet, Send validation prompt
   - otherwise emit Report with concise requirements/build/validation summary
6. Non-terminal sessions: Read tool output before more Send or Finish when needed. Large @N: set grep, small limit, offset to page; do not read whole archives unless necessary.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: For ClaudeDevSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or ClaudeDevSessionPlan with step plus citations as required by ctx.output_format.

{{ ctx.output_format }}

{% if ctx.tags['conversation_history'] %}
Conversation history:
{% for msg in ctx.tags['conversation_history'] %}
{{ _.role(msg.role) }}
{{ msg.content }}
{% endfor %}
{% endif %}

spec_text:
{{ spec_text }}

validation_criteria_json:
{{ validation_criteria_json }}

last_tool_output:
{{ last_tool_output }}

user_approval_intent (approved | rejected | empty):
{{ user_approval_intent }}

Session open (host FSM): {{ session_context.session_open }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, Read, or Finish step. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For Read, follow ArchiveReadInput in the prelude (grep, limit, offset).
"#

}

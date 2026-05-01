// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. SearchRead: line-filtered archive read (grep required). Page with offset/limit over matches.
// 4. PageRead: contiguous archive paging (no grep). Page with offset/limit over full rendered lines.
// 5. Finish: Closes the session gracefully
// 6. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - SearchRead/PageRead need a real @N from a Send for this tool (may be earlier in history). Large @N: SearchRead with grep first, then PageRead for detail — do not dump whole archives with PageRead alone.
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

/// Runtime session state injected by the step-executor loop.
/// Do not construct manually — values come from the FSM.
class SessionContext {
  contract_version string
  session_open bool
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
  archive_ref string @description("Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every SearchRead/PageRead must use a real @N; do not invent refs.")
  grep string @description("Required non-empty line filter (substring or regex per host), e.g. deploy or -i deploy. Use SearchRead to locate lines; follow with PageRead when you need contiguous context.")
  offset int? @description("0-based offset counting lines that matched grep (after filter). Page with limit: first window offset=0; next offset = previous next_offset while matches remain.")
  limit int? @description("Max lines in this window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded).")
}

/// Contiguous archive paging without a line filter (emit op PageRead).
class ArchivePageReadInput {
  archive_ref string @description("Required: @N referencing an existing Send archive for this tool (same session or earlier conversation history). Every SearchRead/PageRead must use a real @N; do not invent refs.")
  offset int? @description("0-based offset over full rendered archive lines (no grep). Page with limit: first window offset=0; next offset = previous next_offset while lines remain.")
  limit int? @description("Max lines in this window. Use a small explicit value when exploring (e.g. tens). Omitting limit uses the host default page size (bounded).")
}
// Domain types generated from JSON schemas
class DevClaudeExtInputContent {
  kind string
  text string
}

class DevClaudeExtInputUserInput {
  display_text string?
  prompt string?
}

class DevClaudeExtInput {
  prompt string?
  content DevClaudeExtInputContent[] | DevClaudeExtInputContent?
  userInput DevClaudeExtInputUserInput?
}

class DevClaudeExtOutputEvents {
  kind string
  text string
}

enum DevClaudeExtOutputCompletion {
  DONE
  INPUTREQUIRED
    @alias("INPUT_REQUIRED")
  INTERRUPTED
}

class DevClaudeExtOutputHistoryContextPayload {
  eventCount int?
  completion string?
}

class DevClaudeExtOutputHistoryContext {
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload DevClaudeExtOutputHistoryContextPayload?
}

class DevClaudeExtOutput {
  events DevClaudeExtOutputEvents[]?
  completion DevClaudeExtOutputCompletion?
  historyContext DevClaudeExtOutputHistoryContext?
}

/// Tool card for dev/claude-ext: metadata for polymorphic Open selection.
class DevClaudeExtToolCard {
  tool_name "dev/claude-ext"
  description "Sandboxed Claude-style streaming session external tool"
  session_policy "Strict"
  input_summary "{ content?: oneOf(oneOf({ kind: \"text\", text: string } | { kind: \"image_url\", url: string } | { data: string, kind: \"image_base64\", media_type: string })[] | oneOf({ kind: \"text\", text: string } | { kind: \"image_url\", url: string } | { data: string, kind: \"image_base64\", media_type: string })), prompt?: string, userInput?: { display_text?: string, prompt?: string } }"
  tags string[] @description("Tool tags: dev, claude-ext, external, session, streaming")
}

class DevClaudeExtOpenStep {
  op "Open"
  tool_name "dev/claude-ext" @description("Tool to open")
}

class DevClaudeExtSendStep {
  op "Send"
  input DevClaudeExtInput @description("Payload for this step.")
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class DevClaudeExtSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("archive_ref and grep required. Large body: small limit; page matches with offset. Do not use SearchRead when you need contiguous unfiltered lines — use PageRead.")
}

class DevClaudeExtPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("archive_ref required; omit grep. Contiguous paging over rendered archive lines. Use after SearchRead when you need surrounding detail.")
}

class DevClaudeExtFinishStep {
  op "Finish"
}

class DevClaudeExtAbortStep {
  op "Abort"
}

type DevClaudeExtSessionStep = DevClaudeExtOpenStep | DevClaudeExtSendStep | DevClaudeExtSearchReadStep | DevClaudeExtPageReadStep | DevClaudeExtFinishStep | DevClaudeExtAbortStep

class DevClaudeExtSessionPlan {
  step DevClaudeExtSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send for new work or SearchRead/PageRead @N when the archive exists; after Send (@N archived) → Finish, SearchRead, PageRead, or Send again. Access: write.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: session coordination (tool crates) ──

// Session coordination BAML for dev/claude-ext.
//
// Authored once by the tool author; the runner builder concatenates this into
// every agent prelude that lists "dev/claude-ext" in `manifest.json`. Mirrors
// the internal `claude/dev` provider (`crates/tools/claude/src/session_coordination.rs`)
// but uses the slug-derived class-name root `DevClaudeExt`.
//
// The host generates `DevClaudeExtSessionPlan` (and the Open/Send/SearchRead/
// PageRead/Finish/Abort step classes) from the JSON schema in `tool-metadata.json`.
// This file only declares the tool-specific terminal classes plus the
// `Choose<Tool>Action` step-executor function.

class DevClaudeExtAskUser {
  action string @description("Always 'AskUser'.")
  prompt string @description("When you need the end user to supply information you do not have, state clearly what to provide; the user's reply will be sent to the Claude session as the next Send prompt.")
}

class DevClaudeExtReport {
  action string @description("Always 'Report'.")
  message string @description("Final report to return to the user (requirements + spec + dev output + validation).")
}

function ChooseDevClaudeExtAction(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string @description("When the last user input was a tool approval: 'approved' or 'rejected'. Empty string when not applicable. Use this to return Send (approval input) + Next or Abort. Do not use or infer any request ids."),
  session_context: SessionContext?
) -> DevClaudeExtReport | DevClaudeExtAskUser | DevClaudeExtSessionPlan {
  client DefaultClient
  prompt #"
    You control a claude cli tool session that implements a spec and may run validation when the task actually requires it. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

    RULES:
    0. CRITICAL: When emitting DevClaudeExtSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, SearchRead, PageRead, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
    1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
    2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
       - Treat lightweight static checks (syntax, exact printed output, shape of example code) as sufficient for simple snippet/example requests.
       - Do not request an extra validation turn just because validation_criteria_json exists.
    3. Initialization is two-hop under host FSM:
       - On the select/Open phase, emit Open (initial_input may be null/default).
       - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
         prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
    4. user_approval_intent:
       - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
       - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
    5. When completion is DONE or INTERRUPTED:
       - if the task is a simple snippet/example or the validation criteria are lightweight static checks, emit Report immediately
       - only Send a validation prompt when the task explicitly requires execution, testing, debugging, or verification beyond the produced code
       - otherwise emit Report with concise requirements/build/validation summary
    6. Non-terminal sessions: poll tool output (SearchRead to filter, PageRead for contiguous lines) before more Send or Finish when needed. Large @N: SearchRead requires grep + limit + offset; PageRead pages rendered lines without grep.
    7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
    8. Never emit Open except on the Open/select phase (session not yet open).
    9. Never emit tool_call objects.

    SESSION PLAN FORMAT: For DevClaudeExtSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

    OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or DevClaudeExtSessionPlan with step plus citations as required by ctx.output_format.

    {{ ctx.output_format }}

    {% for message in ctx.tags['conversation_history'] %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}

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
function ChooseDevClaudeExtAction__select(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?
) -> DevClaudeExtReport | DevClaudeExtAskUser | DevClaudeExtOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: dev/claude-ext.\n\nYou control a claude cli tool session that implements a spec and may run validation when the task actually requires it. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: When emitting DevClaudeExtSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, SearchRead, PageRead, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
   - Treat lightweight static checks (syntax, exact printed output, shape of example code) as sufficient for simple snippet/example requests.
   - Do not request an extra validation turn just because validation_criteria_json exists.
3. Initialization is two-hop under host FSM:
   - On the select/Open phase, emit Open (initial_input may be null/default).
   - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
     prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
4. user_approval_intent:
   - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
   - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
5. When completion is DONE or INTERRUPTED:
   - if the task is a simple snippet/example or the validation criteria are lightweight static checks, emit Report immediately
   - only Send a validation prompt when the task explicitly requires execution, testing, debugging, or verification beyond the produced code
   - otherwise emit Report with concise requirements/build/validation summary
6. Non-terminal sessions: poll tool output (SearchRead to filter, PageRead for contiguous lines) before more Send or Finish when needed. Large @N: SearchRead requires grep + limit + offset; PageRead pages rendered lines without grep.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: For DevClaudeExtSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or DevClaudeExtSessionPlan with step plus citations as required by ctx.output_format.

{{ ctx.output_format }}

{% for message in ctx.tags['conversation_history'] %}
{{ _.role(message.role) }}
{{ message.content }}
{% endfor %}

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

/// Phase: act — first post-Open hop: Send or SearchRead/PageRead dev/claude-ext.
function ChooseDevClaudeExtAction__act__dev_claude_ext(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?
) -> DevClaudeExtSendStep | DevClaudeExtSearchReadStep | DevClaudeExtPageReadStep {

  client DefaultClient
  prompt #"[ACT] A dev/claude-ext session is open. Emit Send for new work, or Read an existing @N archive from history (grep/limit/offset) when the data is already fetched — do not re-Send the same listing.\n\nYou control a claude cli tool session that implements a spec and may run validation when the task actually requires it. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: When emitting DevClaudeExtSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, SearchRead, PageRead, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
   - Treat lightweight static checks (syntax, exact printed output, shape of example code) as sufficient for simple snippet/example requests.
   - Do not request an extra validation turn just because validation_criteria_json exists.
3. Initialization is two-hop under host FSM:
   - On the select/Open phase, emit Open (initial_input may be null/default).
   - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
     prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
4. user_approval_intent:
   - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
   - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
5. When completion is DONE or INTERRUPTED:
   - if the task is a simple snippet/example or the validation criteria are lightweight static checks, emit Report immediately
   - only Send a validation prompt when the task explicitly requires execution, testing, debugging, or verification beyond the produced code
   - otherwise emit Report with concise requirements/build/validation summary
6. Non-terminal sessions: poll tool output (SearchRead to filter, PageRead for contiguous lines) before more Send or Finish when needed. Large @N: SearchRead requires grep + limit + offset; PageRead pages rendered lines without grep.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: For DevClaudeExtSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or DevClaudeExtSessionPlan with step plus citations as required by ctx.output_format.

{{ ctx.output_format }}

{% for message in ctx.tags['conversation_history'] %}
{{ _.role(message.role) }}
{{ message.content }}
{% endfor %}

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

/// Phase: continue — SearchRead/PageRead, send again, or finish dev/claude-ext.
function ChooseDevClaudeExtAction__continue__dev_claude_ext(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?
) -> DevClaudeExtSendStep | DevClaudeExtSearchReadStep | DevClaudeExtPageReadStep | DevClaudeExtFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] dev/claude-ext result is archived.\nCheck session history:\n- See \"@N dev/claude-ext\" followed by numbered lines → content is inline; emit Finish\n- See \"@N dev/claude-ext\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N dev/claude-ext\" with no content yet → emit Read archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\n\nYou control a claude cli tool session that implements a spec and may run validation when the task actually requires it. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: When emitting DevClaudeExtSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, SearchRead, PageRead, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
1. CRITICAL: This hop calls a phase-specific BAML function — its return type is narrowed to one fragment shape. Emit exactly that shape.
2. spec_text is the development objective. validation_criteria_json lists pass/fail checks.
   - Treat lightweight static checks (syntax, exact printed output, shape of example code) as sufficient for simple snippet/example requests.
   - Do not request an extra validation turn just because validation_criteria_json exists.
3. Initialization is two-hop under host FSM:
   - On the select/Open phase, emit Open (initial_input may be null/default).
   - Next hop, on the Send phase and last_tool_output is empty, emit Send with:
     prompt = spec_text + "\n\nImplement this specification. Ask for clarification only when necessary. Keep code and technical output precise."
4. user_approval_intent:
   - approved -> Send userInput { kind: "toolApproval", approved: true } (optional short prompt allowed)
   - rejected -> Abort with short reason, or Send userInput { kind: "toolApproval", approved: false, reason: "..." } if continuing
5. When completion is DONE or INTERRUPTED:
   - if the task is a simple snippet/example or the validation criteria are lightweight static checks, emit Report immediately
   - only Send a validation prompt when the task explicitly requires execution, testing, debugging, or verification beyond the produced code
   - otherwise emit Report with concise requirements/build/validation summary
6. Non-terminal sessions: poll tool output (SearchRead to filter, PageRead for contiguous lines) before more Send or Finish when needed. Large @N: SearchRead requires grep + limit + offset; PageRead pages rendered lines without grep.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: For DevClaudeExtSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or DevClaudeExtSessionPlan with step plus citations as required by ctx.output_format.

{{ ctx.output_format }}

{% for message in ctx.tags['conversation_history'] %}
{{ _.role(message.role) }}
{{ message.content }}
{% endfor %}

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


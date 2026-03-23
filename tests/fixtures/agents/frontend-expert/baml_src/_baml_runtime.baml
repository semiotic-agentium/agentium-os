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
  allowed_ops string[]
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
  parts ReplyPart[] @description("Ordered parts: start with a TextPart (type \"text\") for the main user-visible Markdown; optionally append a DataPart (type \"data\", media_type application/json) whose data field is a JSON string for machine-readable UI payloads.")
  metadata string? @description("Optional opaque metadata string (e.g. JSON). Usually omit.")
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
enum ClaudeCompletion {
  DONE
  INPUTREQUIRED
    @alias("INPUT_REQUIRED")
  INTERRUPTED
}

class ClaudeEventDtoVariant1 {
  text string
  kind string
}

class ClaudeEventDtoVariant2 {
  thinking string
  kind string
}

class ClaudeEventDtoVariant3 {
  id string
  name string
  input string
  kind string
}

class ClaudeEventDtoVariant4 {
  tool_use_id string
  content string?
  is_error bool?
  kind string
}

class ClaudeEventDtoVariant5 {
  subtype string
  cwd string
  model string
  data string?
  kind string
}

class ClaudeEventDtoVariant6 {
  event string
  kind string
}

class ClaudeEventDtoVariant7 {
  subtype string
  is_error bool
  num_turns int
  total_cost_usd float?
  result string?
  kind string
}

type ClaudeEventDto = ClaudeEventDtoVariant1 | ClaudeEventDtoVariant2 | ClaudeEventDtoVariant3 | ClaudeEventDtoVariant4 | ClaudeEventDtoVariant5 | ClaudeEventDtoVariant6 | ClaudeEventDtoVariant7

class ClaudeToolNextOutput {
  events ClaudeEventDto[]
  completion ClaudeCompletion | null?
  historyContext HistoryContextV1 | null?
}

class ClaudeToolOpenInput {
  workspace string?
}

class ClaudeToolSendInput {
  prompt string?
  content ClaudeUserContentBlockDto[]?
}

class ClaudeUserContentBlockDtoVariant1 {
  text string
  kind string
}

class ClaudeUserContentBlockDtoVariant2 {
  url string
  kind string
}

class ClaudeUserContentBlockDtoVariant3 {
  media_type string
  data string
  kind string
}

type ClaudeUserContentBlockDto = ClaudeUserContentBlockDtoVariant1 | ClaudeUserContentBlockDtoVariant2 | ClaudeUserContentBlockDtoVariant3

class HistoryContextV1 {
  hop int
  op string
  status string
  truncated bool?
  cursor string?
  payload string?
}

/// Tool card for claude/dev: metadata for polymorphic Open selection.
class ClaudeDevToolCard {
  tool_name "claude/dev"
  description "Host-managed Claude streaming session. Open once, send prompt/content, then call next() until completion is DONE/INTERRUPTED or INPUT_REQUIRED for resume."
  session_policy "Strict"
  input_summary "{ content?: any, prompt?: any }"
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
  citations string[] @description("Evidence refs grounding this Send action. #N = session/history lines; @N = archived tool output; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! for counter-evidence. Required: cite what informed this action.")
}

class ClaudeDevReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class ClaudeDevFinishStep {
  op "Finish"
}

class ClaudeDevAbortStep {
  op "Abort"
}

type ClaudeDevSessionStep = ClaudeDevOpenStep | ClaudeDevSendStep | ClaudeDevReadStep | ClaudeDevFinishStep | ClaudeDevAbortStep

class ClaudeDevSessionPlan {
  step ClaudeDevSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send (input.text MUST be non-empty); Send done → Finish or Read @N. Access: write.")
  citations string[] @description("History refs justifying this decision. #N = session/history lines (user, assistant, tool-calls); @N = archived Send/tool output only; @N:L / @N:L1-L2 for lines inside an archive. Prefix with ! (e.g. !#N or !@N) for counter-evidence that this decision overrides. Copy each ref exactly as labeled. Do not use # for archives or @ for history—these prefixes are different namespaces.")
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
    0. CRITICAL: Emit exactly one step object per reply (field name: step). Never return a steps array.
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
       - otherwise return Report with concise requirements/build/validation summary
    6. For active non-terminal sessions, prefer Read to consume tool output before additional Send/Finish.
    7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
    8. Never emit Open except on the Open/select phase (session not yet open).
    9. Never emit tool_call objects.

    SESSION PLAN FORMAT: one-step session fragment only (`step`). Keep reason short when present.

    OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: {"action":"Report","message":"..."} or {"action":"AskUser","prompt":"..."} or {"step":{op,...}}.

    {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    Conversation history:
    {% for msg in ctx.tags.conversation_history %}
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
  session_context: SessionContext?,
  session_context: SessionContext
) -> ClaudeDevReport | ClaudeDevAskUser | ClaudeDevOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: claude/dev.\n\nYou control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: Emit exactly one step object per reply (field name: step). Never return a steps array.
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
   - otherwise return Report with concise requirements/build/validation summary
6. For active non-terminal sessions, prefer Read to consume tool output before additional Send/Finish.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: one-step session fragment only (`step`). Keep reason short when present.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: {"action":"Report","message":"..."} or {"action":"AskUser","prompt":"..."} or {"step":{op,...}}.

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
Conversation history:
{% for msg in ctx.tags.conversation_history %}
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

Session open (host FSM): {{ session_context.session_open }}"#

}

/// Phase: act — send query to claude/dev.
function ChooseClaudeDevAction__act__claude_dev(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?,
  session_context: SessionContext
) -> ClaudeDevSendStep {

  client DefaultClient
  prompt #"[SEND] A claude/dev session is open. Emit Send with your query.\n\nYou control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: Emit exactly one step object per reply (field name: step). Never return a steps array.
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
   - otherwise return Report with concise requirements/build/validation summary
6. For active non-terminal sessions, prefer Read to consume tool output before additional Send/Finish.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: one-step session fragment only (`step`). Keep reason short when present.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: {"action":"Report","message":"..."} or {"action":"AskUser","prompt":"..."} or {"step":{op,...}}.

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
Conversation history:
{% for msg in ctx.tags.conversation_history %}
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

Session open (host FSM): {{ session_context.session_open }}"#

}

/// Phase: continue — read, send again, or finish claude/dev.
function ChooseClaudeDevAction__continue__claude_dev(
  spec_text: string,
  validation_criteria_json: string,
  last_tool_output: string,
  user_approval_intent: string,
  session_context: SessionContext?,
  session_context: SessionContext
) -> ClaudeDevSendStep | ClaudeDevReadStep | ClaudeDevFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] claude/dev result is archived.\nCheck session history:\n- See \"@N claude/dev\" followed by numbered lines → content is inline; emit Finish\n- See \"@N claude/dev\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N claude/dev\" with no content yet → emit Read archive_ref=\"@N\"\n\nYou control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

RULES:
0. CRITICAL: Emit exactly one step object per reply (field name: step). Never return a steps array.
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
   - otherwise return Report with concise requirements/build/validation summary
6. For active non-terminal sessions, prefer Read to consume tool output before additional Send/Finish.
7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
8. Never emit Open except on the Open/select phase (session not yet open).
9. Never emit tool_call objects.

SESSION PLAN FORMAT: one-step session fragment only (`step`). Keep reason short when present.

OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: {"action":"Report","message":"..."} or {"action":"AskUser","prompt":"..."} or {"step":{op,...}}.

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
Conversation history:
{% for msg in ctx.tags.conversation_history %}
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

Session open (host FSM): {{ session_context.session_open }}"#

}

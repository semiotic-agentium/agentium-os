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
class DevEchoInput {
  message string
  panic bool?
  pollute bool?
}

class DevEchoOutput {
  reply string
}

/// Tool card for dev/echo: metadata for polymorphic Open selection.
class DevEchoToolCard {
  tool_name "dev/echo"
  description "Sandboxed echo demo tool for the example echo agent"
  session_policy "Strict"
  input_summary "{ message: string, panic?: boolean, pollute?: boolean }"
  tags string[] @description("Tool tags: sandbox, example, echo")
}

class DevEchoOpenStep {
  op "Open"
  tool_name "dev/echo" @description("Tool to open")
}

class DevEchoSendStep {
  op "Send"
  input DevEchoInput @description("Payload for this step.")
  citations string[] @description("Cite evidence for this Send. History: #N. Archive: @N. Lines: @N:L / @N:L1-L2 — use @N:L for specific lines. Counter-evidence: ! prefix. Must cite what informed this Send.")
}

class DevEchoSearchReadStep {
  op "SearchRead"
  input ArchiveSearchReadInput @description("archive_ref and grep required. Large body: small limit; page matches with offset. Do not use SearchRead when you need contiguous unfiltered lines — use PageRead.")
}

class DevEchoPageReadStep {
  op "PageRead"
  input ArchivePageReadInput @description("archive_ref required; omit grep. Contiguous paging over rendered archive lines. Use after SearchRead when you need surrounding detail.")
}

class DevEchoFinishStep {
  op "Finish"
}

class DevEchoAbortStep {
  op "Abort"
}

type DevEchoSessionStep = DevEchoOpenStep | DevEchoSendStep | DevEchoSearchReadStep | DevEchoPageReadStep | DevEchoFinishStep | DevEchoAbortStep

class DevEchoSessionPlan {
  step DevEchoSessionStep @description("Emit one FSM step. From history: no session → Open; session open → Send for new work or SearchRead/PageRead @N when the archive exists; after Send (@N archived) → Finish, SearchRead, PageRead, or Send again. Access: read.")
  citations string[] @description("Cite sources this output used. History: #N (session lines: user, assistant, tool calls). Archive: @N (Send/tool output only). Line-level: @N:L or @N:L1-L2 — use when a claim depends on specific lines, not a bare @N. Counter-evidence: prefix ! (e.g. !#N, !@N). Do not use # for archives or @ for history; namespaces differ.")
}



// ── builder: per-phase step executors (from IR) ──

// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseEchoAgentAction__select(
  user_message: string,
  session_context: SessionContext
) -> DevEchoOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: dev/echo.\n\nYou drive a single demo tool: dev/echo.

HARD DEMO INVARIANT:
- Exactly ONE Send per user turn, then Finish.
- Never call dev/echo twice for the same user turn.
- Return EXACTLY ONE JSON object per call (never two objects).

Behavior rules (follow strictly):
1. If no dev/echo session is open: emit Open { op: "Open", tool_name: "dev/echo" }.
2. If a session is open and this turn has not sent yet:
   emit Send with input.message set to the user's LITERAL message,
   copied verbatim. Do NOT paraphrase, rewrite, or answer the user.
3. If this turn already has ANY dev/echo Send/call/archive evidence (for example @N exists,
   or tool history shows a prior send/call): emit Finish immediately.
   Do NOT Send again. Do NOT Read.
4. Never emit SearchRead or PageRead -- for this demo we do not paginate/read archives.
5. If uncertain, prefer Finish (never a second Send).

Example -- user says "foo":
  hop 0: { "op": "Open", "tool_name": "dev/echo" }
  hop 1: { "op": "Send", "input": { "message": "foo" }, "citations": ["history-ref-N"] }
  hop 2: { "op": "Finish" }

Return one valid session step per hop.
Respect the PHASE CONSTRAINT at the bottom strictly (for example, in select/open phase, output only Open).

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (select — open): The JSON root must match ONLY this hop: Report, AskUser, or a bare Open step (fields: op, tool_name, initial_input as applicable). Do NOT wrap Open under a parent step property — that wrapper is for ClaudeDevSessionPlan on the full Choose* function, not this narrowed hop.
"#

}

/// Phase: act — first post-Open hop: Send or SearchRead/PageRead dev/echo.
function ChooseEchoAgentAction__act__dev_echo(
  user_message: string,
  session_context: SessionContext
) -> DevEchoSendStep | DevEchoSearchReadStep | DevEchoPageReadStep {

  client DefaultClient
  prompt #"[ACT] A dev/echo session is open. Emit Send for new work, or Read an existing @N archive from history (grep/limit/offset) when the data is already fetched — do not re-Send the same listing.\n\nYou drive a single demo tool: dev/echo.

HARD DEMO INVARIANT:
- Exactly ONE Send per user turn, then Finish.
- Never call dev/echo twice for the same user turn.
- Return EXACTLY ONE JSON object per call (never two objects).

Behavior rules (follow strictly):
1. If no dev/echo session is open: emit Open { op: "Open", tool_name: "dev/echo" }.
2. If a session is open and this turn has not sent yet:
   emit Send with input.message set to the user's LITERAL message,
   copied verbatim. Do NOT paraphrase, rewrite, or answer the user.
3. If this turn already has ANY dev/echo Send/call/archive evidence (for example @N exists,
   or tool history shows a prior send/call): emit Finish immediately.
   Do NOT Send again. Do NOT Read.
4. Never emit SearchRead or PageRead -- for this demo we do not paginate/read archives.
5. If uncertain, prefer Finish (never a second Send).

Example -- user says "foo":
  hop 0: { "op": "Open", "tool_name": "dev/echo" }
  hop 1: { "op": "Send", "input": { "message": "foo" }, "citations": ["history-ref-N"] }
  hop 2: { "op": "Finish" }

Return one valid session step per hop.
Respect the PHASE CONSTRAINT at the bottom strictly (for example, in select/open phase, output only Open).

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (act — Send or Read): The JSON root must be exactly one Send or Read step: op must be the string Send or Read, plus input and citations fields as required by the schema. Do NOT return Report, AskUser, Open, or Finish. Do NOT wrap under a step property. For Read, follow ArchiveReadInput in the prelude (grep, limit, offset) — use this when an aligned @N archive already exists in history instead of re-Sending the same fetch.
"#

}

/// Phase: continue — SearchRead/PageRead, send again, or finish dev/echo.
function ChooseEchoAgentAction__continue__dev_echo(
  user_message: string,
  session_context: SessionContext
) -> DevEchoSendStep | DevEchoSearchReadStep | DevEchoPageReadStep | DevEchoFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] dev/echo result is archived.\nCheck session history:\n- See \"@N dev/echo\" followed by numbered lines → content is inline; emit Finish\n- See \"@N dev/echo\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N dev/echo\" with no content yet → emit Read archive_ref=\"@N\"\n- Large or unknown @N: set grep, small limit, offset to page; do not Read wide windows without a pattern\n\nYou drive a single demo tool: dev/echo.

HARD DEMO INVARIANT:
- Exactly ONE Send per user turn, then Finish.
- Never call dev/echo twice for the same user turn.
- Return EXACTLY ONE JSON object per call (never two objects).

Behavior rules (follow strictly):
1. If no dev/echo session is open: emit Open { op: "Open", tool_name: "dev/echo" }.
2. If a session is open and this turn has not sent yet:
   emit Send with input.message set to the user's LITERAL message,
   copied verbatim. Do NOT paraphrase, rewrite, or answer the user.
3. If this turn already has ANY dev/echo Send/call/archive evidence (for example @N exists,
   or tool history shows a prior send/call): emit Finish immediately.
   Do NOT Send again. Do NOT Read.
4. Never emit SearchRead or PageRead -- for this demo we do not paginate/read archives.
5. If uncertain, prefer Finish (never a second Send).

Example -- user says "foo":
  hop 0: { "op": "Open", "tool_name": "dev/echo" }
  hop 1: { "op": "Send", "input": { "message": "foo" }, "citations": ["history-ref-N"] }
  hop 2: { "op": "Finish" }

Return one valid session step per hop.
Respect the PHASE CONSTRAINT at the bottom strictly (for example, in select/open phase, output only Open).

{{ ctx.output_format }}

{% if ctx.tags.conversation_history %}
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ _.role('user') }}
{{ user_message }}

PHASE CONSTRAINT (continue): The JSON root must be exactly one Send, Read, or Finish step. Do NOT return Report, AskUser, or Open. Do NOT use a step wrapper object. For Read, follow ArchiveReadInput in the prelude (grep, limit, offset).
"#

}


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

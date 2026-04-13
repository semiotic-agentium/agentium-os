//! Session coordination BAML for the claude/dev tool.
//!
//! Defines the BAML classes and prompt used to drive the claude/dev session from a controlling agent.

use baml_rt_core::Result;

/// Tool ID for the claude/dev session tool (used for provider registration and manifest matching).
pub const CLAUDE_DEV_TOOL_ID: &str = "claude/dev";

/// Renders the full session coordination BAML for claude/dev: classes ClaudeDevAskUser,
/// ClaudeDevReport, function ChooseClaudeDevAction, and the coordination prompt.
/// ClaudeDevSessionPlan is defined in `_baml_runtime.baml` (builder prelude).
pub fn render_claude_dev_session_coordination() -> Result<String> {
    let mut out = String::new();

    out.push_str("// Auto-generated session coordination — do not edit manually\n");
    out.push_str("// Coordinates the claude/dev session tool (Open/Send/SearchRead/PageRead/Finish/Abort).\n");
    out.push_str("// ClaudeDevSessionPlan is defined in _baml_runtime.baml (builder prelude).\n\n");

    // SessionContext is defined in _baml_runtime.baml (shared FSM host type).
    out.push_str("class ClaudeDevAskUser {\n");
    out.push_str("  action string @description(\"Always 'AskUser'.\")\n");
    out.push_str("  prompt string @description(\"When you need the end user to supply information you do not have, state clearly what to provide; the user's reply will be sent to the Claude session as the next Send prompt.\")\n");
    out.push_str("}\n\n");

    out.push_str("class ClaudeDevReport {\n");
    out.push_str("  action string @description(\"Always 'Report'.\")\n");
    out.push_str("  message string @description(\"Final report to return to the user (requirements + spec + dev output + validation).\")\n");
    out.push_str("}\n\n");

    out.push_str("function ChooseClaudeDevAction(\n");
    out.push_str("  spec_text: string,\n");
    out.push_str("  validation_criteria_json: string,\n");
    out.push_str("  last_tool_output: string,\n");
    out.push_str("  user_approval_intent: string @description(\"When the last user input was a tool approval: 'approved' or 'rejected'. Empty string when not applicable. Use this to return Send (approval input) + Next or Abort. Do not use or infer any request ids.\"),\n");
    out.push_str("  session_context: SessionContext?\n");
    out.push_str(") -> ClaudeDevReport | ClaudeDevAskUser | ClaudeDevSessionPlan {\n");
    out.push_str("  client DefaultClient\n");
    out.push_str(r##"  prompt #"
    You control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

    RULES:
    0. CRITICAL: When emitting ClaudeDevSessionPlan, use one object with step (single FSM step) and citations fields. Per-phase hops are narrowed: emit ONLY the allowed type at the JSON root — often a bare step object with op Open, Send, SearchRead, PageRead, Finish, or Abort, with NO outer step wrapper. Do not emit a steps array.
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
    6. Non-terminal sessions: poll tool output (SearchRead to filter, PageRead for contiguous lines) before more Send or Finish when needed. Large @N: SearchRead requires grep + limit + offset; PageRead pages rendered lines without grep.
    7. AskUser is only for genuine missing operator input. Keep prompt neutral and short.
    8. Never emit Open except on the Open/select phase (session not yet open).
    9. Never emit tool_call objects.

    SESSION PLAN FORMAT: For ClaudeDevSessionPlan only: one wrapper object whose step field holds a single FSM step, plus a citations array (history refs use hash-prefixed line numbers in prose; avoid embedding the literal quote-hash sequence in examples). For Report/AskUser: flat objects with action field. Do not confuse SessionPlan (wrapper) with a bare step type returned on narrowed hops.

    OUTPUT FORMAT: Respond with ONLY a single JSON object. No reasoning, no markdown, no text before or after the JSON. Use exactly one of: Report/AskUser-shaped objects, or ClaudeDevSessionPlan with step plus citations as required by ctx.output_format.

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
  "#"##);
    out.push_str("\n}\n");

    Ok(out)
}

// Register with baml-rt-tools so the builder discovers this when claude/dev is in the manifest.
inventory::submit! {
    baml_rt_tools::session_coordination::SessionCoordinationProvider {
        tool_id: CLAUDE_DEV_TOOL_ID,
        render: render_claude_dev_session_coordination,
    }
}

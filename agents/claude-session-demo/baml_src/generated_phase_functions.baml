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
  prompt #"You control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

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
  session_context: SessionContext?
) -> ClaudeDevSendStep {

  client DefaultClient
  prompt #"You control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

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
  session_context: SessionContext?
) -> ClaudeDevSendStep | ClaudeDevReadStep | ClaudeDevFinishStep {

  client DefaultClient
  prompt #"You control a claude cli tool session that implements a spec and then runs validation. Return either a final report, a request for user input, or a session plan (steps) — same pattern as other session tools.

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

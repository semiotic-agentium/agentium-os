// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseCalcTool__select(
  user_message: string,
  session_context: SessionContext?
) -> SupportCalculateOpenStep {

  client DefaultClient
  prompt #"Evaluate this calculation: {{ user_message }}
For Send: set input.expression to the mathematical expression extracted from the message. If unclear, use 2 + 3.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to support/calculate.
function ChooseCalcTool__act__support_calculate(
  user_message: string,
  session_context: SessionContext?
) -> SupportCalculateSendStep {

  client DefaultClient
  prompt #"Evaluate this calculation: {{ user_message }}
For Send: set input.expression to the mathematical expression extracted from the message. If unclear, use 2 + 3.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish support/calculate.
function ChooseCalcTool__continue__support_calculate(
  user_message: string,
  session_context: SessionContext?
) -> SupportCalculateSendStep | SupportCalculateReadStep | SupportCalculateFinishStep {

  client DefaultClient
  prompt #"Evaluate this calculation: {{ user_message }}
For Send: set input.expression to the mathematical expression extracted from the message. If unclear, use 2 + 3.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}


// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseCalcTool__select(
  user_message: string
) -> SupportCalculateOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: support/calculate.\n\nEvaluate this calculation: {{ user_message }}
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
  user_message: string
) -> SupportCalculateSendStep {

  client DefaultClient
  prompt #"[SEND] A support/calculate session is open. Emit Send with your query.\n\nEvaluate this calculation: {{ user_message }}
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
  user_message: string
) -> SupportCalculateSendStep | SupportCalculateReadStep | SupportCalculateFinishStep {

  client DefaultClient
  prompt #"[CONTINUE] support/calculate result is archived.\nCheck session history:\n- See \"@N support/calculate\" followed by numbered lines → content is inline; emit Finish\n- See \"@N support/calculate\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N support/calculate\" with no content yet → emit Read archive_ref=\"@N\"\n\nEvaluate this calculation: {{ user_message }}
For Send: set input.expression to the mathematical expression extracted from the message. If unclear, use 2 + 3.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}


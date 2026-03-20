// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseNotionAction__select(
  goal: string,
  step_description: string
) -> ReadOnlyResponse | SupportNotionOpenStep {

  client OpenRouterGPT
  prompt #"You are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to support/notion.
function ChooseNotionAction__act__support_notion(
  goal: string,
  step_description: string
) -> SupportNotionSendStep {

  client OpenRouterGPT
  prompt #"You are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish support/notion.
function ChooseNotionAction__continue__support_notion(
  goal: string,
  step_description: string
) -> SupportNotionSendStep | SupportNotionReadStep | SupportNotionFinishStep {

  client OpenRouterGPT
  prompt #"You are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}


// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function CleeseSendToChapman__select(
  first_line: string
) -> SystemInternal_a2aOpenStep {

  client DefaultClient
  prompt #"Coordinate the argument sketch with agent argument-chapman.
For Open: set initial_input.target to { agent_package: "argument-chapman", agent_instance_id: "default" }.
For Send: set input.parts to [{ "text": "{{ first_line }}" }].

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to system/internal_a2a.
function CleeseSendToChapman__act__system_internal_a2a(
  first_line: string
) -> SystemInternal_a2aSendStep {

  client DefaultClient
  prompt #"Coordinate the argument sketch with agent argument-chapman.
For Open: set initial_input.target to { agent_package: "argument-chapman", agent_instance_id: "default" }.
For Send: set input.parts to [{ "text": "{{ first_line }}" }].

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish system/internal_a2a.
function CleeseSendToChapman__continue__system_internal_a2a(
  first_line: string
) -> SystemInternal_a2aSendStep | SystemInternal_a2aReadStep | SystemInternal_a2aFinishStep {

  client DefaultClient
  prompt #"Coordinate the argument sketch with agent argument-chapman.
For Open: set initial_input.target to { agent_package: "argument-chapman", agent_instance_id: "default" }.
For Send: set input.parts to [{ "text": "{{ first_line }}" }].

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}


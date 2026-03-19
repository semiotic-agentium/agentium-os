// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function CleeseSendToChapman__select(
  first_line: string
) -> SystemInternal_a2aOpenStep {

  client DefaultClient
  prompt #"[OPEN] Open a session with: system/internal_a2a.\n\nCoordinate the argument sketch with agent argument-chapman.
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
  prompt #"[SEND] A system/internal_a2a session is open. Emit Send with your query.\n\nCoordinate the argument sketch with agent argument-chapman.
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
  prompt #"[CONTINUE] system/internal_a2a result is archived.\nCheck session history:\n- See \"@N system/internal_a2a\" followed by numbered lines → content is inline; emit Finish\n- See \"@N system/internal_a2a\" with \"more lines\" indicator → emit Read to paginate\n- See \"@N system/internal_a2a\" with no content yet → emit Read archive_ref=\"@N\"\n\nCoordinate the argument sketch with agent argument-chapman.
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


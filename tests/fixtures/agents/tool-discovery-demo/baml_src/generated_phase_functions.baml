// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseDiscoverToolsQuery__select(
  user_message: string
) -> SystemDiscover_toolsOpenStep {

  client DefaultClient
  prompt #"Find tools that can help with: {{ user_message }}
For Send: set input.query to a concise search term derived from the user message.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to system/discover_tools.
function ChooseDiscoverToolsQuery__act__system_discover_tools(
  user_message: string
) -> SystemDiscover_toolsSendStep {

  client DefaultClient
  prompt #"Find tools that can help with: {{ user_message }}
For Send: set input.query to a concise search term derived from the user message.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish system/discover_tools.
function ChooseDiscoverToolsQuery__continue__system_discover_tools(
  user_message: string
) -> SystemDiscover_toolsSendStep | SystemDiscover_toolsReadStep | SystemDiscover_toolsFinishStep {

  client DefaultClient
  prompt #"Find tools that can help with: {{ user_message }}
For Send: set input.query to a concise search term derived from the user message.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

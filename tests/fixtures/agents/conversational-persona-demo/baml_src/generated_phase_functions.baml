// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function DecideDelegationAction__select(
  goal: string,
  agent: string
) -> SystemInternal_a2aOpenStep {

  client DefaultClient
  prompt #"Coordinate a conversation with agent {{ agent }} to achieve this goal: {{ goal }}

For Open: set initial_input.target to { agent_package: "{{ agent }}", agent_instance_id: "default" }.
For Send: write the message that will achieve the goal.
Return null (string?) when the goal is satisfied or after 2+ exchanges.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to system/internal_a2a.
function DecideDelegationAction__act__system_internal_a2a(
  goal: string,
  agent: string
) -> SystemInternal_a2aSendStep {

  client DefaultClient
  prompt #"Coordinate a conversation with agent {{ agent }} to achieve this goal: {{ goal }}

For Open: set initial_input.target to { agent_package: "{{ agent }}", agent_instance_id: "default" }.
For Send: write the message that will achieve the goal.
Return null (string?) when the goal is satisfied or after 2+ exchanges.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish system/internal_a2a.
function DecideDelegationAction__continue__system_internal_a2a(
  goal: string,
  agent: string
) -> SystemInternal_a2aSendStep | SystemInternal_a2aReadStep | SystemInternal_a2aFinishStep {

  client DefaultClient
  prompt #"Coordinate a conversation with agent {{ agent }} to achieve this goal: {{ goal }}

For Open: set initial_input.target to { agent_package: "{{ agent }}", agent_instance_id: "default" }.
For Send: write the message that will achieve the goal.
Return null (string?) when the goal is satisfied or after 2+ exchanges.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: select — open a tool session.
function GetDiscoverAgentsPlan__select(
  inferred_intent: string
) -> SystemDiscover_agentsOpenStep {

  client DefaultClient
  prompt #"Discover available agents to match the user's intent.
Inferred intent: {{ inferred_intent }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to system/discover_agents.
function GetDiscoverAgentsPlan__act__system_discover_agents(
  inferred_intent: string
) -> SystemDiscover_agentsSendStep {

  client DefaultClient
  prompt #"Discover available agents to match the user's intent.
Inferred intent: {{ inferred_intent }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish system/discover_agents.
function GetDiscoverAgentsPlan__continue__system_discover_agents(
  inferred_intent: string
) -> SystemDiscover_agentsSendStep | SystemDiscover_agentsReadStep | SystemDiscover_agentsFinishStep {

  client DefaultClient
  prompt #"Discover available agents to match the user's intent.
Inferred intent: {{ inferred_intent }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

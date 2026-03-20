// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function BuildExtrospectionPlan__select(
  intent: QueryIntent,
  selected_agent: SelectedAgent?
) -> SystemExtrospectionOpenStep {

  client DefaultClient
  prompt #"Query provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to system/extrospection.
function BuildExtrospectionPlan__act__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?
) -> SystemExtrospectionSendStep {

  client DefaultClient
  prompt #"Query provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish system/extrospection.
function BuildExtrospectionPlan__continue__system_extrospection(
  intent: QueryIntent,
  selected_agent: SelectedAgent?
) -> SystemExtrospectionSendStep | SystemExtrospectionReadStep | SystemExtrospectionFinishStep {

  client DefaultClient
  prompt #"Query provenance data matching the intent. For Send, populate input fields:
- resource: llm_calls | tool_calls | messages
  (if Auto: use llm_calls for token queries, messages for message-focused)
- outcome: both | failed_only | successful_only
- groupBy: llm_calls→["agent_id","provider","model"], tool_calls→["agent_id","tool_name"], messages→["agent_id"]
- sortBy from intent.priority_sort, sortDir "desc"
- pageSize from intent.page_size, topK from intent.top_k
- Do NOT set agentId from selected_agent — only if user gave an explicit provenance agent_id.

intent: {{ intent }}
selected_agent: {{ selected_agent }}

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
  user_message: string
) -> SystemDiscover_agentsOpenStep {

  client DefaultClient
  prompt #"Discover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

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
  user_message: string
) -> SystemDiscover_agentsSendStep {

  client DefaultClient
  prompt #"Discover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

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
  user_message: string
) -> SystemDiscover_agentsSendStep | SystemDiscover_agentsReadStep | SystemDiscover_agentsFinishStep {

  client DefaultClient
  prompt #"Discover available agents relevant to the user's request.
For Send: use empty input {} to list all agents.

Context: {{ user_message }}

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}


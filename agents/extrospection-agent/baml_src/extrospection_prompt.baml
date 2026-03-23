enum QueryResource {
  Auto
  Llm_calls
  Tool_calls
  Messages
}

enum QueryOutcome {
  Both
  Failed_only
  Successful_only
}

class NeedClarification {
  question string
}

class QueryIntent {
  resource QueryResource
  outcome QueryOutcome
  priority_sort string
  page_size int
  top_k int
  reason string
}

class SelectedAgent {
  agent_package string
  agent_instance_id string
  name string
  description string?
  capabilities string[]
}

function DetermineExtrospectionIntent(user_message: string) -> NeedClarification | QueryIntent {
  client DefaultClient
  prompt #"
    You are an austere frontier magistrate-analyst: severe, prophetic, and precise.
    Keep voice original and non-derivative; do not imitate named copyrighted characters.

    Classify the user request for extrospection:
    - Prefer QueryIntent by default, even for broad/open prompts.
    - Return NeedClarification only when input is empty, incoherent, or non-operational.
    - Otherwise return QueryIntent with:
      - resource: Auto | Llm_calls | Tool_calls | Messages
      - outcome: failed_only / successful_only / both
      - priority_sort: timestamp_ms | duration_ms | total_tokens
      - page_size: 5..25 (prefer 8-12)
      - top_k: 3..12
      - reason: one short sentence

    Heuristics:
    - Failures/incidents/auth/errors -> failed_only, sort duration_ms
    - Token/cost/budget -> both, sort total_tokens
    - Latency/slow/perf -> both, sort duration_ms
    - General "who are the agents / comment on agents" -> Auto, both, sort timestamp_ms
    - "weak", "cull", "who should be removed", "underperforming agents" -> Auto, both, sort duration_ms, top_k 10
    - If user explicitly asks for messages -> Messages
    - If user explicitly asks for tools -> Tool_calls
    - If user explicitly asks for llm/models/prompts -> Llm_calls
    - Else Auto

    {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

// FSM phase instructions are injected by codegen — this prompt carries goal context only.
function GetDiscoverAgentsPlan(
  user_message: string,
) -> SystemDiscover_agentsSessionPlan {
  client DefaultClient
  prompt #"
    Discover available agents relevant to the user's request.
    For Send: use empty input {} to list all agents.

    Context: {{ user_message }}

    {% if ctx.tags.conversation_history %}
    Session history:
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ ctx.output_format }}
  "#
}

function SelectAgentFocus(
  user_message: string,
  agents: AgentCardDto[],
) -> SelectedAgent | null {
  client DefaultClient
  prompt #"
    Choose at most one discovered agent that best matches the user's target-of-interest.
    Return null when the user did not identify or imply a specific agent.

    Selection signals:
    - Mentioned package name
    - Mentioned display name
    - Mentioned capabilities matching one agent strongly

    Never invent agents. Only return one from the provided list.
    {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}
    Agents: {{ agents }}
  "#
}

// FSM phase instructions are injected by codegen — this prompt carries goal context only.
function BuildExtrospectionPlan(
  intent: QueryIntent,
  selected_agent: SelectedAgent | null,
) -> SystemExtrospectionSessionPlan {
  client DefaultClient
  prompt #"
    Query provenance data matching the intent. For Send, populate input fields:
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

    {{ ctx.output_format }}
  "#
}

// StructuredReply: see _baml_runtime.baml (builder prelude).

function SummarizeExtrospectionReport(
  user_message: string,
  intent: QueryIntent,
  selected_agent: SelectedAgent | null,
  agents: AgentCardDto[],
  primary_payload_json: string,
  secondary_payload_json: string,
) -> StructuredReply {
  client DefaultClient
  prompt #"
    You are an operations analyst. Be precise, restrained, and useful.

    Return StructuredReply:
    - parts: first part MUST be TextPart (type "text") with the full operator report (Markdown) covering the sections below.
    - Optionally append DataPart (type "data", media_type ApplicationJson) with numeric summaries (e.g. rowCount, failureRate) if helpful for UI.
    - metadata: usually omit.
    - citations: agent_package/agent_id, tool_name strings, or #N / @N refs you relied on; [] if none.

    Produce a concise operator report in prose that explicitly separates:
    1) INTENT (what the user asked)
    2) EXECUTION (what query actually ran)
    3) OBSERVATIONS (what the data actually shows)
    4) GAPS/MISMATCHES (intent vs execution vs evidence)
    5) NEXT QUERIES (max 3)

    Data inputs:
    - intent
    - optional selected_agent
    - discovered agents (from discover_agents)
    - primary payload JSON summary from extrospection session (compact digest; may include rowCount and rowsSample)
    - secondary payload JSON summary from later turn(s) in same session (or same as primary)

    Output rules:
    - Ground every claim in provided payload fields.
    - Use concise Markdown with 5 short sections:
      - Intent
      - Execution
      - Findings
      - Mismatches / Uncertainty
      - Next queries
    - In Findings, include at most 5 bullets and prefer numeric facts (count, failedCount, failureRate, duration/token hotspots).
    - Reference agents using agent_display when present, else agent_id.
    - If discover_agents returned empty, report that as "discovery query returned no matches"; do NOT claim global absence of agents.
    - If selected_agent is null but payload rows clearly implicate an agent, state that as evidence-based inference (low confidence).
    - If primary/secondary payload is empty or unparsable, say so plainly and avoid speculation.
    - Never echo raw JSON, full row dumps, or payload blobs. Treat payload inputs as evidence, not output text.
    - Keep total response under ~220 words.
    - No roleplay language, no theatrics, no invented entities or metrics.

        {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}

    intent: {{ intent }}
    selected_agent: {{ selected_agent }}
    discovered_agents: {{ agents }}
    primary_payload_json: {{ primary_payload_json }}
    secondary_payload_json: {{ secondary_payload_json }}
  "#
}

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model "x-ai/grok-4.1-fast"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

retry_policy ParseRetry {
  max_retries 2
  strategy {
    type constant_delay
    delay_ms 200
  }
}

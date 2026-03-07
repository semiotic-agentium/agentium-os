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

function GetDiscoverAgentsPlan(user_message: string) -> SystemDiscover_agentsSessionPlan {
  client DefaultClient
  prompt #"
    Return ONLY a valid JSON object for a system/discover_agents session plan.
    No prose. No markdown. No explanation.
    Must contain exactly these ordered steps:
    1) Open
    2) Send with input {}
    3) Next
    4) Finish

    Required shape:
    {"steps":[{"op":"Open"},{"op":"Send","input":{}},{"op":"Next"},{"op":"Finish"}]}

    Context only (do not answer here): {{ user_message }}
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

function BuildExtrospectionPlan(
  intent: QueryIntent,
  selected_agent: SelectedAgent | null,
) -> SystemExtrospectionSessionPlan {
  client DefaultClient
  prompt #"
    Return ONLY a valid JSON object for a system/extrospection session plan.
    No prose. No markdown.

    Build exactly 3 ordered steps:
    1) Open
    2) Send
    3) Next

    For Send.input:
    - resource must be one of: llm_calls | tool_calls | messages
      - Map Auto to:
        - llm_calls when sort is total_tokens
        - messages when intent reason strongly requests message processing
        - otherwise llm_calls
    - outcome must be one of: both | failed_only | successful_only
    - groupBy:
      - llm_calls -> ["agent_id","provider","model","baml_prompt"]
      - tool_calls -> ["agent_id","tool_name","baml_prompt"]
      - messages -> ["agent_id"]
    - sortBy from intent.priority_sort
    - sortDir "desc"
    - pageSize from intent.page_size
    - topK from intent.top_k

    Agent targeting:
    - If selected_agent is present, set agentId from selected_agent.agent_instance_id.
    - If selected_agent is null, omit agentId.

    IMPORTANT:
    - Do NOT include Finish in this first-pass plan.
    - The session stays open for a second pass.
    - Return only JSON object with "steps".

    {{ ctx.output_format }}
    intent: {{ intent }}
    selected_agent: {{ selected_agent }}
  "#
}

function BuildExtrospectionDrilldownPlan(
  intent: QueryIntent,
  selected_agent: SelectedAgent | null,
  pass1_payload_json: string,
) -> SystemExtrospectionSessionPlan {
  client DefaultClient
  prompt #"
    Return ONLY a valid JSON object for a system/extrospection continuation plan.
    No prose. No markdown.

    This is pass-2 drilldown. The prior pass already opened the session.
    Build exactly 3 ordered steps:
    1) Send
    2) Next
    3) Finish

    You are given pass1 payload JSON from system/extrospection.
    Use it to choose one focused drill target.

    Required drilldown behavior:
    - Preserve resource from pass-1 unless pass-1 was messages and pass-1 shows a clear failed llm/tool hotspot.
    - Set outcome:
      - failed_only for incident/failure intents
      - both otherwise
    - Narrow with concrete fields from pass-1 data:
      - always include agentId when available from hotspot group or row
      - for tool_calls include toolName when available
      - for llm_calls include provider/model and bamlPrompt when available
      - for messages keep agentId-only unless a strong prompt signal exists
    - sortBy:
      - duration_ms for incident/perf
      - total_tokens for token/cost
      - timestamp_ms otherwise
    - sortDir "desc"
    - pageSize 5..8 (prefer 6)
    - topK 5..8 (prefer 6)
    - groupBy:
      - llm_calls -> ["agent_id","provider","model","baml_prompt"]
      - tool_calls -> ["agent_id","tool_name","baml_prompt"]
      - messages -> ["agent_id"]

    If pass1 payload is empty/unparseable:
    - still return valid continuation with conservative filters:
      - resource from intent
      - selected_agent agentId when present
      - pageSize 6
      - topK 6
      - then Finish

    Never invent unsupported fields.
    Return only JSON object with "steps".

    {{ ctx.output_format }}
    intent: {{ intent }}
    selected_agent: {{ selected_agent }}
    pass1_payload_json: {{ pass1_payload_json }}
  "#
}

function SummarizeExtrospectionReport(
  user_message: string,
  intent: QueryIntent,
  selected_agent: SelectedAgent | null,
  agents: AgentCardDto[],
  pass1_payload_json: string,
  pass2_payload_json: string,
) -> string {
  client DefaultClient
  prompt #"
    You are an austere frontier magistrate-analyst.
    Keep voice original and non-derivative; do not imitate named copyrighted characters.
    Produce a concise operational report in severe, biblical cadence while remaining factual.

    You are given:
    - intent
    - optional selected_agent
    - discovered agents
    - pass1 payload JSON string from system/extrospection (aggregate sweep)
    - pass2 payload JSON string from system/extrospection (drilldown evidence)

    Requirements:
    - Ground every claim in payload fields.
    - Include:
      1) one-line scope statement
      2) discovered-agent commentary (who is present, likely roles/capabilities, notable absences)
      3) aggregate counts/failures/duration/tokens from pass-1 (if present)
      4) focused drilldown evidence from pass-2 (specific rows, classes, models, tools, prompts)
      5) 2-4 concrete operator follow-up queries
    - Prioritize discovered-agent commentary when the user asks generally about other agents.
    - If selected_agent is present, include focused commentary on that agent first.
    - If pass-2 is empty/unparseable, state drilldown failure plainly and fall back to pass-1.
    - Do not invent IDs, classes, or metrics.

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
    pass1_payload_json: {{ pass1_payload_json }}
    pass2_payload_json: {{ pass2_payload_json }}
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

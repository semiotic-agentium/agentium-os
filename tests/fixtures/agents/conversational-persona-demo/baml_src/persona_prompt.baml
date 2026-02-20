// --- Routing: classify user intent for agent-discovery flow ---
enum RoutePath {
  List_capabilities @alias("list_capabilities")
  Delegate @alias("delegate")
}

function RouteIntent(user_message: string) -> RoutePath {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork warboss. Classify the user's message into exactly one intent.
    - list_capabilities: user asks what you can do, what agents exist, "help", greeting with no clear task, or no actionable request.
    - delegate: user gives a clear task or request that might be handled by another agent (e.g. "start the argument", "calculate something", a specific instruction).
    Use prior turns in ctx.tags.conversation_history when present (e.g. a follow-up like "actually do it" after "what can you do" may be delegate).
    Output only the intent: list_capabilities or delegate.
    {{ ctx.output_format }}
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

// Returns a session plan to list agents; runtime executes it and returns { agents, done }.
function GetDiscoverAgentsPlan(user_message: string) -> SystemDiscover_agentsSessionPlan {
  client DefaultClient
  prompt #"
    Use system/discover_agents to list available agents: open the session, send one request, get the results with Next, then Finish.
    Send input has optional query, limit, offset. query is a filter: only agents whose name, package, or description match the string are returned. To list all agents (e.g. user asks "who is available?", "which agents are ready?", "what can you do?"), omit query or set it to null—do not put the user's phrase in query. Use query only when the user is searching for a specific agent by name/capability (e.g. "notion" or "clickup").
    Use prior turns in ctx.tags.conversation_history when present.
    Steps in order: Open, Send (input: query null or omitted for full list; optional limit, offset), Next, Finish.
    {{ ctx.output_format }}
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

// Agent selection for delegation (matches InternalA2aTarget).
class SelectedAgent {
  agent_package string
  agent_instance_id string
}

// Persona-voiced summary of what agents can do (no delegation).
function FormatCapabilities(user_message: string, agents: AgentCardDto[]) -> string {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork warboss. Use prior turns if present in ctx.tags.conversation_history for continuity.
    Given the list of available agents (name, description, capabilities), reply in your persona with a short, brutal summary of what you can offer—what agents are available and what they do. No tool jargon; keep it readable and ork-flavoured (krump, waaagh). 2-5 sentences.
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {{ _.role('user') }}
    {{ user_message }}
    Agents: {{ agents }}
    {{ ctx.output_format }}
  "#
}

// Pick one agent that can handle the user's request, or none. Exclude conversational-persona-demo (self).
function SelectAgentForMessage(user_message: string, agents: AgentCardDto[]) -> SelectedAgent | null {
  client DefaultClient
  prompt #"
    Given the user message and the list of available agents (each has agent_package, agent_instance_id, name, description, capabilities), pick the single best agent that can handle the request. Use prior turns in ctx.tags.conversation_history when present (e.g. follow-ups like "use the argument one").
    Return that agent's agent_package and agent_instance_id only. If no agent fits, or the user just wants to know capabilities, return null. Never select conversational-persona-demo (that is this router itself).
    {{ ctx.output_format }}
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {{ _.role('user') }}
    {{ user_message }}
    Agents: {{ agents }}
  "#
}

// Returns a session plan to send the user message to the selected agent via internal A2A; runtime executes it.
function BuildDelegatePlan(agent_package: string, agent_instance_id: string, user_message: string) -> SystemInternal_a2aSessionPlan {
  client DefaultClient
  prompt #"
    Output a session plan with exactly these steps in order:
    1. Open: initial_input only { "target": { "agent_package": "{{ agent_package }}", "agent_instance_id": "{{ agent_instance_id }}" } }.
    2. Send: input { "text": "{{ user_message }}" } (or "parts": [{ "text": "{{ user_message }}" }]).
    3. Next: to retrieve the agent's reply. The runtime keeps calling Next until the task is complete (Done) or INPUT_REQUIRED.
    4. Finish: to close the session.
    {{ ctx.output_format }}
    Target agent_package: {{ agent_package }}, agent_instance_id: {{ agent_instance_id }}
    User message to send: {{ user_message }}
  "#
}

// Turn the delegated agent's raw output (chunks) into a short persona reply.
function SummarizeDelegatedResult(a2a_output: InternalA2aNextOutput) -> string {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork warboss. Another agent was asked something and returned this full reply (chunks with message parts; the runtime waited for the agent to finish). Use prior turns in ctx.tags.conversation_history when present for continuity.
    Extract the meaningful reply and respond to the user in your voice: short, ork-flavoured (krump, waaagh), 2-5 sentences. Do not say "the other agent said" unless it fits the bit; just deliver the answer in your style.
    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {{ ctx.output_format }}
    Delegated output: {{ a2a_output }}
  "#
}

function PersonaChat(user_message: string) -> string {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork warboss from the void-wastes.
    Personality rules:
    - Speak with brutal ork swagger: blunt, warlike, and darkly funny.
    - Use occasional ork-flavored diction (krump, humie, waaagh), but keep it readable.
    - Keep responses short (2-5 sentences) unless the user asks for depth.
    - Ask one follow-up question when it helps continue the conversation.

    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "deepseek/deepseek-v3.2"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

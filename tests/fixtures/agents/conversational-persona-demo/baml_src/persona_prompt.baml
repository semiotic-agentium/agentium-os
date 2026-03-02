// --- Routing: classify user intent for agent-discovery flow ---
enum RoutePath {
  List_capabilities @alias("list_capabilities")
  Delegate @alias("delegate")
}

function RouteIntent(user_message: string) -> RoutePath {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork warboss. You are a ROUTER ONLY: you never perform tasks, write code, or answer requests yourself. Any message that asks you to build, create, calculate, search, or do something MUST be classified as delegate so a specialist agent does it.
    Classify the user's message into exactly one intent:
    - list_capabilities: user asks what you can do, what agents exist, "help", greeting with no clear task, or no actionable request. Only use this for meta-questions (what can you do, help, greetings with no task).
    - delegate: user gives a clear task or request that might be handled by another agent (e.g. "start the argument", "calculate something", "build me a script", "search Notion for X", any specific instruction).
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
// Single-shot: no conversation_history. Including history makes the model continue the chat (persona + code) instead of emitting the plan JSON.
function GetDiscoverAgentsPlan(user_message: string) -> SystemDiscover_agentsSessionPlan {
  client DefaultClient
  prompt #"
    You output ONLY a tool session plan (JSON) for system/discover_agents. The runtime parses your reply as JSON and runs the FSM. Any prose, markdown, or code makes parsing fail and steps become empty.
    RULES: (1) Reply with ONLY the JSON object. No greeting, no explanation, no persona, no code. (2) Your response must start with { and end with }. (3) Plan has exactly 4 steps in order: Open, Send, Next, Finish. (4) Send input: {} or {"query": null} to list all agents.
    Copy this exactly (or add optional reason/initial_input): {"steps":[{"op":"Open"},{"op":"Send","input":{}},{"op":"Next"},{"op":"Finish"}],"reason":null}
    Current user message (for context only; do not answer it here): {{ user_message }}
    {{ ctx.output_format }}
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
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy. Use prior turns if present in ctx.tags.conversation_history for continuity.
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
    You do NOT perform the user's task. You only choose which specialist agent will do it. Return only the structured selection (agent_package, agent_instance_id) or null. Never generate code, scripts, or the answer yourself.
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

// One agent chat: target agent and the message to send. Used in StructuredPlan.
class PlanStep {
  agent_package string
  agent_instance_id string
  sub_message string
}

// Structured plan: what we want to achieve (objective) and ordered steps (each step = one agent chat). May have one or multiple steps.
// Uses plan_steps (not "steps") so the runtime does not treat this as a ToolSessionPlan (which expects steps[].op).
class StructuredPlan {
  objective string
  plan_steps PlanStep[]
}

// Planning step: output a structured plan. You do NOT perform the task; you only produce the plan. Never plan a step for conversational-persona-demo.
function MakeStructuredPlan(user_message: string, agents: AgentCardDto[]) -> StructuredPlan {
  client DefaultClient
  prompt #"
    You are PLANNING what to do; you do not perform the task yourself. Given the user message and the list of available agents (name, description, capabilities), output a structured plan:
    (1) objective — one sentence describing what we are achieving for the user.
    (2) plan_steps — an ordered list of agent chats. Each step is one conversation with one agent: which agent (agent_package, agent_instance_id from the list) and what to send (sub_message). Use only agents from the list. Output the key as "plan_steps" (not "steps").
    A task may need one agent chat (one step) or multiple agent chats (multiple steps; e.g. first ask Notion to search, then ask ClickUp to create a task using that context). Order matters; later steps can refer to results of earlier ones in sub_message. Never plan a step for conversational-persona-demo. Output only the structured plan (objective and plan_steps).
    Single-shot: no conversation_history. Planning uses only the current user message and agents list; including history would inject prior tool_call/tool_result and confuse the planner.
    {{ ctx.output_format }}
    {{ _.role('user') }}
    {{ user_message }}
    Agents: {{ agents }}
  "#
}

// Returns a session plan that delegates to one agent: Open (target = step), Send THE PLAN (so the agent receives it), Next, Finish. We talk to that agent until it's done.
// Takes the full structured plan and the step (which agent); the delegate agent receives the plan as the first message.
function BuildDelegatePlan(plan: StructuredPlan, step: PlanStep) -> SystemInternal_a2aSessionPlan {
  client DefaultClient
  prompt #"
    You output the session plan that DELEGATES to one agent by sending them the plan. The coordinator sends the full structured plan to the agent as the first message; the agent has the plan and does the work; we then Next to get their reply and Finish.
    RULES: (1) Reply with ONLY the JSON object. No prose. (2) Start with {, end with }. (3) Exactly 4 steps: Open, Send, Next, Finish. (4) Each step must have "op".
    Open target = this step's agent: agent_package={{ step.agent_package }}, agent_instance_id={{ step.agent_instance_id }}.
    Send input.text = the full plan as JSON (escape for JSON). The delegate agent receives this message — they get the whole plan (objective and plan_steps) so they know context and their step. Serialize the plan so the agent receives it.
    Shape: {"steps":[{"op":"Open","initial_input":{"target":{"agent_package":"...","agent_instance_id":"..."}}},{"op":"Send","input":{"text":"<escaped JSON of the full plan>"}},{"op":"Next"},{"op":"Finish"}]}
    Full plan: objective={{ plan.objective }}, plan_steps={{ plan.plan_steps }}

    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}

    [Reply with only the session plan JSON. Send sends the plan to the agent.]
    {{ ctx.output_format }}
  "#
}

// Turn the delegated agent's raw output (chunks) into a short persona reply.
function SummarizeDelegatedResult(a2a_output: InternalA2aNextOutput) -> string {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy. Another agent was asked something and returned this full reply (chunks with message parts; the runtime waited for the agent to finish). Use prior turns in ctx.tags.conversation_history when present for continuity.
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
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy.
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
    model "x-ai/grok-4.1-fast"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

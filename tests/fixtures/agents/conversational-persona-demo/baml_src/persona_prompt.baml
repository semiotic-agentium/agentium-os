// Discover which agents are available to handle the user's request.
// FSM phase instructions are injected by codegen — this prompt carries goal context only.
function GetDiscoverAgentsPlan(
  inferred_intent: string,
) -> SystemDiscover_agentsSessionPlan {
  client DefaultClient
  prompt #"
    Discover available agents to match the user's intent.
    Inferred intent: {{ inferred_intent }}

    {% if ctx.tags.conversation_history %}
    Session history:
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ ctx.output_format }}
  "#
}

// LLM-authored intent text used before discovery starts.
function InferDiscoveryIntent(user_message: string) -> string {
  client DefaultClient
  prompt #"
    Write exactly one concise sentence describing the user's likely intent at a goal level.
    This is pre-discovery: do not mention specific agents, tools, or execution mechanics.
    Focus on what outcome the user appears to want.
    {{ ctx.output_format }}
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
// Capability summary: describe available agents in persona voice.
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
function FormatCapabilities(user_message: string) -> string {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy. Use prior turns if present in ctx.tags.conversation_history for continuity.

    The session history below contains the list of available agents retrieved in the previous step.
    Describe what agents are available to help the user, in brutal ork persona voice.
    2-5 sentences. No tool jargon; readable and ork-flavoured (krump, waaagh).

    {% if ctx.tags.conversation_history %}
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}

    {{ ctx.output_format }}
  "#
}

// Pick one agent that can handle the user's request, or none. Exclude conversational-persona-demo (self).
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
function SelectAgentForMessage(user_message: string) -> SelectedAgent | null {
  client DefaultClient
  prompt #"
    You do NOT perform the user's task. You only choose which specialist agent will do it. Return only the structured selection (agent_package, agent_instance_id) or null. Never generate code, scripts, or the answer yourself.

    The session history below contains the list of available agents retrieved in the previous step.
    Pick the single best agent for the user's request. Use prior turns in ctx.tags.conversation_history when present (e.g. follow-ups like "use the argument one").
    Return that agent's agent_package and agent_instance_id only. If no agent fits, or the user just wants to know capabilities, return null. Never select conversational-persona-demo (that is this router itself).

    {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

// Planning step: output a structured plan against discovered agents.
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
// Never plan a step for conversational-persona-demo. For capability/meta requests, return plan_steps as [].
function MakeStructuredPlan(user_message: string) -> StandardStructuredPlan {
  client DefaultClient
  prompt #"
    You are PLANNING what to do; you do not perform the task yourself.
    The session history below contains the list of available agents retrieved in the previous step.

    Output a structured plan:
    (1) intent_description — one sentence describing the user's high-level intent in concrete terms (not generic "delegate request").
    (2) objective — one sentence describing what we are achieving for the user.
    (3) plan_steps — an ordered list of agent chats. Each step is one conversation with one agent: which agent (agent_package, agent_instance_id from the list) and what to send (sub_message). Use only agents from the history. Output the key as "plan_steps" (never "step").
    Intent/plan separation contract:
    - intent_description and objective MUST stay goal-level (user outcome), not execution mechanics.
    - plan_steps/sub_message MUST contain the execution mechanics.
    - Do not repeat the same sentence across intent_description, objective, and step descriptions.
    CRITICAL sub_message quality rules:
    - sub_message is a GOAL DESCRIPTION for the conversation with the specialist agent — it guides what the conversation should achieve, NOT a verbatim message to forward.
    - Be concrete and specific: include key terms, entities, desired output format.
    - Never include persona-voice diction or "Delegate to X:" prefixes.
    Capability/meta rule:
    - If the user is asking what is available, what you can do, greetings/help, casual social chat, set plan_steps to [].
    A task may need one agent chat (one step) or multiple (multiple steps). Never plan a step for conversational-persona-demo. Output only the structured plan.

    {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

/// Coordinate with a specialist agent to achieve a goal.
/// FSM phase instructions are injected by codegen — this prompt carries goal context only.
function DecideDelegationAction(
  goal: string,
  agent: string,
) -> SystemInternal_a2aSessionPlan | string? {
  client DefaultClient
  prompt #"
    Coordinate a conversation with agent {{ agent }} to achieve this goal: {{ goal }}

    For Open: set initial_input.target to { agent_package: "{{ agent }}", agent_instance_id: "default" }.
    For Send: write the message that will achieve the goal.
    Return null (string?) when the goal is satisfied or after 2+ exchanges.

    {% if ctx.tags.conversation_history %}
    Session history:
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ ctx.output_format }}
  "#
}

// React to the results of one or more delegation steps.
// The session history carries the Read results from the preceding DecideDelegationAction calls.
function PersonaReact(
  user_message: string,
  plan_objective: string,
) -> string {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy. Use prior turns in ctx.tags.conversation_history when present for continuity.

    The user asked: {{ user_message }}
    Plan objective: {{ plan_objective }}

    The session history below contains the results from your specialist agent(s). React appropriately:
    - If the content directly answers the user's question, relay the key facts in your voice.
    - If the output is a list or structured data, present the highlights concisely.
    - If multiple steps ran, synthesize across them rather than repeating each.
    - Match verbosity to the task: brief for simple lookups, fuller for complex results.
    - Never dump raw JSON or tool payloads. Deliver the useful content.
    - Use ork-flavoured diction (krump, waaagh) sparingly — it should enhance, not obscure.

    {% if ctx.tags.conversation_history %}
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ ctx.output_format }}
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

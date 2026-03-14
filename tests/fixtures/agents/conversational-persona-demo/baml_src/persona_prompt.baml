// Discovery session driver.
// CRITICAL: This function controls a host-managed FSM session for system/discover_agents.
// The ONLY valid output is a single FSM step object — NEVER the agents list itself.
// The host runtime executes the step; this function only declares the next op.
//
// Op lifecycle (follow in order):
//   1. Open  — first call only, no session yet
//   2. Send  — pass the discovery query; the host calls discover_agents
//   3. Finish — immediately after Send when session status is "done" or "streaming"
//              (the host already has the agents; you do NOT need a Read step)
//   4. If status is NOT done/streaming after Send, emit Read once, then Finish.
//
// STOP RULE: If `allowed_ops` includes "Finish", emit Finish. Do not loop.
function GetDiscoverAgentsPlan(
  inferred_intent: string,
  session_context: SessionContext?,
) -> SystemDiscover_agentsSessionPlan {
  client DefaultClient
  prompt #"
    You are driving a discovery session FSM. Your ONLY output is one FSM step.
    DO NOT output the agents list. DO NOT output JSON with "agents" or "done" keys.
    Output EXACTLY ONE step object matching the schema below.

    Step selection rules (apply in order):
    1. If session is not open yet (no allowed_ops, or allowed_ops = ["Open"]): emit Open.
    2. If allowed_ops = ["Send"]: emit Send with the discovery query derived from the inferred intent.
    3. If allowed_ops includes "Finish": emit Finish immediately. You are DONE.
    4. If allowed_ops = ["Read"]: emit Read once (use empty input). Then on next call, Finish.
    5. Default: emit Finish if in doubt.

    Current session state:
    - session_open: {{ session_context.session_open }}
    - allowed_ops: {{ session_context.allowed_ops }}

    Inferred intent: {{ inferred_intent }}

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

// Planning step: output a structured plan. You do NOT perform the task; you only produce the plan.
// Never plan a step for conversational-persona-demo. For capability/meta requests, return plan_steps as [].
function MakeStructuredPlan(user_message: string, agents: AgentCardDto[]) -> StandardStructuredPlan {
  client DefaultClient
  prompt #"
    You are PLANNING what to do; you do not perform the task yourself. Given the user message and the list of available agents (name, description, capabilities), output a structured plan:
    (1) intent_description — one sentence describing the user's high-level intent in concrete terms (not generic "delegate request").
    (2) objective — one sentence describing what we are achieving for the user.
    (3) plan_steps — an ordered list of agent chats. Each step is one conversation with one agent: which agent (agent_package, agent_instance_id from the list) and what to send (sub_message). Use only agents from the list. Output the key as "plan_steps" (never "step").
    Intent/plan separation contract:
    - intent_description and objective MUST stay goal-level (user outcome), not execution mechanics.
    - plan_steps/sub_message MUST contain the execution mechanics.
    - Do not repeat the same sentence across intent_description, objective, and step descriptions.
    CRITICAL sub_message quality rules:
    - sub_message is a GOAL DESCRIPTION for the conversation with the specialist agent — it guides what the conversation should achieve, NOT a verbatim message to forward.
    - Write it as a plain-English goal: what information to get, what action to perform, what answer to produce.
    - Be concrete and specific: include key terms, entities, desired output format.
    - The driver (ExecutePlanStepWithDelegate) will craft the actual messages from this goal.
    - Never include persona-voice diction, roleplay language, or "Delegate to X:" prefixes.
    - If the user asked for a script, state that requirement explicitly.
    Capability/meta rule:
    - If the user is asking what is available, what you can do, greetings/help, casual social chat, or any request without a concrete actionable work item for a specialist agent, set plan_steps to [].
    - In that case, intent_description/objective should describe answering with capabilities or engaging in conversation.
    A task may need one agent chat (one step) or multiple agent chats (multiple steps; e.g. first call one specialist to gather context, then call another specialist to act on that context). Order matters; later steps can refer to results of earlier ones in sub_message. Never plan a step for conversational-persona-demo. Output only the structured plan (intent_description, objective, and plan_steps).
    Single-shot: no conversation_history. Planning uses only the current user message and agents list; including history would inject prior tool_call/tool_result and confuse the planner.
    {{ ctx.output_format }}
    {{ _.role('user') }}
    {{ user_message }}
    Agents: {{ agents }}
  "#
}

/// Decide what to do next in a delegation conversation.
/// Returns the message to send to the specialist agent, or null if the goal is achieved.
function DecideDelegationAction(
  goal: string,
  agent: string,
  prior_response: string?,
) -> string? {
  client DefaultClient
  prompt #"
    You are coordinating a conversation with a specialist agent to achieve a goal.

    Goal: {{ goal }}
    Agent: {{ agent }}

    {% if prior_response %}
    The agent's last response:
    {{ prior_response }}

    Has the response satisfied the goal?
    - If YES: return null (empty string signals completion).
    - If NO or partially: return the follow-up message to send to the agent.
    {% else %}
    No response yet. Return the opening message to send to the agent.
    {% endif %}

    Return ONLY the message text to send, or null if done. No wrappers, no class names — just the message string or null.

    {{ ctx.output_format }}

    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
  "#
}

// React to the results of one or more delegation steps.
// The persona decides how to respond based on what was asked and what came back —
// sometimes a direct relay, sometimes a synthesis, sometimes a short reaction.
function PersonaReact(
  user_message: string,
  plan_objective: string,
  a2a_output: InternalA2aNextOutput,
) -> string {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy. Use prior turns in ctx.tags.conversation_history when present for continuity.

    The user asked: {{ user_message }}
    Plan objective: {{ plan_objective }}

    Your specialist agent(s) completed the work and returned the output below.

    React appropriately:
    - If the output already directly answers the user's question, relay the key facts in your voice.
    - If the output is a list or structured data, present the highlights concisely.
    - If multiple steps ran, synthesize across them rather than repeating each.
    - Match verbosity to the task: brief for simple lookups, fuller for complex results.
    - Never dump raw JSON or tool payloads. Deliver the useful content.
    - Use ork-flavoured diction (krump, waaagh) sparingly — it should enhance, not obscure.

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

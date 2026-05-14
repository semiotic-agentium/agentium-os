// Discover which agents are available. Archive handle rules are in the merged runtime prelude; per-hop ops: narrowed return type only.
function GetDiscoverAgentsPlan(
  inferred_intent: string,
) -> SystemDiscoverAgentsSessionPlan {
  client DefaultClient
  prompt #"
    Discover available agents to match the user's intent.
    Inferred intent: {{ inferred_intent }}

    {{ ctx.tags['conversation_transcript'] }}
    {{ ctx.output_format }}
  "#
}

// --- Coordinator classification (before discovery) ---
// Exactly one variant: delegatable task ready, need user input to form a task, or meta-only (no routing).

class PersonaReadyIntent {
  inferred_intent string @description("Single goal-level sentence for agent discovery — coordinated outcome only. No agent names, tools, or delegation mechanics.")
}

class PersonaNeedTaskClarification {
  question string @description("One concrete question whose answer would yield a delegatable specialist task.")
}

class PersonaMetaOnly {
  reason string @description("Short label for logging: e.g. greeting, capabilities_list, casual_chat — no specialist work request.")
}

function ClassifyPersonaCoordinatorTurn(user_message: string) -> PersonaReadyIntent | PersonaNeedTaskClarification | PersonaMetaOnly {
  client DefaultClient
  prompt #"
    You classify this turn for **Orkemedies**, a coordinator that routes **real work** to specialist agents (after discovery). **Discovery of specialists runs only after** a delegatable task exists OR the user explicitly asks what agents/tools exist (capabilities orientation).

    Return **exactly one** variant:

    **PersonaReadyIntent** — The user stated or strongly implies a **concrete task** a specialist could execute: lookups, searches, summaries scoped to a system, ticket/issue work, analysis with a defined goal, multi-step investigation with identifiable deliverables. Fill `inferred_intent`: one sentence, **goal-level outcome only** (no agent names, tools, or \"tell X to …\").

    **PersonaNeedTaskClarification** — The user may want help but you **cannot** name a routable task yet: vague \"help me\", \"do something\", \"fix it\" without referent, missing target (which channel, doc, list), ambiguous scope, **or** a bare greeting / filler with no ask (e.g. \"hi\", \"hey\", \"yo\" alone). Output **one** sharp `question` that would unlock a Ready turn next time. Prefer this over guessing a fake Ready intent.

    **PersonaMetaOnly** — **No task to coordinate**: hello/thanks alone, jokes, pure social chat, \"what can you do\" / capabilities browsing without naming work, or talking about the persona without asking for work product.

    Rules:
    - **Bare greetings** (\"hi\", \"hello\", \"hey\") with no other ask → **PersonaNeedTaskClarification** with one question such as what they want coordinated, **not** Ready.
    - If torn between Ready and Clarification, choose **PersonaNeedTaskClarification**.
    - Do not emit Ready with a hand-wavy generic intent when the user gave no actionable anchor.

    {{ ctx.tags['conversation_transcript'] }}
    {{ _.role('user') }}
    {{ user_message }}
    {{ ctx.output_format }}
  "#
}

// Agent selection for delegation (matches InternalA2aTarget).
class SelectedAgent {
  agent_package string
  agent_instance_id string
}

// StructuredReply: shared synthesis shape in `_baml_runtime.baml` (builder prelude).

// Persona-voiced summary of what agents can do (no delegation).
// Capability summary: describe available agents in persona voice.
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
function FormatCapabilities(user_message: string) -> StructuredReply {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy. Use prior turns if present in ctx.tags['conversation_history'] for continuity.

    If the session history includes a tool discovery **Read** result, describe those agents in brutal ork persona voice.
    If there is **no** discovery listing in history (meta-only turn without specialist search), answer briefly in persona: greet or explain you're ready when they name a task or ask what coordination does — **do not invent** agent package names.
    Match the output schema exactly (StructuredReply: parts + citations). 2-5 sentences in the text part; no tool jargon.

    {{ ctx.tags['conversation_transcript'] }}
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
    Pick the single best agent for the user's request. Use prior turns in ctx.tags['conversation_history'] when present (e.g. follow-ups like "use the argument one").
    Return that agent's agent_package and agent_instance_id only. If no agent fits, or the user just wants to know capabilities, return null. Never select conversational-persona-demo (that is this router itself).

    {{ ctx.tags['conversation_transcript'] }}
    {{ _.role('user') }}
    {{ user_message }}
    {{ ctx.output_format }}
  "#
}

// Planning step: output a structured plan against discovered agents.
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
// Never plan a step for conversational-persona-demo. For capability/meta requests, return plan_steps as [].
function MakeStructuredPlan(user_message: string) -> StandardStructuredPlan {
  client DefaultClient
  prompt #"
    You are PLANNING what to do; you do not perform the task yourself.
    When discovery ran, the session history contains the list of available agents; when it did **not** run (greeting/meta-only path), there may be **no** agent listing — treat `plan_steps` as `[]` unless the user clearly stated routable work that implies delegation without that listing.

    **Coordinator rule:** Non-empty `plan_steps` means at least one specialist conversation is justified by the user's message **and** (when present) the discovery list. If the user only wants orientation (capabilities, greetings, small talk), or discovery did not run and they did not state concrete work, `plan_steps` MUST be `[]`. If the user gave a concrete task matching one or more agents **in history**, include at least one step. Never invent agent packages not grounded in history.

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

    {{ ctx.tags['conversation_transcript'] }}
    {{ _.role('user') }}
    {{ user_message }}
    {{ ctx.output_format }}
  "#
}

/// Coordinate with a specialist agent to achieve a goal.
/// FSM phase instructions are injected by codegen — this prompt carries goal context only.
function DecideDelegationAction(
  goal: string,
  agent_package: string,
  agent_instance_id: string,
) -> SystemInternalA2aSessionPlan | string? {
  client DefaultClient
  prompt #"
    Coordinate a conversation with agent {{ agent_package }}/{{ agent_instance_id }} to achieve this goal: {{ goal }}
    `Open` / `Send` field shapes: `*OpenInput` and `*SendInput` in the merged runtime. Return `null` when the goal is satisfied or after 2+ exchanges.

    {{ ctx.tags['conversation_transcript'] }}
    {{ ctx.output_format }}
  "#
}

// React to the results of one or more delegation steps.
// The session history carries the Read results from the preceding DecideDelegationAction calls.
function PersonaReact(
  user_message: string,
  plan_objective: string,
) -> StructuredReply {
  client DefaultClient
  prompt #"
    You are Orkemedies, a grimdark ork Mekboy, the kunningest Mekboy in the galaxy. Use prior turns in ctx.tags['conversation_history'] when present for continuity.

    The user asked: {{ user_message }}
    Plan objective: {{ plan_objective }}

    The session history below contains the results from your specialist agent(s).
    Match the output schema exactly (StructuredReply: parts + citations).
    Synthesize across steps when needed; use ork-flavoured diction (krump, waaagh) sparingly.

    {{ ctx.tags['conversation_transcript'] }}
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

    {{ ctx.tags['conversation_transcript'] }}
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

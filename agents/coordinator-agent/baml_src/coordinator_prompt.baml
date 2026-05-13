// Discover which agents are available. Archive handle rules are in the merged runtime prelude; per-hop ops: narrowed return type only.
function GetDiscoverAgentsPlan(
  inferred_intent: string,
) -> SystemDiscoverAgentsSessionPlan {
  client DefaultClient
  prompt #"
    Discover available agents to match the user's intent.
    Inferred intent: {{ inferred_intent }}
  "#
}

// --- Coordinator classification (before discovery) ---
// Exactly one variant: delegatable task ready, need user input to form a task, or meta-only (no routing).

class CoordinatorReadyIntent {
  inferred_intent string @description("Single goal-level sentence for agent discovery — coordinated outcome only. No agent names, tools, or delegation mechanics.")
}

class CoordinatorNeedTaskClarification {
  question string @description("One concrete question whose answer would yield a delegatable specialist task.")
}

class CoordinatorMetaOnly {
  reason string @description("Short label for logging: e.g. greeting, capabilities_list, casual_chat — no specialist work request.")
}

function ClassifyCoordinatorTurn(user_message: string) -> CoordinatorReadyIntent | CoordinatorNeedTaskClarification | CoordinatorMetaOnly {
  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}You classify this turn for a **coordinator agent** that routes **real work** to specialist agents (after discovery). **Discovery of specialists runs only after** a delegatable task exists OR the user explicitly asks what agents or tools exist (capabilities orientation).

    Return **exactly one** variant:

    **CoordinatorReadyIntent** — The user stated or strongly implies a **concrete task** a specialist could execute: lookups, searches, summaries scoped to a system, ticket or issue work, analysis with a defined goal, multi-step investigation with identifiable deliverables. Fill `inferred_intent`: one sentence, **goal-level outcome only** (no agent names, tools, or \"tell X to …\").

    **CoordinatorNeedTaskClarification** — The user may want help but you **cannot** name a routable task yet: vague \"help me\", \"do something\", \"fix it\" without referent, missing target (which channel, document, list), ambiguous scope, **or** a bare greeting / filler with no ask (e.g. \"hi\", \"hey\", \"yo\" alone). Output **one** sharp `question` that would unlock a Ready turn next time. Prefer this over guessing a fake Ready intent.

    **CoordinatorMetaOnly** — **No task to coordinate**: hello or thanks alone, jokes, pure social chat, \"what can you do\" / capabilities browsing without naming work, or meta discussion without asking for work product.

    Rules:
    - **Bare greetings** (\"hi\", \"hello\", \"hey\") with no other ask → **CoordinatorNeedTaskClarification** with one question such as what they want coordinated, **not** Ready.
    - If torn between Ready and Clarification, choose **CoordinatorNeedTaskClarification**.
    - Do not emit Ready with a hand-wavy generic intent when the user gave no actionable anchor.

    {{ _.role('user') }}
    {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}
"#
}

// StructuredReply: shared synthesis shape in `_baml_runtime.baml` (builder prelude).

// Professional summary of what agents can do (no delegation).
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
function FormatCapabilities(user_message: string) -> StructuredReply {
  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}You are the coordinator agent: calm, precise, and brief. Use prior turns from the transcript below when present for continuity.

    If the session history includes a tool discovery **Read** result, summarize those agents in clear professional language (what each is for, major capabilities).
    If there is **no** discovery listing in history (meta-only turn without specialist search), answer briefly: greet if appropriate and explain you are ready when they name a task or ask what coordination does — **do not invent** agent package names.
    Match the output schema exactly (StructuredReply: parts + citations). Two to five sentences in the text part; no internal tool jargon.

    {{ _.role('user') }}
    {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}
"#
}

// Planning step: output a structured plan against discovered agents.
// The session history carries the Read result from the preceding GetDiscoverAgentsPlan call.
// Never plan a step for coordinator-agent. For capability/meta requests, return plan_steps as [].
function MakeStructuredPlan(user_message: string) -> StandardStructuredPlan {
  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}You are PLANNING what to do; you do not perform the task yourself.
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
    - Never include playful persona voice or "Delegate to X:" prefixes.
    Capability/meta rule:
    - If the user is asking what is available, what you can do, greetings/help, casual social chat, set plan_steps to [].
    A task may need one agent chat (one step) or multiple (multiple steps). Never plan a step for coordinator-agent. Output only the structured plan.

    {{ _.role('user') }}
    {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

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
    Goal: Coordinate with agent {{ agent_package }}/{{ agent_instance_id }} to achieve: {{ goal }}
    Return `null` when the goal is satisfied or after 2+ exchanges.
  "#
}

// Synthesize the user-visible reply after delegation step(s).
// The session history carries the Read results from the preceding DecideDelegationAction calls.
function CoordinatorSynthesizeReply(
  user_message: string,
  plan_objective: string,
) -> StructuredReply {
  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}You are the coordinator agent: professional, concise, and accurate. Use prior turns from the transcript below when present for continuity.

    The user asked: {{ user_message }}
    Plan objective: {{ plan_objective }}

    The transcript below contains the results from your specialist agent(s).
    Match the output schema exactly (StructuredReply: parts + citations).
    Synthesize across steps when needed; prefer bullet structure in markdown only when it improves clarity.

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}
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

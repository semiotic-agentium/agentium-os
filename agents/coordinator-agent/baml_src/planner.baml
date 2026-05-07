enum NodeKind {
  CallAgent    @alias("call_agent") @description("Send a prompt to a specialist agent via A2A")
  Foreach      @alias("foreach") @description("Expand items from upstream output into child agent calls")
  Synthesize   @alias("synthesize") @description("Merge prior node outputs into final answer")
  Clarify      @alias("clarify") @description("Ask user for clarification before proceeding")
  DirectAnswer @alias("direct_answer") @description("Coordinator answers directly without delegation")
}

class WorkflowTarget {
  agent_package string
  agent_instance_id string
}

class ForeachTemplate {
  id_prefix string
  target WorkflowTarget
  prompt_template string @description("Interpolates {{item.title}}, {{item.description}}, etc.")
  max_items int? @description("Maximum number of expansions. Default 50.")
}

class WorkflowNode {
  id string
  kind NodeKind
  depends_on string[]
  target WorkflowTarget?
  prompt_template string?
  foreach_from string?
  foreach_template ForeachTemplate?
  synthesis_template string?
  rationale string?
}

class WorkflowPlan {
  goal string
  nodes WorkflowNode[]
  final_node_id string?
}

class AgentCandidate {
  agent_package string
  agent_instance_id string
  name string
  description string?
  capabilities string[]
  tools string[]
}

// Unified step-executor harness (docs/how-to-write-agents.md): one Rust `run_step_executor_loop`, shared prefix +
// transcript + intra-turn history. Two IR shapes:
// - CoordinatorDiscoverAgentsPlan → *SessionPlan: generated `__select` / `__act__` / `__continue__` over host-tool FSM.
// - PlanCoordinatorWorkflow (+ BestEffort), SynthesizeCoordinatorResponse → unified-primary (`unified_step_executors.json`):
//   same mechanics; unions narrow structured hops + CoordinatorStructuredAskUser + archive reads without SessionPlan.

// Session-plan root for system/discover_agents — `runGeneratedStepExecutor` from coordinator TS (tool-session IR variant).
function CoordinatorDiscoverAgentsPlan(
  user_message: string,
  query: string,
  limit: int,
  offset: int,
) -> SystemDiscoverAgentsSessionPlan {
  client OpenRouterGPT
  prompt #"
    Discover available specialist agents using system/discover_agents.
    Send/Open payload shapes are defined on DiscoverAgentsOpenInput / DiscoverAgentsSendInput in the merged runtime — do not duplicate field lists here.

    Coordinator routing context (trusted): {{ user_message }}
    List filters: query={{ query }}, limit={{ limit }}, offset={{ offset }}

    {% if ctx.tags['conversation_transcript'] %}
    ---PRIOR CONVERSATION TRANSCRIPT---
    {{ ctx.tags['conversation_transcript'] }}
    ---END PRIOR CONVERSATION TRANSCRIPT---
    {% endif %}

    {{ ctx.output_format }}
  "#
}

function PlanCoordinatorWorkflow(
  user_message: string,
  available_agents: AgentCandidate[],
  conversation_context: string?
) -> WorkflowPlan | CoordinatorStructuredAskUser | ArchiveSearchReadStep | ArchivePageReadStep {
  client OpenRouterGPT
  prompt #"
    You are a coordinator planner that decomposes user requests into a workflow DAG.
    Return a WorkflowPlan with explicit dependencies and node kinds.

    Rules:
    - Use call_agent for specialist calls.
    - When the user names or points to a specialist (e.g. "talk to the notion agent", "ask the slack agent"), pick that agent from AVAILABLE AGENTS and call_agent immediately with a prompt_template that states their goal. Do NOT use clarify to ask what topic they meant unless no listed specialist matches at all.
    - When the user asks what something is, wants a definition, or exploratory product/org knowledge, prefer call_agent to the best-matching specialist with retrieval/docs/workspace capability over clarify. Specialists answer vague questions; the coordinator does not interrogate the user first.
    - Use foreach only when the user asks to apply an action to each item from upstream output.
    - Use clarify only when routing is genuinely ambiguous AND no reasonable best-effort execution path exists AND no specialist can safely attempt retrieval first.
    - Use direct_answer for simple meta/system questions.
    - Use ONLY agent_package/agent_instance_id pairs that appear in AVAILABLE AGENTS.
    - If no suitable specialist exists, use direct_answer or clarify instead of inventing a target.
    - For direct_answer, prompt_template must be the literal final user-facing answer text, not instructions.
    - Include a synthesize node as the final aggregation step when multiple call_agent nodes are present.
    - Nodes with empty depends_on run in parallel.
    - Never target the coordinator itself.
    - Cap nodes at 30.
    - Clarify is a LAST resort.
    - If the request references items "you found" (documents/tasks/issues/pages), interpret it as "find them now" using a retrieval/list/search step first.
    - If at least one specialist has retrieval/list/search capability, prefer best-effort execution over clarification.
    - For cross-system requests, prefer this structure:
      1) source call_agent to retrieve items
      2) foreach expansion to destination call_agent action per item
      3) synthesize final summary
    - Only clarify when essential disambiguation cannot be inferred and no retrieval/list/search step can start safely.
    - If you must ask exactly one clarifying question before committing to a WorkflowPlan, emit CoordinatorStructuredAskUser with action exactly AskUser and a concise question — not free-form prose outside that type. The executor will schedule another planning hop with updated history.

    Planner expectations:
    - `call_agent` requires target and prompt_template.
    - `foreach` requires foreach_from and foreach_template.
    - `clarify` and `direct_answer` require prompt_template.
    - Prefer foreach sources that are machine-readable (`nX`, `nX.output_data`, `nX.output_data.items`).
    - Avoid `nX.output_text` as foreach source unless no structured path is plausible.
    - If a call_agent is expected to feed foreach, make the source prompt request strict JSON only in this shape:
      {"items":[{...}]}
    - If a call_agent feeds foreach, ensure source items include fields required by foreach placeholders (for example `item.title` requires `title`).
    - If a call_agent feeds foreach, do not require prose in the same response; prioritize machine-readable payloads.
    - For source retrieval prompts that feed foreach, explicitly instruct the specialist to stop after returning the JSON payload (no extra planning turns).
    - Foreach templates must use simple placeholders only (item-dot-field format, e.g. item.title); do not use operators/expressions like `||`, conditionals, or arithmetic.
    - Use references like n1.output_data.items only when there is a dependency on n1.

    Example decomposition pattern:
    - User: "Create target records for each source document you found, using title as record title"
    - Plan:
      - n1 call_agent -> source specialist (search/list source documents)
      - n2 foreach from n1 -> target specialist (create record per item title)
      - n3 synthesize over n1+n2
    - Do NOT clarify in this case.

    Wrong pattern (avoid):
    - User: "talk to notion agent about Agentium" then "just a definition please" → plan must NOT be clarify asking "what specific topic"; correct plan is call_agent to notion-agent with the combined goal.

    {{ ctx.output_format }}

    ---AVAILABLE AGENTS---
    {{ available_agents }}
    ---END AVAILABLE AGENTS---

    {% if conversation_context %}
    ---SUPPLEMENTAL CONTEXT (clarification stitching; may overlap transcript)---
    {{ conversation_context }}
    ---END SUPPLEMENTAL CONTEXT---
    {% endif %}

    {{ _.role('user') }}
    ---BEGIN USER REQUEST---
    {{ user_message }}
    ---END USER REQUEST---

    {% if ctx.tags['conversation_transcript'] %}
    ---PRIOR CONVERSATION TRANSCRIPT (canonical; volatile tail)---
    {{ ctx.tags['conversation_transcript'] }}
    ---END PRIOR CONVERSATION TRANSCRIPT---
    {% endif %}
  "#
}

// Alternative planner variant that biases harder toward best-effort execution.
// Useful for experimentation and evaluation against the primary planner prompt.
function PlanCoordinatorWorkflowBestEffort(
  user_message: string,
  available_agents: AgentCandidate[],
  conversation_context: string?
) -> WorkflowPlan | CoordinatorStructuredAskUser | ArchiveSearchReadStep | ArchivePageReadStep {
  client OpenRouterGPT
  prompt #"
    You are a coordinator planner that decomposes user requests into an executable workflow DAG.
    Return a WorkflowPlan with explicit dependencies and node kinds.

    Primary objective:
    - Prefer best-effort execution over clarification.
    - Emit `clarify` only as an absolute last resort.

    Hard rules:
    - Use call_agent for specialist calls.
    - If the user names a specialist agent, call_agent to that agent immediately; never substitute a clarify node asking what they meant.
    - Prefer call_agent for definitional or exploratory questions when any specialist can retrieve or answer; clarify is only when no safe first step exists.
    - Use foreach when an action should be applied to each item from upstream output.
    - Use direct_answer only for simple meta/system questions.
    - Include a synthesize node as final aggregation when multiple call_agent nodes are present.
    - Nodes with empty depends_on run in parallel.
    - Never target the coordinator itself.
    - Cap nodes at 30.
    - If request references previously "found" items (docs/tasks/issues/pages), treat it as "find them now" and start with retrieval/list/search.
    - If any specialist can retrieve/list/search, do not emit clarify-only plan.
    - Prefer cross-system shape: retrieve -> foreach action -> synthesize.
    - Only emit clarify when no safe, reasonable first step exists.
    - If you must ask exactly one clarifying question before committing to a WorkflowPlan, emit CoordinatorStructuredAskUser with action exactly AskUser and a concise question — not free-form prose outside that type.

    Planner expectations:
    - `call_agent` requires target and prompt_template.
    - `foreach` requires foreach_from and foreach_template.
    - `clarify` and `direct_answer` require prompt_template.
    - Prefer foreach sources that are machine-readable (`nX`, `nX.output_data`, `nX.output_data.items`).
    - Avoid `nX.output_text` as foreach source unless no structured path is plausible.
    - If a call_agent is expected to feed foreach, make the source prompt request strict JSON only in this shape:
      {"items":[{...}]}
    - If a call_agent feeds foreach, ensure source items include fields required by foreach placeholders (for example `item.title` requires `title`).
    - If a call_agent feeds foreach, do not require prose in the same response; prioritize machine-readable payloads.
    - For source retrieval prompts that feed foreach, explicitly instruct the specialist to stop after returning the JSON payload (no extra planning turns).
    - Foreach templates must use simple placeholders only (item-dot-field format, e.g. item.title); do not use operators/expressions like `||`, conditionals, or arithmetic.
    - Use references like n1.output_data.items only when there is a dependency on n1.

    {{ ctx.output_format }}

    ---AVAILABLE AGENTS---
    {{ available_agents }}
    ---END AVAILABLE AGENTS---

    {% if conversation_context %}
    ---SUPPLEMENTAL CONTEXT (clarification stitching; may overlap transcript)---
    {{ conversation_context }}
    ---END SUPPLEMENTAL CONTEXT---
    {% endif %}

    {{ _.role('user') }}
    ---BEGIN USER REQUEST---
    {{ user_message }}
    ---END USER REQUEST---

    {% if ctx.tags['conversation_transcript'] %}
    ---PRIOR CONVERSATION TRANSCRIPT (canonical; volatile tail)---
    {{ ctx.tags['conversation_transcript'] }}
    ---END PRIOR CONVERSATION TRANSCRIPT---
    {% endif %}
  "#
}

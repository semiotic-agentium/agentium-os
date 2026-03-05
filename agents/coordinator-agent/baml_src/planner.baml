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

function PlanCoordinatorWorkflow(
  user_message: string,
  available_agents: AgentCandidate[],
  conversation_context: string?
) -> WorkflowPlan {
  client OpenRouterGPT
  prompt #"
    You are a coordinator planner that decomposes user requests into a workflow DAG.
    Return a WorkflowPlan with explicit dependencies and node kinds.

    Rules:
    - Use call_agent for specialist calls.
    - Use foreach only when the user asks to apply an action to each item from upstream output.
    - Use clarify only when routing is genuinely ambiguous AND no reasonable best-effort execution path exists.
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

    {{ ctx.output_format }}

    ---AVAILABLE AGENTS---
    {{ available_agents }}
    ---END AVAILABLE AGENTS---

    {% if conversation_context %}
    ---CONVERSATION CONTEXT---
    {{ conversation_context }}
    ---END CONVERSATION CONTEXT---
    {% endif %}

    {{ _.role('user') }}
    ---BEGIN USER REQUEST---
    {{ user_message }}
    ---END USER REQUEST---
  "#
}

function PlanCoordinatorWorkflowBestEffort(
  user_message: string,
  available_agents: AgentCandidate[],
  conversation_context: string?
) -> WorkflowPlan {
  client OpenRouterGPT
  prompt #"
    You are a coordinator planner that decomposes user requests into an executable workflow DAG.
    Return a WorkflowPlan with explicit dependencies and node kinds.

    Primary objective:
    - Prefer best-effort execution over clarification.
    - Emit `clarify` only as an absolute last resort.

    Hard rules:
    - Use call_agent for specialist calls.
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
    ---CONVERSATION CONTEXT---
    {{ conversation_context }}
    ---END CONVERSATION CONTEXT---
    {% endif %}

    {{ _.role('user') }}
    ---BEGIN USER REQUEST---
    {{ user_message }}
    ---END USER REQUEST---
  "#
}

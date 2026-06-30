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
  client DefaultClient
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
    - For direct_answer, prompt_template must be the literal final user-facing answer text.
    - Include a synthesize node as the final aggregation step when multiple call_agent nodes are present.
    - Nodes with empty depends_on run in parallel.
    - Never target the coordinator itself.
    - Cap nodes at 30.

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

client DefaultClient {
  provider openai-generic
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

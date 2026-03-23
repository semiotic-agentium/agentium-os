function PlanSyntheticSessionStep(
  objective: string,
) -> SyntheticPlannerPlan {
  client DefaultClient
  prompt #"
    Output ONLY one JSON object with exactly one session `decision`. No prose. No markdown.

    Objective: {{ objective }}

    Strategy:
    - Reach goal_id = non_trivial_scope_cache_goal via explicit session reads.
    - Use projection progression: summary → identity → detail before Finish.
    - Minimise wasted reads: once you have data at one projection level, advance to the next.

    The allowed ops and current session state are in the session history below.
    Read the history to determine what has already been done, then emit the single next step.

    {% if ctx.tags.conversation_history %}
    Session history:
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ ctx.output_format }}
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

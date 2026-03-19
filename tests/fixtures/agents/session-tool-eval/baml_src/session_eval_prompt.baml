function PlanSyntheticSessionStep(
  objective: string,
) -> SyntheticPlannerPlan {
  client DefaultClient
  prompt #"
    Output ONLY one JSON object with exactly one session `decision`. No prose. No markdown.

    Objective:
    1) reach goal_id = non_trivial_scope_cache_goal
    2) minimize wasted reads and payload
    3) use projection progression: summary -> identity -> detail before Finish

    Look at the session history to determine the current state and what to do next:
    - No prior session step in history → emit Open (optional reason only)
    - Session opened, no Send → emit Send to set/refresh scope defaults
    - Send completed, no Read yet → emit Read with input.read.refId = "root" and projection "summary"
    - Prior Read results available → inspect them to decide:
      - If candidates present: Read with max severity ref_id
      - If refs present but no candidates: Read with refs[0]
      - If goal_id = "non_trivial_scope_cache_goal" and summary+identity+detail all used → emit Finish
      - Otherwise continue reading with next projection in the sequence

    Current objective: {{ objective }}

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

function PlanSyntheticSessionStep(
  objective: string,
  session_context: SessionContext?,
  session_state: SyntheticPlannerSendInput?,
) -> SyntheticPlannerPlan {
  client DefaultClient
  prompt #"
    Output ONLY one JSON object with exactly one session `decision`. No prose. No markdown.

    Objective:
    1) reach goal_id = non_trivial_scope_cache_goal
    2) minimize wasted reads and payload
    3) use projection progression: summary -> identity -> detail before Finish

    Current session state:
    - session_open: {{ session_context.session_open }}
    - allowed_ops: {{ session_context.allowed_ops }}
    - session_state: {{ session_state }}

    Decision rules based on current state:
    - session_open = false → emit Open
    - session_open = true, session_state = null → emit Send to set scope defaults
    - session_state present, no Read results yet → emit Read with refId="root" and projection="summary"
    - Read results present → inspect to decide next Read or Finish:
      - If goal_id = "non_trivial_scope_cache_goal" seen and summary+identity+detail all used → emit Finish
      - Otherwise read next projection in the sequence (summary → identity → detail)

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

// Fixture: Tool discovery demo. Use system/discover_tools to answer the user; tool metadata defines how to call it.
function ChooseDiscoverToolsQuery(user_message: string, session_context: SessionContext?) -> SystemDiscover_toolsSessionPlan {
  client DefaultClient
  prompt #"
    Return ONLY a valid JSON object for one system/discover_tools FSM step.
    Emit exactly one `step` object per reply. No prose. No markdown.

    DECISION LADDER — choose the FIRST matching rule:
    1. session_context is null or allowed_ops contains only "Open" → emit Open (empty initial_input).
    2. allowed_ops contains "Send"   → emit Send with input.query derived from user_message.
    3. allowed_ops contains "Read"   → emit Read with empty input {}.
    4. allowed_ops contains "Finish" → emit Finish to close the session.

    {{ ctx.output_format }}
    {{ _.role('user') }}
    User message: {{ user_message }}
    {% if session_context %}Allowed ops: {{ session_context.allowed_ops }}{% endif %}
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

// Fixture: BAML tool streaming. FSM phase instructions injected by codegen.
function ChooseCalcTool(user_message: string, session_context: SessionContext?) -> SupportCalculateSessionPlan {
  client DefaultClient
  prompt #"
    Evaluate this calculation: {{ user_message }}
    For Send: set input.expression to the mathematical expression extracted from the message. If unclear, use 2 + 3.

    {% if ctx.tags['conversation_history'] %}
    Session history:
    {% for message in ctx.tags['conversation_history'] %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {% endif %}

    {{ ctx.output_format }}
  "#
}

// Streaming variant for JS invoke/stream tests.
function ChooseCalcToolStream(user_message: string) -> string {
  client DefaultClient
  prompt #"
    The user says: {{ user_message }}
    Reply briefly with one word: OK
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

retry_policy ParseRetry {
  max_retries 2
  strategy {
    type constant_delay
    delay_ms 200
  }
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

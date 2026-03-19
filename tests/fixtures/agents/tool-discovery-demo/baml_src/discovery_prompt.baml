// Fixture: Tool discovery demo. FSM phase instructions injected by codegen.
function ChooseDiscoverToolsQuery(user_message: string) -> SystemDiscover_toolsSessionPlan {
  client DefaultClient
  prompt #"
    Find tools that can help with: {{ user_message }}
    For Send: set input.query to a concise search term derived from the user message.

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

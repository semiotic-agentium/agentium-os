// Fixture: Tool discovery demo. Generated __select / __act__* / __continue__* functions reuse this prompt verbatim; per-hop variance is only the narrowed return type (hence {{ ctx.output_format }}).
function ChooseDiscoverToolsQuery(user_message: string) -> SystemDiscoverToolsSessionPlan {
  client DefaultClient
  prompt #"
    Find tools that can help with: {{ user_message }}
    For Send: set input.query to a concise search term derived from the user message.

    Session discipline: discover_tools returns matching tools in the Send (and optional Read) archive.
    Once the tool list for the user's query is present in history, use the Continue hop to emit Finish and close the session.
    Do not issue extra Reads or Sends unless the archive explicitly indicates more pages or incomplete results.

    {% for message in ctx.tags['conversation_history'] %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}

    {{ ctx.output_format }}
  "#
}

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model env.BAML_TEST_MODEL
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

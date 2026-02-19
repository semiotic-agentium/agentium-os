// Fixture: Tool discovery demo. Use system/discover_tools to answer the user; tool metadata defines how to call it.
function ChooseDiscoverToolsQuery(user_message: string) -> SystemDiscover_toolsSessionPlan {
  client DefaultClient
  prompt #"
    Use system/discover_tools stepwise to find out what tools match what the user is asking about: open the session, send a search (query from the user's question), get the results, then finish.
    {{ ctx.output_format }}
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model "deepseek/deepseek-v3.2"
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

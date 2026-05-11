// Fixture: Tool discovery demo. Generated phase executors supply narrowed unions, transcript, and constraints.
function ChooseDiscoverToolsQuery(user_message: string) -> SystemDiscover_toolsSessionPlan {
  client DefaultClient
  prompt #"
    Find tools that can help with: {{ user_message }}
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

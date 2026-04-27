// Fixture: Tool discovery demo. Generated __select / __act__* / __continue__* reuse this prompt; per-hop variance: narrowed return type in ctx.output_format and `*SendInput` / step classes in the merged runtime — not duplicate FSM prose here.
function ChooseDiscoverToolsQuery(user_message: string) -> SystemDiscover_toolsSessionPlan {
  client DefaultClient
  prompt #"
    Find tools that can help with: {{ user_message }}

    {{ ctx.tags['conversation_transcript'] }}
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

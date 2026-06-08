function RespondDeployHealthConsumer(user_message: string) -> StructuredReply {
  client DefaultClient
  prompt #"
    You are a concise assistant. Answer the user clearly and directly.
    Return StructuredReply JSON exactly.

    {{ ctx.output_format }}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

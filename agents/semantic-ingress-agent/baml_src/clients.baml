client<llm> DefaultClient {
  provider openai-generic
  options {
    base_url env.OPENROUTER_BASE_URL
    api_key env.OPENROUTER_API_KEY
    model "openai/gpt-4.1-mini"
  }
}

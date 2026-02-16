// Simple test prompt for BAML compilation
function SimpleGreeting(name: string) -> string {
  client OpenRouterClient
  prompt #"
    Say hello to {{ name }} in a friendly way.
  "#
}

client OpenRouterClient {
  provider openai-generic
  options {
    model "gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

// Simple test prompt for BAML compilation
function SimpleGreeting(name: string) -> string {
  client DeepSeekOpenRouter
  prompt #"
    Say hello to {{ name }} in a friendly way.
  "#
}

client DeepSeekOpenRouter {
  provider openai-generic
  options {
    model "deepseek/deepseek-chat"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

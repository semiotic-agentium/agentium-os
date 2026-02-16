// Simple test prompt for BAML compilation
function SimpleGreeting(name: string) -> string {
  client DeepSeekOpenRouter
  prompt #"
    Say hello to {{ name }} in a friendly way.
    Reply with exactly one short greeting sentence. Do not leave the response empty or blank.
  "#
}

client DeepSeekOpenRouter {
  provider openai-generic
  options {
    model "deepseek/deepseek-v3.2"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

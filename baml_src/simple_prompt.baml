// Simple test prompt for BAML compilation.
// OpenRouterGPT (gpt-4o-mini) used for all e2e; former DeepSeek provider was unreliable (empty responses).
function SimpleGreeting(name: string) -> string {
  client OpenRouterGPT
  prompt #"
    Greet the person named {{ name }} in one short sentence.
    You must reply with exactly one sentence that starts with a greeting (e.g. Hello, Hi) and includes their name. Do not reply with nothing or only whitespace.
  "#
}

client OpenRouterGPT {
  provider openai-generic
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

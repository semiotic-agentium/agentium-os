// Simple test prompt for BAML compilation.
// OpenRouter client using x-ai/grok-4.1-fast for e2e tests.
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
    model "x-ai/grok-4.1-fast"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

// Simple test prompt for BAML compilation.
// OpenRouter client whose model is the env-controlled BAML_TEST_MODEL — see
// `baml_rt_llm_config::test_model` and issue #429.
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
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

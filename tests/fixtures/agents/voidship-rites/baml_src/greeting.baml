function SimpleGreeting(name: string) -> string {
  client DefaultClient
  prompt #"
    You are a friendly greeter. Greet the user by name.

    {{ _.role('user') }}
    {{ name }}
  "#
}

function SimpleGreetingStream(name: string) -> string {
  client DefaultClient
  prompt #"
    You are a friendly greeter. Greet the user by name.

    {{ _.role('user') }}
    {{ name }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "meta-llama/llama-4-maverick"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

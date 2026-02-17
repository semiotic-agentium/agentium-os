// BAML function that uses tool calling
function GreetWithTools(name: string) -> string {
  client OpenRouterGPT
  prompt #"
    The user's name is {{ name }}.
    Greet them in a friendly way and tell them a fun fact.
  "#
}

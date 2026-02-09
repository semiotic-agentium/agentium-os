// Fixture: BAML tool streaming. Prompt returns a calculation session plan (2+3 default).
function ChooseCalcTool(user_message: string) -> SupportCalculateSessionPlan {
  client DefaultClient
  prompt #"
    The user says: {{ user_message }}
    Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
    {{ ctx.output_format }}
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "deepseek/deepseek-chat"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

class ChatMessage {
  role string
  content string
}

function ChooseCalcTool(user_message: string) -> SupportCalculateSessionPlan {
  client DefaultClient
  prompt #"
    The user says: {{ user_message }}
    Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
    Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
    {{ ctx.output_format }}
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

function ChatWithContext(user_message: string) -> string {
  client DefaultClient
  prompt #"
    You are in a multi-turn conversation.
    Use prior turns if present in `ctx.tags.conversation_history`.
    If the user asks what to remember or asks for the codeword:
    - Locate the most recent USER message in conversation history that contains the word "codeword".
    - Extract the token immediately following "codeword".
    - Reply with ONLY that token, uppercase, no punctuation or extra words.
    - If no such token exists, reply exactly: UNKNOWN

    {% for message in ctx.tags.conversation_history %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

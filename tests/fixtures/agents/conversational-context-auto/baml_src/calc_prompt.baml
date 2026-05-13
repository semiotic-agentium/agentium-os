class ChatMessage {
  role string
  content string
}

function ChooseCalcTool(user_message: string) -> SupportCalculateSessionPlan {
  client DefaultClient
  prompt #"
    The user says: {{ user_message }}
    Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

function ChatWithContext(user_message: string) -> string {
  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}You are in a multi-turn conversation.
    Use prior turns if present in the structured conversation history block below.
    If the user asks what to remember or asks for the codeword:
    - Locate the most recent `role: user` entry in the structured history that contains the word "codeword".
    - Extract the token immediately following "codeword".
    - Reply with ONLY that token, uppercase, no punctuation or extra words.
    - If no such token exists, reply exactly: UNKNOWN

    Structured conversation history:

    {{ _.role('user') }}
    {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

{{ ctx.output_format }}
"#
}

client DefaultClient {
  provider openai-generic
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

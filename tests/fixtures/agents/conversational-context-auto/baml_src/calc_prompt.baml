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
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and operation types:
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You are in a multi-turn conversation.
    Use prior turns if present in the structured conversation history block below.
    If the user asks what to remember or asks for the codeword:
    - Locate the most recent `role: user` entry in the structured history that contains the word "codeword".
    - Extract the token immediately following "codeword".
    - Reply with ONLY that token, uppercase, no punctuation or extra words.
    - If no such token exists, reply exactly: UNKNOWN

    Structured conversation history:

    {{ _.role('user') }}
    {{ user_message }}

{{ ctx.output_format }}

Return exactly one output matching the schema below. Do not add extra text before or after it.
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

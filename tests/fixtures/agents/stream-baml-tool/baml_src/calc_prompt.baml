// Fixture: BAML tool streaming. FSM phase instructions injected by codegen.
function ChooseCalcTool(user_message: string, session_context: SessionContext?) -> SupportCalculateSessionPlan {
  client DefaultClient
  prompt #"
    Evaluate this calculation: {{ user_message }}
  "#
}

// Streaming variant for JS invoke/stream tests.
function ChooseCalcToolStream(user_message: string) -> string {
  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}The user says: {{ user_message }}
    Reply briefly with one word: OK
    {{ _.role('user') }}
    {{ user_message }}

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

Return exactly one output matching the schema below. Do not add extra text before or after it.

{{ ctx.output_format }}
"#
}

retry_policy ParseRetry {
  max_retries 2
  strategy {
    type constant_delay
    delay_ms 200
  }
}

client DefaultClient {
  provider openai-generic
  retry_policy ParseRetry
  options {
    model env.BAML_TEST_MODEL
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

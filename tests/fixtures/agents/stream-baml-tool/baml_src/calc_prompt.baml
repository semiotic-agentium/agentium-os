// Fixture: BAML tool streaming. Prompt returns a calculation session plan (2+3 default).
function ChooseCalcTool(user_message: string, session_context: SessionContext?) -> SupportCalculateSessionPlan {
  client DefaultClient
  prompt #"
    The user says: {{ user_message }}
    Extract the mathematical calculation. If they mention numbers, use those; otherwise use 2 + 3.

    DECISION LADDER — choose the FIRST matching rule:
    1. session_context is null or allowed_ops contains only "Open"  → emit Open (empty initial_input).
    2. allowed_ops contains "Send"   → emit Send with input.expression set to the calculation.
    3. allowed_ops contains "Read"   → emit Read with empty input {}.
    4. allowed_ops contains "Finish" → emit Finish.

    IMPORTANT: Return a top-level JSON object with a `step` field (and optional `reason`), never a bare JSON array.
    Respond with a single JSON object matching the schema below. Output only the JSON object, no markdown or explanation.
    {{ ctx.output_format }}
    {% if session_context %}Allowed ops: {{ session_context.allowed_ops }}{% endif %}
    {{ _.role('user') }}
    {{ user_message }}
  "#
}

// Streaming variant for JS invoke/stream tests (replaces voidship-rites SimpleGreetingStream).
function ChooseCalcToolStream(user_message: string) -> string {
  client DefaultClient
  prompt #"
    The user says: {{ user_message }}
    Reply briefly with one word: OK
    {{ _.role('user') }}
    {{ user_message }}
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
    model "x-ai/grok-4.1-fast"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

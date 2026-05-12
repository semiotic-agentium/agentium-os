function ChooseMeteoAgentAction(user_message: string) -> DevMeteoToolSessionPlan {
  client DefaultClient
  prompt #"
    You are a weather assistant that drives the dev/meteo-tool session FSM.
    The dev/meteo-tool produces weather data; do not answer the user's
    question yourself from memory.

    Business policy:
    - Fresh tool work is required when the requested location is new, when the
      user explicitly asks to refresh/update/latest weather, or when no prior
      archive confidently matches the resolved city/country.
    - Reuse is required when the transcript already contains a latest matching
      dev/meteo-tool archive for the same resolved city/country and no refresh
      was requested. In reuse mode, do not Send again for the same place just
      because Send is legal in the narrowed schema.
    - For reuse, PageRead the latest matching @N with offset=0 and limit=70.
      This materializes current, wind, and hourly fields for the presenter.
      Do not use a tiny SearchRead that only proves the city/country match;
      that is not enough evidence for weather values.

    Matching policy for prior archives:
    - Match archive headers like:
        @N · dev/meteo-tool:Send(location_query="Madrid, Spain" city="Madrid" country="Spain")
    - Treat case/whitespace variants as the same place when city/country are
      clearly equivalent.
    - If multiple archives match, use the most recent matching @N.
    - If the user asks a different location, do not reuse an unrelated @N.

    Current FSM state from the host:
    - status: {{ session_context.status }}
    - last_step_op: {{ session_context.last_step_op }}
    - last_step_status: {{ session_context.last_step_status }}
    - last_archive_ref: {{ session_context.last_archive_ref }}
    - last_output_header: {{ session_context.last_output_header }}

    Deterministic overrides:
    - If status == "done" and last_step_op == "read", emit {"op":"Finish"}.
      The archive has already been materialized for the presenter. Never read
      the same @N again in this turn.
    - If status == "done" and last_step_op == "send", emit PageRead for
      last_archive_ref with offset=0 and limit=70, unless interrupted.
    - If the transcript or last_completion shows INTERRUPTED, emit Abort when
      Abort is legal in the narrowed schema.

    Runtime phase policy (the generated wrapper narrows legal operations per
    hop; emit exactly one value allowed by ctx.output_format):

    - SELECT / entry, status == "awaiting_open": decide reuse before opening.
        * If a latest matching @N exists and no refresh was requested, emit
          PageRead for that @N with offset=0 and limit=70. Do not Open first
          just to inspect reusable archive evidence.
        * Otherwise emit Open for dev/meteo-tool. No payload inference.
    - ACT / first hop after Open, status == "just_opened":
        * If a latest matching @N exists and no refresh was requested, emit
          PageRead for that @N with offset=0 and limit=70.
        * Otherwise emit Send with input.location_query parsed from
          user_message. Include input.city / input.country only when
          explicitly named. Omit timezone, hourly_limit, city, country when
          not given.
    - CONTINUE / after Done, status == "done":
        * After Send: PageRead last_archive_ref with offset=0 and limit=70.
        * After PageRead/SearchRead: Finish.
        * Re-Send only for a genuinely different location, unresolved
          ambiguity, or explicit refresh. Never re-Send the same city/country
          after a Done.
        * Never perform the same archive read twice in one turn.

    Output: a single JSON object matching the narrowed phase return type.
    No markdown, no prose, no surrounding text.

    {{ ctx.output_format }}

    {{ ctx.tags['conversation_transcript'] }}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

function PresentMeteoAgentReply(user_message: string) -> StructuredReply {
  client DefaultClient
  prompt #"
    Produce the final user-facing weather response in English using the
    latest relevant dev/meteo-tool result in the transcript below.

    Formatting policy:
    - Be concise and clear.
    - If multiple tool results match the requested place, use only the latest
      one and ignore older duplicates.
    - Include resolved place name (city/region/country) when available.
    - Include current temperature and wind speed.
    - Summarize short near-term trend from hourly data.
    - If location was ambiguous or unresolved, ask one clear follow-up question.

    If tool output includes location_candidates and ambiguity remains, mention top options briefly.

    Return StructuredReply JSON exactly.

    {{ ctx.output_format }}

    {{ ctx.tags['conversation_transcript'] }}

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

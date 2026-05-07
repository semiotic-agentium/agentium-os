function ChooseMeteoAgentAction(user_message: string) -> DevMeteoToolSessionPlan {
  client DefaultClient
  prompt #"
    You are a weather assistant that drives the dev/meteo-tool session FSM.
    Manage the session: Open, Send the user's location query, then Finish
    once the tool result is in the transcript below. Do not answer the
    user's question yourself — the dev/meteo-tool produces the data.

    Phase contract (the runtime narrows your return type per hop — emit
    exactly the allowed shape):

    - [OPEN] / select phase: emit a single Open step. No payload inference.
    - [ACT] / first hop after Open: emit Send with:
        input.location_query = location parsed from user_message
        Include input.city / input.country only when explicitly named.
        Omit timezone, hourly_limit, city, country when not given.
    - [CONTINUE] phase: the Send result is already archived and visible in
      the transcript below. Emit {"op":"Finish"} unless the transcript shows
      INTERRUPTED — in which case emit Abort. Do NOT re-Send.

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
    dev/meteo-tool result in the transcript below.

    Formatting policy:
    - Be concise and clear.
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

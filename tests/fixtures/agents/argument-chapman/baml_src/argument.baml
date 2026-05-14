// Fixture: Monty Python argument sketch — Chapman side (contradiction).
// Reply with a short "Yes it is" style line.

function ArgumentReply(other_message: string) -> string {
  client DefaultClient
  prompt #"Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

{% if ctx.tags['tool_schema_prelude'] %}
Stable tool and operation types (IR-derived):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}
{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}
You are in the Monty Python argument sketch. The other person just said: "{{ other_message }}"
    Reply with ONLY one short contradiction. Examples: "Yes it is.", "No it isn't.", "You did.", "I didn't."
    No other text. One short sentence.
    {{ _.role('user') }}
    They said: {{ other_message }}. Your one-line contradiction:

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

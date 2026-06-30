// Bootstrap template prompt (LLM path). The committed eval uses a deterministic TS echo
// in src/index.ts so CI does not require API keys. Customize index.ts after bootstrap
// or wire Bootstrapechoeval when running with LLM config.
function Bootstrapechoeval(input: string) -> string {
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
Current step instructions:

You are a helpful agent. The user said: {{ input }}
    Reply briefly.

{{ ctx.output_format }}

Return exactly one output matching the schema below. Do not add extra text before or after it.
"#
}

client DefaultClient {
  provider openai
  options {
    model "gpt-4o-mini"
    api_key env.OPENAI_API_KEY
  }
}

class IngressUnitSeen {
  kind "seen"
  unit_label string @description("Short label for the lifecycle unit inferred from Session history.")
}

class IngressNotRelevant {
  kind "not_relevant"
  reason string
}

function ClassifyIngressUnit() -> IngressUnitSeen | IngressNotRelevant {
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

You classify a single host ingress work unit (one lifecycle record slice already in Session history).
Return seen with a short unit_label when the history describes a ClickUp lifecycle task row.
Return not_relevant only when the history has no ClickUp task content.

{{ ctx.output_format }}

Return exactly one JSON object.
Select the object shape with discriminator `kind`:
- `kind: "seen"` -> IngressUnitSeen
- `kind: "not_relevant"` -> IngressNotRelevant
Set `kind` exactly. Do not mix fields from different object shapes.
Do not add text before or after the JSON object.
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

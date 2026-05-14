// Fixture: Monty Python argument sketch — Cleese side (contradiction).
// Reply with a short "No it isn't" style line. Chat-style prompts; session plan for Chapman is prompt-driven.

function ArgumentReply(other_message: string) -> string {
  client DefaultClient
  prompt #"Archive: a `tool: @N` line is a handle, not the body. Read with SearchRead or PageRead before citing line content. Prefer reading an existing @N that could answer the task over another Send to repeat the same ask.

For `op: "Open"`, emit `tool_name` as a sibling of `op` in the same JSON object. Do not nest `tool_name` under `input` — `input` is only for Send, SearchRead, and PageRead.

{% if ctx.tags['tool_schema_prelude'] %}
Tool and session-step types (authoritative field shapes):
{{ ctx.tags['tool_schema_prelude'] }}

{% endif %}You are in the Monty Python argument sketch. The other person just said: "{{ other_message }}"
    Reply with ONLY one short contradiction. Examples: "No it isn't.", "Yes it is.", "I didn't.", "You did."
    No other text. One short sentence.
    {{ _.role('user') }}
    They said: {{ other_message }}. Your one-line contradiction:

{% if ctx.tags['conversation_transcript'] %}
Session history:
{{ ctx.tags['conversation_transcript'] }}
{% endif %}

Return exactly one output matching the schema below. Do not add extra text before or after it.

{{ ctx.output_format }}
"#
}

// Send Cleese's line to Chapman via internal_a2a. Generated phase executors supply FSM hardness and session history; op shape comes from {{ ctx.output_format }} per hop.
function CleeseSendToChapman(first_line: string) -> SystemInternalA2aSessionPlan {
  client DefaultClient
  prompt #"
    Argue in the style of a Monty Python sketch.

    For Open: set initial_input.target to { agent_package: "argument-chapman", agent_instance_id: "default" }.

    Cleese line — this is the only text you may send to Chapman on Send (copy it exactly; do not invent substitute sketch lines or alternate wording):
    {{ first_line }}

    On Send, the message body must be exactly the Cleese line above, character for character. Do not improvise dialogue.

    One round trip only: send Cleese’s line once; when Chapman’s reply appears in session history (or after paging the delegated archive if needed), Finish — do not Send again to Chapman in the same planner turn.
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

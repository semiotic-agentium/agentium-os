// Fixture: Monty Python argument sketch — Cleese side (contradiction).
// Reply with a short "No it isn't" style line. Chat-style prompts; session plan for Chapman is prompt-driven.

function ArgumentReply(other_message: string) -> string {
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

Archive refs: `@N` names a visible tool-result handle, not evidence by itself. Read archive content with SearchRead or PageRead before citing `@N:L` or `@N:L1-L2`.
Evidence refs: use `#N` for transcript evidence already visible in Session history, `@N:L` / `@N:L1-L2` for materialized archive lines, and prefix `!` for counter-evidence.

You are in the Monty Python argument sketch. The other person just said: "{{ other_message }}"
    Reply with ONLY one short contradiction. Examples: "No it isn't.", "Yes it is.", "I didn't.", "You did."
    No other text. One short sentence.
    {{ _.role('user') }}
    They said: {{ other_message }}. Your one-line contradiction:

{{ ctx.output_format }}

Return exactly one output matching the schema below. Do not add extra text before or after it.
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

    Current FSM state from the host:
    - status: {{ session_context.status }}
    - session_open: {{ session_context.session_open }}
    - last_step_op: {{ session_context.last_step_op }}
    - last_step_status: {{ session_context.last_step_status }}
    - last_archive_ref: {{ session_context.last_archive_ref }}
    - last_output_header: {{ session_context.last_output_header }}
    - last_completion: {{ session_context.last_completion }}

    Deterministic overrides:
    - If status == "just_opened" and last_step_op == "open":
      emit Send. input/message body must be exactly "{{ first_line }}".
      Never emit Finish in this state.
    - If status == "done" and last_step_op == "send":
      emit Finish. The delegated reply has already been returned by Send chunks.
      Do not Send again.
    - If status == "done" and last_step_op == "read":
      emit Finish.
    - If last_completion == "INTERRUPTED":
      emit Abort if Abort is legal.
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

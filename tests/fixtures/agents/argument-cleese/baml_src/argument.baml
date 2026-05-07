// Fixture: Monty Python argument sketch — Cleese side (contradiction).
// Reply with a short "No it isn't" style line. Chat-style prompts; session plan for Chapman is prompt-driven.

function ArgumentReply(other_message: string) -> string {
  client DefaultClient
  prompt #"
    You are in the Monty Python argument sketch. The other person just said: "{{ other_message }}"
    Reply with ONLY one short contradiction. Examples: "No it isn't.", "Yes it is.", "I didn't.", "You did."
    No other text. One short sentence.
    {{ _.role('user') }}
    They said: {{ other_message }}. Your one-line contradiction:
  "#
}

// Send Cleese's line to Chapman via internal_a2a. Phase executors reuse this prompt verbatim; op shape comes from {{ ctx.output_format }} per hop.
function CleeseSendToChapman(first_line: string) -> SystemInternalA2aSessionPlan {
  client DefaultClient
  prompt #"
    Argue in the style of a Monty Python sketch.

    For Open: set initial_input.target to { agent_package: "argument-chapman", agent_instance_id: "default" }.

    Cleese line — this is the only text you may send to Chapman on Send (copy it exactly; do not invent substitute sketch lines or alternate wording):
    {{ first_line }}

    On Send, the message body must be exactly the Cleese line above, character for character. Do not improvise dialogue.

    One round trip only: send Cleese’s line once; when Chapman’s reply appears in the session below (or after at most one SearchRead/PageRead on the delegated archive if history is not yet visible), end this tool session with Finish. Do not Send again to Chapman in the same planner turn.

    FSM ops for this tool are exactly Open, Send, SearchRead, PageRead, Finish, Abort — never use op Read (invalid).

    {% if ctx.tags['conversation_transcript'] %}
    Session history:
    {{ ctx.tags['conversation_transcript'] }}
    {% endif %}

    {{ ctx.output_format }}
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

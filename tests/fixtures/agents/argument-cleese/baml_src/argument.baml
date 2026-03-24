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

// Send Cleese's line to Chapman via an A2A session. FSM phase instructions injected by codegen.
function CleeseSendToChapman(first_line: string) -> SystemInternal_a2aSessionPlan {
  client DefaultClient
  prompt #"
    Coordinate the argument sketch with agent argument-chapman.
    For Open: set initial_input.target to { agent_package: "argument-chapman", agent_instance_id: "default" }.
    For Send: set input.parts to [{ "text": "{{ first_line }}" }].

    {% if ctx.tags['conversation_history'] %}
    Session history:
    {% for msg in ctx.tags['conversation_history'] %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ ctx.output_format }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "x-ai/grok-4.1-fast"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

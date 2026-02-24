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

// Returns a session plan to send Cleese's line to Chapman; runtime executes the plan (chat in the prompt, not in agent logic).
function CleeseSendToChapman(first_line: string) -> SystemInternal_a2aSessionPlan {
  client DefaultClient
  prompt #"
    You are coordinating the argument sketch. Cleese said: "{{ first_line }}"
    Output a session plan with exactly these steps in order:
    1. Open: initial_input only { "target": { "agent_package": "argument-chapman", "agent_instance_id": "default" } }. No other fields in initial_input.
    2. Send: input must be { "parts": [{ "text": "{{ first_line }}" }] } (structured message parts).
    3. Next: to retrieve Chapman's reply.
    4. Finish: to close the session.
    {{ ctx.output_format }}
    {{ _.role('user') }}
    Cleese's line to send to Chapman: {{ first_line }}
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

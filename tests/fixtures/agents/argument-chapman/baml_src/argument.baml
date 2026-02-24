// Fixture: Monty Python argument sketch — Chapman side (contradiction).
// Reply with a short "Yes it is" style line.

function ArgumentReply(other_message: string) -> string {
  client DefaultClient
  prompt #"
    You are in the Monty Python argument sketch. The other person just said: "{{ other_message }}"
    Reply with ONLY one short contradiction. Examples: "Yes it is.", "No it isn't.", "You did.", "I didn't."
    No other text. One short sentence.
    {{ _.role('user') }}
    They said: {{ other_message }}. Your one-line contradiction:
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

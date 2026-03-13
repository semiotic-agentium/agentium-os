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

// Returns one session step at a time to send Cleese's line to Chapman; keep chat policy in prompt, not agent logic.
function CleeseSendToChapman(first_line: string) -> SystemInternal_a2aSessionPlan {
  client DefaultClient
  prompt #"
    You are coordinating the argument sketch.
    Return ONLY a valid JSON object for a strict incremental system/internal_a2a session fragment.
    Emit exactly one `step` object per reply.

    If no session is open, return Open with:
    { "initial_input": { "target": { "agent_package": "argument-chapman", "agent_instance_id": "default" } } }

    If session is open and message not sent yet, return Send with:
    { "input": { "parts": [{ "text": "{{ first_line }}" }] } }

    Then use Next, and Finish when done.
    No prose. No markdown.
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

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

// Returns one session step at a time to send Cleese's line to Chapman via internal_a2a.
// Uses session_context to determine which FSM step to emit (Open/Send/Read/Finish).
function CleeseSendToChapman(first_line: string, session_context: SessionContext?) -> SystemInternal_a2aSessionPlan {
  client DefaultClient
  prompt #"
    You are coordinating the argument sketch. Send Cleese's line to Chapman via internal_a2a.
    Return ONLY a valid JSON object for a strict incremental session fragment.
    Emit exactly one `step` object per reply.

    DECISION LADDER — choose the FIRST matching rule:
    1. session_context is null or allowed_ops contains only "Open" → emit Open:
       {"step":{"op":"Open","initial_input":{"target":{"agent_package":"argument-chapman","agent_instance_id":"default"}}}}
    2. allowed_ops contains "Send" → emit Send with the message:
       {"step":{"op":"Send","input":{"parts":[{"text":"{{ first_line }}"}]}}}
    3. allowed_ops contains "Read" → emit Read:
       {"step":{"op":"Read","input":{"parts":[]}}}
    4. allowed_ops contains "Finish" → emit Finish:
       {"step":{"op":"Finish"}}

    No prose. No markdown. Return only the JSON object.
    {{ ctx.output_format }}
    {% if session_context %}Allowed ops: {{ session_context.allowed_ops }}{% endif %}
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

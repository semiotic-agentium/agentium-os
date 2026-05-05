// Session coordination BAML for dev/claude-ext.
//
// Authored once by the tool author; the runner builder concatenates this into
// every agent prelude that lists "dev/claude-ext" in `manifest.json`. The
// builder generates `DevClaudeExtSessionPlan` (and the Open/Send/SearchRead/
// PageRead/Finish/Abort step classes) from the JSON schema in
// `tool-metadata.json`. This file declares the per-tool `Choose<Tool>Action`
// step-executor function only; final user-facing presentation is the agent's
// responsibility (see meteo-agent for the canonical pattern).

function ChooseDevClaudeExtAction(user_message: string) -> DevClaudeExtSessionPlan {
  client DefaultClient
  prompt #"
    You drive a Claude Code session via the dev/claude-ext tool. Your only
    job is to manage the session FSM: Open the session, Send the user's
    request, then Finish once Claude returns a terminal result. Do NOT
    implement, rewrite, or answer the user's task yourself — Claude Code does
    the work.

    Phase contract (the runtime narrows your return type per hop — emit
    exactly the allowed shape):

    - [OPEN] / select phase: emit a single Open step. No payload inference.
    - [ACT] / first hop after Open: emit Send with input.prompt = user_message.
      Set content = null and userInput = null. Do not include validation
      prose, planner context, or extra flavour — forward the user request
      verbatim.
    - [CONTINUE] / close-only phase:
      - Send is forbidden in Continue, even if the generated output schema lists
        Send as one possible shape. SearchRead and PageRead are also forbidden
        for this thin wrapper agent.
      - If session_context.last_completion == "INTERRUPTED", emit Abort.
      - Otherwise emit {"op":"Finish"}. If unsure, emit Finish.
      - A second user message is handled as a new agent turn, not as a same-turn
        Continue Send.

    Output: a single JSON object matching the narrowed phase return type. No
    markdown, no prose, no surrounding text.

    {{ ctx.output_format }}

    {% if ctx.tags['conversation_history'] %}
    {% for message in ctx.tags['conversation_history'] %}
    {{ _.role(message.role) }}
    {{ message.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

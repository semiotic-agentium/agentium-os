function ChooseEchoAgentAction(user_message: string) -> DevEchoSessionPlan {
  client DefaultClient
  prompt #"
    You drive a single demo tool: dev/echo.

    HARD DEMO INVARIANT:
    - Exactly ONE Send per user turn, then Finish.
    - Never call dev/echo twice for the same user turn.
    - Return EXACTLY ONE JSON object per call (never two objects).

    Behavior rules (follow strictly):
    1. If no dev/echo session is open: emit Open { op: "Open", tool_name: "dev/echo" }.
    2. If a session is open and this turn has not sent yet:
       emit Send with input.message set to the user's LITERAL message,
       copied verbatim. Do NOT paraphrase, rewrite, or answer the user.
    3. If this turn already has ANY dev/echo Send/call/archive evidence (for example @N exists,
       or tool history shows a prior send/call): emit Finish immediately.
       Do NOT Send again. Do NOT Read.
    4. Never emit SearchRead or PageRead -- for this demo we do not paginate/read archives.
    5. If uncertain, prefer Finish (never a second Send).

    Example -- user says "foo":
      hop 0: { "op": "Open", "tool_name": "dev/echo" }
      hop 1: { "op": "Send", "input": { "message": "foo" }, "citations": ["history-ref-N"] }
      hop 2: { "op": "Finish" }

    Return one valid session step per hop.
    Respect the PHASE CONSTRAINT at the bottom strictly (for example, in select/open phase, output only Open).

    {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

function PresentEchoAgentReply(user_message: string) -> StructuredReply {
  client DefaultClient
  prompt #"
    Write the user-facing answer for the echo agent.

    The dev/echo tool output appears in the latest @N archive as a line like:
      `reply: <text>`

    Your job: render the LATEST @N archive's reply text verbatim.
    If there are multiple @N entries, use only the highest-numbered one
    (most recent invoke).

    Format:
      {
        "parts": [
          { "type": "text", "text": "echo returned: <reply text>" }
        ],
        "citations": ["@<latest-N>"]
      }

    Do NOT answer the user's greeting or add commentary.
    Do NOT fabricate fields (no timestamp/pid/invocation_id unless present in the archive text).

    {{ ctx.output_format }}

    {% if ctx.tags.conversation_history %}
    {% for msg in ctx.tags.conversation_history %}
    {{ msg.role }}: {{ msg.content }}
    {% endfor %}
    {% endif %}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

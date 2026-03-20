// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — open a tool session.
function ChooseNotionAction__select(
  goal: string,
  step_description: string
) -> ReadOnlyResponse | SupportNotionOpenStep {

  client OpenRouterGPT
  prompt #"You are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

Check the session history to determine your next action:
- No session open yet → emit Open (empty initial_input)
- Session open, no Send yet → emit Send with the appropriate Notion input:
    For a "discover" step: NotionSearchPagesInput { query: "<search term>", page_size: 10 }
    For a "read" step: NotionGetPageBlocksInput { block_id: "<page_id from history>", page_size: 50, max_depth: 2 }
- Send done (history shows "@N support/notion") → emit Read with:
    input.archive_ref = "@N"
    input.grep = "<key term from goal or step>"
    input.limit = 40
- Read result in history:
    If content is sufficient for this step → emit Finish, then return ReadOnlyResponse
      { "message": "<what was found>", "next_step": null }
    If more content needed → emit Read again with a different grep term or offset

Rules:
- Always grep when reading. Never Read without input.grep set.
- For "read" steps: use page_id values found in prior Read results in history.
- Never guess IDs — only use identifiers visible in session history.
- Do not attempt writes or edits.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: act — send query to support/notion.
function ChooseNotionAction__act__support_notion(
  goal: string,
  step_description: string
) -> SupportNotionSendStep {

  client OpenRouterGPT
  prompt #"You are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

Check the session history to determine your next action:
- No session open yet → emit Open (empty initial_input)
- Session open, no Send yet → emit Send with the appropriate Notion input:
    For a "discover" step: NotionSearchPagesInput { query: "<search term>", page_size: 10 }
    For a "read" step: NotionGetPageBlocksInput { block_id: "<page_id from history>", page_size: 50, max_depth: 2 }
- Send done (history shows "@N support/notion") → emit Read with:
    input.archive_ref = "@N"
    input.grep = "<key term from goal or step>"
    input.limit = 40
- Read result in history:
    If content is sufficient for this step → emit Finish, then return ReadOnlyResponse
      { "message": "<what was found>", "next_step": null }
    If more content needed → emit Read again with a different grep term or offset

Rules:
- Always grep when reading. Never Read without input.grep set.
- For "read" steps: use page_id values found in prior Read results in history.
- Never guess IDs — only use identifiers visible in session history.
- Do not attempt writes or edits.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — read, send again, or finish support/notion.
function ChooseNotionAction__continue__support_notion(
  goal: string,
  step_description: string
) -> SupportNotionSendStep | SupportNotionReadStep | SupportNotionFinishStep {

  client OpenRouterGPT
  prompt #"You are executing one step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

Check the session history to determine your next action:
- No session open yet → emit Open (empty initial_input)
- Session open, no Send yet → emit Send with the appropriate Notion input:
    For a "discover" step: NotionSearchPagesInput { query: "<search term>", page_size: 10 }
    For a "read" step: NotionGetPageBlocksInput { block_id: "<page_id from history>", page_size: 50, max_depth: 2 }
- Send done (history shows "@N support/notion") → emit Read with:
    input.archive_ref = "@N"
    input.grep = "<key term from goal or step>"
    input.limit = 40
- Read result in history:
    If content is sufficient for this step → emit Finish, then return ReadOnlyResponse
      { "message": "<what was found>", "next_step": null }
    If more content needed → emit Read again with a different grep term or offset

Rules:
- Always grep when reading. Never Read without input.grep set.
- For "read" steps: use page_id values found in prior Read results in history.
- Never guess IDs — only use identifiers visible in session history.
- Do not attempt writes or edits.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}


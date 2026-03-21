// Auto-generated per-phase step executor functions.
// Each phase narrows the return type to only the legal FSM ops.

/// Phase: select — always Open; no history needed.
function ChooseNotionAction__select(
  goal: string,
  step_description: string
) -> ReadOnlyResponse | SupportNotionOpenStep {

  client OpenRouterGPT
  prompt #"You are starting a step of a read-only Notion retrieval plan.
Overall goal: {{ goal }}
This step: {{ step_description }}

Emit Open to begin a new Notion session for this step.

{{ ctx.output_format }}"#

}

/// Phase: act — session is just opened; emit Send with the right query.
function ChooseNotionAction__act__support_notion(
  goal: string,
  step_description: string
) -> SupportNotionSendStep {

  client OpenRouterGPT
  prompt #"A Notion session is open. Emit Send with the appropriate query.

Overall goal: {{ goal }}
This step: {{ step_description }}

Choose the right input:
- For a discover/search step: NotionSearchPagesInput { query: "<search term>", page_size: 10 }
- For a read/blocks step: NotionGetPageBlocksInput { block_id: "<page_id>", page_size: 50, max_depth: 2 }

Rules:
- For read steps: the page_id must come from the session history below — do not guess.
- Do not write or edit anything.

{% if ctx.tags.conversation_history %}
Prior conversation context (for page IDs and user intent only — @N archive refs below are from previous steps, not the current session):
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

/// Phase: continue — result archived at @N; Read it or Finish.
function ChooseNotionAction__continue__support_notion(
  goal: string,
  step_description: string
) -> SupportNotionSendStep | SupportNotionReadStep | SupportNotionFinishStep {

  client OpenRouterGPT
  prompt #"A support/notion Send just completed. The result is archived.

Overall goal: {{ goal }}
This step: {{ step_description }}

Check the session history to decide:
- See "@N support/notion" header followed by numbered lines → content is inline; emit Finish
- See "@N support/notion" with "more lines" indicator → emit Read to paginate
- See "@N support/notion" with no lines yet → emit Read archive_ref="@N" with a grep term

Read rules:
- Always set input.grep to a relevant keyword. Never Read without grep.
- Use the highest @N (most recent result for this session).
- Only use page_id values visible in prior Read output — never guess IDs.

{% if ctx.tags.conversation_history %}
Session history:
{% for msg in ctx.tags.conversation_history %}
{{ msg.role }}: {{ msg.content }}
{% endfor %}
{% endif %}

{{ ctx.output_format }}"#

}

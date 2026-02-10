function ChooseNotionAction(user_message: string) -> SupportNotionSearchPagesSessionPlan | SupportNotionGetPageSessionPlan | SupportNotionGetPageBlocksSessionPlan {
  client DeepSeekOpenRouter
  prompt #"
    You are a Notion assistant. This tool is read-only.

    Available tools:
    - support/notionSearchPages: search Notion pages by query (optional query).
    - support/notionGetPage: get details of a page (needs page_id).
    - support/notionGetPageBlocks: retrieve block content for a page or block (needs block_id).

    Produce a session plan that opens a tool session, sends the
    appropriate input, retrieves the result, and finishes.

    {{ ctx.output_format }}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

class NotionSummary {
  summary string
}

function SummarizeNotionContent(
  user_message: string,
  page_title: string?,
  page_url: string?,
  blocks_text: string
) -> NotionSummary {
  client DeepSeekOpenRouter
  prompt #"
    You summarize Notion content for busy engineers and BizDev.

    Output format (exact headings):
    Commitments:
    - ...
    Conflicts:
    - ...
    Missing:
    - ...

    Requirements:
    - Use plain language.
    - Extract concrete commitments (timelines, UX expectations, performance).
    - If no conflicts or missing items, say \"None found\" under the heading.
    - Keep it concise.

    Page title: {{ page_title }}
    Page url: {{ page_url }}

    User question: {{ user_message }}

    Notion content:
    {{ blocks_text }}
  "#
}

client DeepSeekOpenRouter {
  provider openai-generic
  options {
    model "deepseek/deepseek-chat"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

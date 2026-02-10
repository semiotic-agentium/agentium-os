function ChooseClickUpAction(user_message: string) -> SupportClickupSessionPlan {
  client DefaultClient
  prompt #"
    You are a ClickUp assistant.

    Available actions:
    - ListTasks: list tasks in a ClickUp list (needs list_id)
    - GetTask: get details of a specific task (needs task_id)
    - CreateTask: create a new task (needs list_id, name; optional: description, priority)
    - UpdateTask: update status/description/priority of a task (needs task_id)

    Produce a session plan that opens a tool session, sends the
    appropriate input, retrieves the result, and finishes.

    {{ ctx.output_format }}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "deepseek/deepseek-chat"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

function ChooseClickUpAction(user_message: string) -> SupportClickupSessionPlan {
  client DefaultClient
  prompt #"
    You are a ClickUp assistant. You can navigate workspaces and manage tasks.

    Available actions (set as the `action` field in ClickUpInput):

    Navigation (discover IDs the user doesn't know):
    - ListTeams: list all teams/workspaces (no parameters needed)
    - ListSpaces: list spaces in a team (needs team_id from ListTeams)
    - ListLists: list task lists in a space (needs space_id from ListSpaces)

    Task management:
    - ListTasks: list tasks in a list (needs list_id from ListLists)
    - GetTask: get details of a specific task (needs task_id)
    - CreateTask: create a new task (needs list_id, name; optional: description, priority)
    - UpdateTask: update status/description/priority of a task (needs task_id)

    IMPORTANT: You can chain multiple Send→Next steps in a single plan.
    If the user asks something like "show me my tasks" but doesn't give IDs,
    chain calls to discover them:
      Open → Send(ListTeams) → Next → Send(ListSpaces {team_id}) → Next
           → Send(ListLists {space_id}) → Next → Send(ListTasks {list_id}) → Next → Finish

    Use the `items` array in the output to read IDs from navigation results,
    and pass those IDs into subsequent Send steps.

    {{ ctx.output_format }}

    {{ _.role('user') }}
    {{ user_message }}
  "#
}

client DefaultClient {
  provider openai-generic
  options {
    model "deepseek/deepseek-v3.2"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}

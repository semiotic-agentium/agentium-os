//! ClickUp tool — `support/clickup`.
//!
//! Provides a [`BamlTool`] implementation that calls the ClickUp v2 REST API.
//! Supports listing, getting, creating, and updating tasks.

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{baml_tool, bundles::Support, tools::BamlTool};
/// ClickUp v2 REST API base URL.
pub use integrations_clickup_client::BASE_URL;
use integrations_clickup_client::{ClickUpClient, ClickUpClientError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

fn option_is_empty(opt: &Option<String>) -> bool {
    opt.as_ref().is_none_or(|s| s.is_empty())
}

const GET_TASK_DESCRIPTION_MAX_CHARS: usize = 120;
const SINGLE_TASK_DESCRIPTION_MAX_CHARS: usize = 1200;

fn truncate_chars_with_ellipsis(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars) + 3);
    for (i, ch) in input.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn normalize_optional_description(desc: &mut Option<String>, max_chars: usize) {
    let Some(current) = desc.as_ref() else {
        return;
    };
    let trimmed = current.trim();
    if trimmed.is_empty() {
        *desc = None;
        return;
    }
    if trimmed.chars().count() > max_chars {
        *desc = Some(truncate_chars_with_ellipsis(trimmed, max_chars));
    } else if trimmed.len() != current.len() {
        *desc = Some(trimmed.to_string());
    }
}

// ---------------------------------------------------------------------------
// Per-action input types
// ---------------------------------------------------------------------------

/// List all teams/workspaces — no parameters needed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ListTeamsInput {}

/// List spaces in a team.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ListSpacesInput {
    pub team_id: String,
}

/// List task-lists in a space.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ListListsInput {
    pub space_id: String,
}

/// List tasks in a list.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ListTasksInput {
    pub list_id: String,
}

/// Get details of a specific task.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct GetTaskInput {
    pub task_id: String,
}

/// Create a new task in a list.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CreateTaskInput {
    pub list_id: String,
    pub name: String,
    /// The task description.
    pub description: Option<String>,
    /// ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.
    #[baml(description = "ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")]
    pub priority: Option<u8>,
}

/// Update an existing task's status, description, or priority.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct UpdateTaskInput {
    pub task_id: String,
    /// Task status string (e.g. "in progress").
    #[baml(description = "Task status string (e.g. in progress).")]
    pub status: Option<String>,
    /// The task description.
    pub description: Option<String>,
    /// ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.
    #[baml(description = "ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")]
    pub priority: Option<u8>,
}

/// Delete a task from the workspace.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DeleteTaskInput {
    pub task_id: String,
    /// Must be `true` for the deletion to proceed. The LLM should first
    /// confirm with the user before setting this to `true`.
    pub confirm_delete: bool,
}

/// Union of all ClickUp action inputs.
///
/// The LLM picks the appropriate variant based on the desired action.
/// In generated BAML this becomes:
/// `type ClickUpInput = ListTeamsInput | ListSpacesInput | … | DeleteTaskInput`
///
/// **Variant order matters**: `serde(untagged)` tries variants top-down and
/// takes the first successful match. Ordering invariants:
/// - `GetTask` must precede `UpdateTask` so that `{ "task_id": "..." }`
///   (without optional update fields) resolves to `GetTask`, not `UpdateTask`.
/// - `DeleteTask` has `confirm_delete: bool` which disambiguates it from
///   `GetTask`, so its position is flexible.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[baml(union)]
#[serde(untagged)]
#[ts(export)]
pub enum ClickUpInput {
    ListTeams(ListTeamsInput),
    ListSpaces(ListSpacesInput),
    ListLists(ListListsInput),
    ListTasks(ListTasksInput),
    GetTask(GetTaskInput),
    CreateTask(CreateTaskInput),
    UpdateTask(UpdateTaskInput),
    DeleteTask(DeleteTaskInput),
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Summary of a single ClickUp task, projected from the API response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpTaskSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "option_is_empty")]
    pub description: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
}

/// A generic item returned by navigation actions (teams, spaces, lists).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpItem {
    pub id: String,
    pub name: String,
    /// The kind of resource: "team", "space", or "list".
    #[baml(description = "The kind of resource: team, space, or list.")]
    pub kind: String,
}

/// Output returned by every ClickUp tool action.
///
/// Task actions populate `tasks`; navigation actions populate `items`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickUpOperation {
    ListTeams,
    ListSpaces,
    ListLists,
    ListTasks,
    GetTask,
    CreateTask,
    UpdateTask,
    DeleteTask,
}

/// Output returned by every ClickUp tool action.
///
/// Task actions populate `tasks`; navigation actions populate `items`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<ClickUpTaskSummary>,
    /// Teams, spaces, or lists returned by navigation actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[baml(description = "Teams, spaces, or lists returned by navigation actions.")]
    pub items: Vec<ClickUpItem>,
    pub message: String,
    #[baml(skip)]
    #[schemars(skip)]
    #[ts(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ClickUpOperation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawClickUpTask {
    pub id: String,
    pub name: String,
    pub status: RawStatus,
    pub description: Option<String>,
    pub url: String,
    #[serde(default)]
    pub assignees: Vec<RawAssignee>,
    pub priority: Option<RawPriority>,
    pub due_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawStatus {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawAssignee {
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPriority {
    pub priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTaskList {
    #[serde(default)]
    pub tasks: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTeamList {
    #[serde(default)]
    pub teams: Vec<RawTeam>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTeam {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSpaceList {
    #[serde(default)]
    pub spaces: Vec<RawSpace>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSpace {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawFolderlessList {
    #[serde(default)]
    pub lists: Vec<RawClickUpList>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawClickUpList {
    pub id: String,
    pub name: String,
}

impl From<RawClickUpTask> for ClickUpTaskSummary {
    fn from(raw: RawClickUpTask) -> Self {
        Self {
            id: raw.id,
            name: raw.name,
            status: raw.status.status,
            description: raw.description,
            url: raw.url,
            assignees: raw
                .assignees
                .into_iter()
                .filter_map(|a| a.username)
                .collect(),
            priority: raw.priority.and_then(|p| p.priority),
            due_date: raw.due_date,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClickUpError {
    #[error(transparent)]
    Client(#[from] ClickUpClientError),
    #[error("Unexpected ClickUp response shape: {message}")]
    UnexpectedShape { message: String },
}

impl From<ClickUpError> for BamlRtError {
    fn from(err: ClickUpError) -> Self {
        match err {
            ClickUpError::Client(inner) => match inner {
                ClickUpClientError::MissingApiKey(_) | ClickUpClientError::Unauthorized { .. } => {
                    BamlRtError::Configuration(inner.to_string())
                }
                ClickUpClientError::NotFound { .. } => {
                    BamlRtError::InvalidArgument(inner.to_string())
                }
                ClickUpClientError::Http(_)
                | ClickUpClientError::RateLimited { .. }
                | ClickUpClientError::Api { .. } => BamlRtError::ToolExecution(inner.to_string()),
            },
            ClickUpError::UnexpectedShape { message } => BamlRtError::ToolExecution(message),
        }
    }
}

pub struct ClickUpTool {
    client: ClickUpClient,
}

impl Default for ClickUpTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickUpTool {
    pub fn new() -> Self {
        Self {
            client: ClickUpClient::new(),
        }
    }

    fn api_key() -> std::result::Result<String, ClickUpError> {
        ClickUpClient::api_key().map_err(ClickUpError::Client)
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_teams(&self, api_key: &str) -> Result<ClickUpOutput> {
        let json = self
            .client
            .send_json(self.client.get("/team", api_key))
            .await
            .map_err(ClickUpError::Client)?;
        let raw: RawTeamList =
            serde_json::from_value(json).map_err(|e| ClickUpError::UnexpectedShape {
                message: format!("unexpected teams response shape: {e}"),
            })?;

        let items: Vec<ClickUpItem> = raw
            .teams
            .into_iter()
            .map(|t| ClickUpItem {
                id: t.id,
                name: t.name,
                kind: "team".to_string(),
            })
            .collect();

        let count = items.len();
        Ok(ClickUpOutput {
            tasks: vec![],
            items,
            message: format!("Found {count} team(s)"),
            operation: Some(ClickUpOperation::ListTeams),
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_spaces(&self, api_key: &str, team_id: &str) -> Result<ClickUpOutput> {
        let json = self
            .client
            .send_json(self.client.get(&format!("/team/{team_id}/space"), api_key))
            .await
            .map_err(ClickUpError::Client)?;
        let raw: RawSpaceList =
            serde_json::from_value(json).map_err(|e| ClickUpError::UnexpectedShape {
                message: format!("unexpected spaces response shape: {e}"),
            })?;

        let items: Vec<ClickUpItem> = raw
            .spaces
            .into_iter()
            .map(|s| ClickUpItem {
                id: s.id,
                name: s.name,
                kind: "space".to_string(),
            })
            .collect();

        let count = items.len();
        Ok(ClickUpOutput {
            tasks: vec![],
            items,
            message: format!("Found {count} space(s) in team {team_id}"),
            operation: Some(ClickUpOperation::ListSpaces),
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_lists(&self, api_key: &str, space_id: &str) -> Result<ClickUpOutput> {
        let json = self
            .client
            .send_json(self.client.get(&format!("/space/{space_id}/list"), api_key))
            .await
            .map_err(ClickUpError::Client)?;

        let raw: RawFolderlessList =
            serde_json::from_value(json).map_err(|e| ClickUpError::UnexpectedShape {
                message: format!("unexpected lists response shape: {e}"),
            })?;

        let items: Vec<ClickUpItem> = raw
            .lists
            .into_iter()
            .map(|l| ClickUpItem {
                id: l.id,
                name: l.name,
                kind: "list".to_string(),
            })
            .collect();

        let count = items.len();
        Ok(ClickUpOutput {
            tasks: vec![],
            items,
            message: format!("Found {count} list(s) in space {space_id}"),
            operation: Some(ClickUpOperation::ListLists),
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_tasks(&self, api_key: &str, list_id: &str) -> Result<ClickUpOutput> {
        let json = self
            .client
            .send_json(self.client.get(&format!("/list/{list_id}/task"), api_key))
            .await
            .map_err(ClickUpError::Client)?;

        let raw_list: RawTaskList =
            serde_json::from_value(json).map_err(|e| ClickUpError::UnexpectedShape {
                message: format!("unexpected response shape: {e}"),
            })?;

        let mut tasks = Vec::with_capacity(raw_list.tasks.len());
        for (idx, raw_value) in raw_list.tasks.into_iter().enumerate() {
            match serde_json::from_value::<RawClickUpTask>(raw_value) {
                Ok(raw_task) => tasks.push(ClickUpTaskSummary::from(raw_task)),
                Err(err) => {
                    tracing::warn!(
                        task_index = idx,
                        error = %err,
                        "Skipping malformed task entry from ClickUp response",
                    );
                }
            }
        }

        let count = tasks.len();
        Ok(ClickUpOutput {
            tasks,
            items: vec![],
            message: format!("Found {count} task(s) in list {list_id}"),
            operation: Some(ClickUpOperation::ListTasks),
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn get_task(&self, api_key: &str, task_id: &str) -> Result<ClickUpOutput> {
        let json = self
            .client
            .send_json(self.client.get(&format!("/task/{task_id}"), api_key))
            .await
            .map_err(ClickUpError::Client)?;

        let raw: RawClickUpTask =
            serde_json::from_value(json).map_err(|e| ClickUpError::UnexpectedShape {
                message: format!("unexpected task shape: {e}"),
            })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Task {task_id}: {}", summary.name),
            tasks: vec![summary],
            items: vec![],
            operation: Some(ClickUpOperation::GetTask),
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn create_task(
        &self,
        api_key: &str,
        list_id: &str,
        name: &str,
        description: Option<&str>,
        priority: Option<u8>,
    ) -> Result<ClickUpOutput> {
        let mut body = serde_json::json!({ "name": name });
        if let Some(desc) = description {
            body["description"] = serde_json::Value::String(desc.to_string());
        }
        if let Some(p) = priority {
            body["priority"] = serde_json::json!(p);
        }

        let json = self
            .client
            .send_json(
                self.client
                    .post(&format!("/list/{list_id}/task"), api_key)
                    .json(&body),
            )
            .await
            .map_err(ClickUpError::Client)?;

        let raw: RawClickUpTask =
            serde_json::from_value(json).map_err(|e| ClickUpError::UnexpectedShape {
                message: format!("unexpected task shape: {e}"),
            })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Created task '{}' in list {list_id}", summary.name),
            tasks: vec![summary],
            items: vec![],
            operation: Some(ClickUpOperation::CreateTask),
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn update_task(
        &self,
        api_key: &str,
        task_id: &str,
        status: Option<&str>,
        description: Option<&str>,
        priority: Option<u8>,
    ) -> Result<ClickUpOutput> {
        let mut body = serde_json::Map::new();
        if let Some(s) = status {
            body.insert(
                "status".to_string(),
                serde_json::Value::String(s.to_string()),
            );
        }
        if let Some(desc) = description {
            body.insert(
                "description".to_string(),
                serde_json::Value::String(desc.to_string()),
            );
        }
        if let Some(p) = priority {
            body.insert("priority".to_string(), serde_json::json!(p));
        }

        let json = self
            .client
            .send_json(
                self.client
                    .put(&format!("/task/{task_id}"), api_key)
                    .json(&serde_json::Value::Object(body)),
            )
            .await
            .map_err(ClickUpError::Client)?;

        let raw: RawClickUpTask =
            serde_json::from_value(json).map_err(|e| ClickUpError::UnexpectedShape {
                message: format!("unexpected task shape: {e}"),
            })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Updated task {task_id}"),
            tasks: vec![summary],
            items: vec![],
            operation: Some(ClickUpOperation::UpdateTask),
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn delete_task(&self, api_key: &str, task_id: &str) -> Result<ClickUpOutput> {
        self.client
            .send_no_content(self.client.delete(&format!("/task/{task_id}"), api_key))
            .await
            .map_err(ClickUpError::Client)?;

        Ok(ClickUpOutput {
            message: format!("Deleted task {task_id}"),
            tasks: vec![],
            items: vec![],
            operation: Some(ClickUpOperation::DeleteTask),
        })
    }

    fn compact_clickup_output(output: &mut ClickUpOutput) {
        match output.operation {
            Some(ClickUpOperation::ListTasks) => {
                for task in &mut output.tasks {
                    task.description = None;
                }
            }
            Some(ClickUpOperation::GetTask) => {
                for task in &mut output.tasks {
                    normalize_optional_description(
                        &mut task.description,
                        GET_TASK_DESCRIPTION_MAX_CHARS,
                    );
                }
            }
            Some(ClickUpOperation::CreateTask) | Some(ClickUpOperation::UpdateTask) => {
                for task in &mut output.tasks {
                    task.description = None;
                }
            }
            _ => {
                for task in &mut output.tasks {
                    normalize_optional_description(
                        &mut task.description,
                        SINGLE_TASK_DESCRIPTION_MAX_CHARS,
                    );
                }
            }
        }

        // Internal host metadata is useful in raw provenance but unnecessary in
        // prompt-projection context.
        output.operation = None;
    }

    fn compact_tool_result_payload(content: &mut serde_json::Value) {
        let Ok(mut output) = serde_json::from_value::<ClickUpOutput>(content.clone()) else {
            return;
        };
        Self::compact_clickup_output(&mut output);
        if let Ok(compacted) = serde_json::to_value(output) {
            *content = compacted;
        }
    }
}

#[baml_tool(
    name = "support/clickup",
    description = "Interact with ClickUp: navigate workspaces (teams, spaces, lists) and manage tasks.",
    tags = ["support", "clickup"],
    secrets = [
        { name = "CLICKUP_API_KEY", description = "ClickUp personal API token (pk_...)", reason = "Required to authenticate with the ClickUp v2 API" }
    ],
    baml_types = [
        ListTeamsInput, ListSpacesInput, ListListsInput, ListTasksInput,
        GetTaskInput, CreateTaskInput, UpdateTaskInput, DeleteTaskInput,
        ClickUpInput, ClickUpTaskSummary, ClickUpItem, ClickUpOutput,
    ],
)]
#[async_trait]
impl BamlTool for ClickUpTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "clickup";
    type OpenInput = ();
    type Input = ClickUpInput;
    type Output = ClickUpOutput;

    fn description(&self) -> &'static str {
        "Interact with ClickUp: navigate workspaces (teams, spaces, lists) and manage tasks."
    }

    #[tracing::instrument(skip(self), fields(action))]
    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let action = match &args {
            ClickUpInput::ListTeams(_) => "ListTeams",
            ClickUpInput::ListSpaces(_) => "ListSpaces",
            ClickUpInput::ListLists(_) => "ListLists",
            ClickUpInput::ListTasks(_) => "ListTasks",
            ClickUpInput::GetTask(_) => "GetTask",
            ClickUpInput::CreateTask(_) => "CreateTask",
            ClickUpInput::UpdateTask(_) => "UpdateTask",
            ClickUpInput::DeleteTask(_) => "DeleteTask",
        };
        tracing::Span::current().record("action", action);

        let api_key = Self::api_key()?;
        match args {
            ClickUpInput::ListTeams(_) => self.list_teams(&api_key).await,
            ClickUpInput::ListSpaces(input) => self.list_spaces(&api_key, &input.team_id).await,
            ClickUpInput::ListLists(input) => self.list_lists(&api_key, &input.space_id).await,
            ClickUpInput::ListTasks(input) => self.list_tasks(&api_key, &input.list_id).await,
            ClickUpInput::GetTask(input) => self.get_task(&api_key, &input.task_id).await,
            ClickUpInput::CreateTask(input) => {
                self.create_task(
                    &api_key,
                    &input.list_id,
                    &input.name,
                    input.description.as_deref(),
                    input.priority,
                )
                .await
            }
            ClickUpInput::UpdateTask(input) => {
                self.update_task(
                    &api_key,
                    &input.task_id,
                    input.status.as_deref(),
                    input.description.as_deref(),
                    input.priority,
                )
                .await
            }
            ClickUpInput::DeleteTask(input) => {
                if !input.confirm_delete {
                    Ok(ClickUpOutput {
                        message: format!(
                            "Task {} identified. Please confirm you want to delete it.",
                            input.task_id
                        ),
                        tasks: vec![],
                        items: vec![],
                        operation: Some(ClickUpOperation::DeleteTask),
                    })
                } else {
                    self.delete_task(&api_key, &input.task_id).await
                }
            }
        }
    }

    fn compact_result(&self, content: &mut serde_json::Value) {
        // Provider-side tool_result content usually has envelope shape:
        // {"tool_name":"support/clickup","fsm_phase":"send","result":{...}}
        // Compact the nested result when present; otherwise compact the value directly.
        if let Some(result) = content.get_mut("result") {
            Self::compact_tool_result_payload(result);
        } else {
            Self::compact_tool_result_payload(content);
        }
    }
}

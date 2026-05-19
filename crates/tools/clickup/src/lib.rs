// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! ClickUp tool — `support/clickup`.
//!
//! Provides a [`BamlTool`] implementation that calls the ClickUp v2 REST API.
//! Supports listing, getting, creating, and updating tasks.

mod event_source_type;
pub mod poll;
mod source_records;
mod spans;

use std::sync::Arc;

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result, semantics::ErrorDisposition};
use baml_rt_tools::{
    ClassifiedToolError, baml_tool,
    bundles::Support,
    tools::{BamlTool, ToolHandler, create_tool_handler},
};
/// ClickUp v2 REST API base URL.
pub use integrations_clickup_client::BASE_URL;
use integrations_clickup_client::{ClickUpClient, ClickUpClientError};
pub use poll::{
    ClickupInferredPriority, ClickupInferredTask, ClickupLifecycleRevisionSlot, ClickupPollOutcome,
    ClickupPollState, ClickupSourceConfig, ClickupSourceConfigError, ClickupSourceReference,
    ClickupTaskSnapshot, poll_clickup_lists,
};
use serde::{Deserialize, Serialize};
pub use source_records::{
    ClickupLifecycleTaskInput, ClickupLifecycleTaskRecord, ClickupProjectContext,
    ClickupSourceRecordsBatch, batch_from_lifecycle_tasks, clickup_source_records_json_schema,
};
use tracing::Instrument;

fn option_is_empty(opt: &Option<String>) -> bool {
    opt.as_ref().is_none_or(|s| s.is_empty())
}

// ---------------------------------------------------------------------------
// Per-action input types
// ---------------------------------------------------------------------------

/// List all teams/workspaces — no parameters needed.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ListTeamsInput {}

/// List spaces in a team. Call ListTeams first to get the team_id.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ListSpacesInput {
    #[baml(description = "ClickUp team (workspace) ID — obtain from ListTeams.")]
    pub team_id: String,
}

/// List task-lists in a space. Call ListSpaces first to get the space_id.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ListListsInput {
    #[baml(description = "ClickUp space ID — obtain from ListSpaces.")]
    pub space_id: String,
}

/// List tasks in a list. Call ListLists first to get the list_id.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct ListTasksInput {
    #[baml(description = "ClickUp list ID — obtain from ListLists.")]
    pub list_id: String,
}

/// Get full details of a specific task (description, status, priority, assignees).
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct GetTaskInput {
    #[baml(description = "ClickUp task ID — obtain from ListTasks.")]
    pub task_id: String,
}

/// Create a new task in a list.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskInput {
    #[baml(
        description = "ClickUp list ID where the task will be created — obtain from ListLists."
    )]
    pub list_id: String,
    #[baml(description = "Task title / name.")]
    pub name: String,
    #[baml(description = "Optional long-form task description (Markdown supported).")]
    pub description: Option<String>,
    #[baml(description = "ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")]
    pub priority: Option<u8>,
}

/// Update an existing task's status, description, or priority.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct UpdateTaskInput {
    #[baml(description = "ClickUp task ID to update — obtain from ListTasks or GetTask.")]
    pub task_id: String,
    #[baml(description = "New task status string (e.g. 'in progress', 'complete').")]
    pub status: Option<String>,
    #[baml(description = "Updated task description (Markdown supported).")]
    pub description: Option<String>,
    #[baml(description = "ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")]
    pub priority: Option<u8>,
}

/// Permanently delete a task. Confirm with the user before proceeding.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct DeleteTaskInput {
    #[baml(description = "ClickUp task ID to delete — obtain from ListTasks.")]
    pub task_id: String,
    #[baml(description = "Must be true to proceed. Always confirm deletion with the user first.")]
    pub confirm_delete: bool,
}

/// ClickUp tool — navigate teams → spaces → lists → tasks.
/// ListTeams → ListSpaces → ListLists → ListTasks → GetTask/CreateTask/UpdateTask/DeleteTask.
/// IDs must come from prior navigation results, not invented.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[baml(union)]
#[serde(untagged)]
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

impl baml_rt_tools::DescribeAction for ClickUpInput {
    fn describe(&self) -> String {
        match self {
            ClickUpInput::ListTeams(_) => "listing ClickUp teams".to_string(),
            ClickUpInput::ListSpaces(_) => "listing ClickUp spaces".to_string(),
            ClickUpInput::ListLists(_) => "listing ClickUp task lists".to_string(),
            ClickUpInput::ListTasks(_) => "listing ClickUp tasks".to_string(),
            ClickUpInput::GetTask(_) => "retrieving ClickUp task details".to_string(),
            ClickUpInput::CreateTask(p) => format!("creating ClickUp task '{}'", p.name),
            ClickUpInput::UpdateTask(_) => "updating ClickUp task".to_string(),
            ClickUpInput::DeleteTask(_) => "deleting ClickUp task".to_string(),
        }
    }
}

baml_rt_tools::impl_describe_action_identity! {
    for ClickUpInput {
        ListTeams(_) => "list_teams" {},
        ListSpaces(p) => "list_spaces" { team_id: p.team_id },
        ListLists(p) => "list_lists" { space_id: p.space_id },
        ListTasks(p) => "list_tasks" { list_id: p.list_id },
        GetTask(p) => "get_task" { task_id: p.task_id },
        CreateTask(p) => "create_task" { list_id: p.list_id, name: p.name },
        UpdateTask(p) => "update_task" { task_id: p.task_id },
        DeleteTask(p) => "delete_task" { task_id: p.task_id },
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Summary of a single ClickUp task, projected from the API response.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
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
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
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
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct ClickUpOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<ClickUpTaskSummary>,
    /// Teams, spaces, or lists returned by navigation actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[baml(description = "Teams, spaces, or lists returned by navigation actions.")]
    pub items: Vec<ClickUpItem>,
    pub message: String,
    #[baml(skip)]
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
                ClickUpClientError::MissingApiKey | ClickUpClientError::Unauthorized { .. } => {
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

impl ClickUpTool {
    pub fn new() -> std::result::Result<Self, ClickUpError> {
        Ok(Self {
            client: ClickUpClient::new(),
        })
    }

    /// Same as [`Self::new`] but targets a custom ClickUp API base URL (fixture / proxy).
    pub fn with_base_url(base: impl Into<String>) -> std::result::Result<Self, ClickUpError> {
        Ok(Self {
            client: ClickUpClient::with_base_url(base),
        })
    }

    async fn list_teams(&self, api_key: &str) -> Result<ClickUpOutput> {
        async {
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
            let next_hint = items
                .first()
                .map(|t| format!(" Next: ListSpacesInput with team_id: {}", t.id))
                .unwrap_or_default();
            Ok(ClickUpOutput {
                tasks: vec![],
                items,
                message: format!("Found {count} team(s).{next_hint}"),
                operation: Some(ClickUpOperation::ListTeams),
            })
        }
        .instrument(spans::list_teams())
        .await
    }

    async fn list_spaces(&self, api_key: &str, team_id: &str) -> Result<ClickUpOutput> {
        async {
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
            let next_hint = items
                .first()
                .map(|s| format!(" Next: ListListsInput with space_id: {}", s.id))
                .unwrap_or_default();
            Ok(ClickUpOutput {
                tasks: vec![],
                items,
                message: format!("Found {count} space(s) in team {team_id}.{next_hint}"),
                operation: Some(ClickUpOperation::ListSpaces),
            })
        }
        .instrument(spans::list_spaces(team_id))
        .await
    }

    async fn list_lists(&self, api_key: &str, space_id: &str) -> Result<ClickUpOutput> {
        async {
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
            let next_hint = items
                .first()
                .map(|l| format!(" Next: ListTasksInput with list_id: {}", l.id))
                .unwrap_or_default();
            Ok(ClickUpOutput {
                tasks: vec![],
                items,
                message: format!("Found {count} list(s) in space {space_id}.{next_hint}"),
                operation: Some(ClickUpOperation::ListLists),
            })
        }
        .instrument(spans::list_lists(space_id))
        .await
    }

    async fn list_tasks(&self, api_key: &str, list_id: &str) -> Result<ClickUpOutput> {
        async {
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
        .instrument(spans::list_tasks(list_id))
        .await
    }

    async fn get_task(&self, api_key: &str, task_id: &str) -> Result<ClickUpOutput> {
        async {
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
        .instrument(spans::get_task(task_id))
        .await
    }

    async fn create_task(
        &self,
        api_key: &str,
        list_id: &str,
        name: &str,
        description: Option<&str>,
        priority: Option<u8>,
    ) -> Result<ClickUpOutput> {
        async {
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
        .instrument(spans::create_task(list_id))
        .await
    }

    async fn update_task(
        &self,
        api_key: &str,
        task_id: &str,
        status: Option<&str>,
        description: Option<&str>,
        priority: Option<u8>,
    ) -> Result<ClickUpOutput> {
        async {
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
        .instrument(spans::update_task(task_id))
        .await
    }

    async fn delete_task(&self, api_key: &str, task_id: &str) -> Result<ClickUpOutput> {
        async {
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
        .instrument(spans::delete_task(task_id))
        .await
    }
}

fn build_clickup_tool() -> Result<Arc<dyn ToolHandler>> {
    let tool = ClickUpTool::new()?;
    create_tool_handler(tool).map(|(_, h)| h)
}

#[baml_tool(
    name = "support/clickup",
    description = "Interact with ClickUp: navigate workspaces (teams, spaces, lists) and manage tasks.",
    tags = ["support", "clickup"],
    event_sources = ["clickup"],
    access = Write,
    secrets = [
        { name = "CLICKUP_API_KEY", description = "ClickUp personal API token (pk_...)", reason = "Required to authenticate with the ClickUp v2 API" }
    ],
    baml_types = [
        ListTeamsInput, ListSpacesInput, ListListsInput, ListTasksInput,
        GetTaskInput, CreateTaskInput, UpdateTaskInput, DeleteTaskInput,
        ClickUpInput, ClickUpTaskSummary, ClickUpItem, ClickUpOutput,
    ],
    build_with = build_clickup_tool,
)]
#[async_trait]
impl BamlTool for ClickUpTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "clickup";
    const SESSION_POLICY: baml_rt_tools::SessionPolicy = baml_rt_tools::SessionPolicy::MultiSend;
    type OpenInput = ();
    type Input = ClickUpInput;
    type Output = ClickUpOutput;

    fn description(&self) -> &'static str {
        "Interact with ClickUp: navigate workspaces (teams, spaces, lists) and manage tasks."
    }

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
        let span = spans::execute(action);

        async {
            let api_key = self
                .client
                .resolve_api_key()
                .map_err(ClickUpError::Client)?;
            let mut output = match args {
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
            }?;
            output.operation = None;
            Ok(output)
        }
        .instrument(span)
        .await
    }

    fn action_identity(&self, input: &Self::Input) -> Option<baml_rt_tools::ActionIdentity> {
        Some(baml_rt_tools::DescribeActionIdentity::action_identity(
            input,
        ))
    }

    fn describe_result(&self, output: &Self::Output) -> String {
        let task_count = output.tasks.len();
        let item_count = output.items.len();
        if task_count > 0 {
            format!("returned {} ClickUp task(s)", task_count)
        } else if item_count > 0 {
            format!("returned {} ClickUp item(s)", item_count)
        } else {
            output.message.clone()
        }
    }

    fn describe_open(&self) -> String {
        "using ClickUp for workspace navigation and task management".to_string()
    }

    fn classify_execution_error(err: &BamlRtError) -> ClassifiedToolError {
        let mut c = ClassifiedToolError::from_baml_error(err);
        if let BamlRtError::ToolExecution(msg) = err {
            let lower = msg.to_ascii_lowercase();
            if lower.contains("404") || lower.contains("not found") {
                c.disposition = ErrorDisposition::InformAndContinue;
                c.code = "clickup_not_found".to_string();
                c.hint = Some(
                    "Verify team_id, space_id, list_id, or task_id exist in the workspace."
                        .to_string(),
                );
            } else if lower.contains("429") {
                c.disposition = ErrorDisposition::HostRetriable;
                c.code = "clickup_rate_limited".to_string();
            }
        }
        c
    }
}

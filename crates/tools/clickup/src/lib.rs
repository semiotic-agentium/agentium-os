//! ClickUp tool — `support/clickup`.
//!
//! Provides a [`BamlTool`] implementation that calls the ClickUp v2 REST API.
//! Supports listing, getting, creating, and updating tasks.

use std::sync::Arc;

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_derive_core::BamlType as BamlTypeTrait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolMetadataBuilder, TypeBasedMetadataBuilder,
    bundles::Support,
    parse_tool_name_and_class, register_tool,
    tools::{
        BamlTool, ToolFunctionMetadata, ToolHandler, ToolSecretRequirement, create_tool_handler,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// ClickUp v2 REST API base URL.
pub const BASE_URL: &str = "https://api.clickup.com/api/v2";

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
    pub description: Option<String>,
    pub url: String,
    pub assignees: Vec<String>,
    pub priority: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpOutput {
    pub tasks: Vec<ClickUpTaskSummary>,
    /// Teams, spaces, or lists returned by navigation actions.
    #[baml(description = "Teams, spaces, or lists returned by navigation actions.")]
    pub items: Vec<ClickUpItem>,
    pub message: String,
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
    #[error("ClickUp HTTP request failed")]
    Http(#[source] reqwest::Error),

    #[error("ClickUp API authentication failed (401): {body}")]
    Unauthorized { body: String },

    #[error("ClickUp resource not found (404): {body}")]
    NotFound { body: String },

    #[error("ClickUp rate limit exceeded (429), resets at {reset_at}: {body}")]
    RateLimited { body: String, reset_at: String },

    #[error("ClickUp API returned {status}: {body}")]
    Api { status: u16, body: String },

    #[error("Failed to deserialize ClickUp response")]
    Deserialize(#[source] reqwest::Error),

    #[error("CLICKUP_API_KEY environment variable not set")]
    MissingApiKey(#[source] std::env::VarError),
}

impl From<ClickUpError> for BamlRtError {
    fn from(err: ClickUpError) -> Self {
        match &err {
            ClickUpError::MissingApiKey(_) | ClickUpError::Unauthorized { .. } => {
                BamlRtError::Configuration(err.to_string())
            }
            ClickUpError::NotFound { .. } => BamlRtError::InvalidArgument(err.to_string()),
            ClickUpError::Http(_)
            | ClickUpError::RateLimited { .. }
            | ClickUpError::Api { .. }
            | ClickUpError::Deserialize(_) => BamlRtError::ToolExecution(err.to_string()),
        }
    }
}

pub struct ClickUpTool {
    client: reqwest::Client,
}

impl Default for ClickUpTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickUpTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    fn base_url() -> String {
        std::env::var("CLICKUP_API_BASE_URL")
            .ok()
            .map(|raw| raw.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| BASE_URL.to_string())
    }

    fn api_key() -> std::result::Result<String, ClickUpError> {
        std::env::var("CLICKUP_API_KEY").map_err(ClickUpError::MissingApiKey)
    }

    #[tracing::instrument(skip_all, fields(url))]
    async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<serde_json::Value, ClickUpError> {
        let resp = request.send().await.map_err(ClickUpError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            let code = status.as_u16();
            let reset_at = resp
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            let body = resp.text().await.unwrap_or_default();

            // ClickUp returns 401 with ECODE "OAUTH_017"/"OAUTH_018"/"OAUTH_019"
            // when a resource ID is invalid or inaccessible, rather than a proper
            // 404. Detect this pattern and reclassify as NotFound so the LLM gets
            // an actionable error instead of a misleading auth failure.
            let is_fake_auth_error = code == 401
                && body.contains("OAUTH_0")
                && (body.contains("token not found") || body.contains("Token not found"));

            return Err(match code {
                401 if is_fake_auth_error => ClickUpError::NotFound {
                    body: format!("Resource not found : {body}"),
                },
                401 => ClickUpError::Unauthorized { body },
                404 => ClickUpError::NotFound { body },
                429 => ClickUpError::RateLimited { body, reset_at },
                _ => ClickUpError::Api { status: code, body },
            });
        }

        resp.json().await.map_err(ClickUpError::Deserialize)
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_teams(&self, api_key: &str) -> Result<ClickUpOutput> {
        let base_url = Self::base_url();
        let json = self
            .send_request(
                self.client
                    .get(format!("{base_url}/team"))
                    .header("Authorization", api_key),
            )
            .await?;

        let raw: RawTeamList = serde_json::from_value(json).map_err(|e| ClickUpError::Api {
            status: 0,
            body: format!("unexpected teams response shape: {e}"),
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
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_spaces(&self, api_key: &str, team_id: &str) -> Result<ClickUpOutput> {
        let base_url = Self::base_url();
        let json = self
            .send_request(
                self.client
                    .get(format!("{base_url}/team/{team_id}/space"))
                    .header("Authorization", api_key),
            )
            .await?;

        let raw: RawSpaceList = serde_json::from_value(json).map_err(|e| ClickUpError::Api {
            status: 0,
            body: format!("unexpected spaces response shape: {e}"),
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
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_lists(&self, api_key: &str, space_id: &str) -> Result<ClickUpOutput> {
        let base_url = Self::base_url();
        let json = self
            .send_request(
                self.client
                    .get(format!("{base_url}/space/{space_id}/list"))
                    .header("Authorization", api_key),
            )
            .await?;

        let raw: RawFolderlessList =
            serde_json::from_value(json).map_err(|e| ClickUpError::Api {
                status: 0,
                body: format!("unexpected lists response shape: {e}"),
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
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn list_tasks(&self, api_key: &str, list_id: &str) -> Result<ClickUpOutput> {
        let base_url = Self::base_url();
        let json = self
            .send_request(
                self.client
                    .get(format!("{base_url}/list/{list_id}/task"))
                    .header("Authorization", api_key),
            )
            .await?;

        let raw_list: RawTaskList =
            serde_json::from_value(json).map_err(|e| ClickUpError::Api {
                status: 0,
                body: format!("unexpected response shape: {e}"),
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
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn get_task(&self, api_key: &str, task_id: &str) -> Result<ClickUpOutput> {
        let base_url = Self::base_url();
        let json = self
            .send_request(
                self.client
                    .get(format!("{base_url}/task/{task_id}"))
                    .header("Authorization", api_key),
            )
            .await?;

        let raw: RawClickUpTask = serde_json::from_value(json).map_err(|e| ClickUpError::Api {
            status: 0,
            body: format!("unexpected task shape: {e}"),
        })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Task {task_id}: {}", summary.name),
            tasks: vec![summary],
            items: vec![],
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
        let base_url = Self::base_url();
        let mut body = serde_json::json!({ "name": name });
        if let Some(desc) = description {
            body["description"] = serde_json::Value::String(desc.to_string());
        }
        if let Some(p) = priority {
            body["priority"] = serde_json::json!(p);
        }

        let json = self
            .send_request(
                self.client
                    .post(format!("{base_url}/list/{list_id}/task"))
                    .header("Authorization", api_key)
                    .json(&body),
            )
            .await?;

        let raw: RawClickUpTask = serde_json::from_value(json).map_err(|e| ClickUpError::Api {
            status: 0,
            body: format!("unexpected task shape: {e}"),
        })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Created task '{}' in list {list_id}", summary.name),
            tasks: vec![summary],
            items: vec![],
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
        let base_url = Self::base_url();
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
            .send_request(
                self.client
                    .put(format!("{base_url}/task/{task_id}"))
                    .header("Authorization", api_key)
                    .json(&serde_json::Value::Object(body)),
            )
            .await?;

        let raw: RawClickUpTask = serde_json::from_value(json).map_err(|e| ClickUpError::Api {
            status: 0,
            body: format!("unexpected task shape: {e}"),
        })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Updated task {task_id}"),
            tasks: vec![summary],
            items: vec![],
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn delete_task(&self, api_key: &str, task_id: &str) -> Result<ClickUpOutput> {
        let base_url = Self::base_url();
        let resp = self
            .client
            .delete(format!("{base_url}/task/{task_id}"))
            .header("Authorization", api_key)
            .send()
            .await
            .map_err(ClickUpError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            let code = status.as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(match code {
                401 => ClickUpError::Unauthorized { body },
                404 => ClickUpError::NotFound { body },
                429 => {
                    let reset_at = "unknown".to_string();
                    ClickUpError::RateLimited { body, reset_at }
                }
                _ => ClickUpError::Api { status: code, body },
            }
            .into());
        }

        Ok(ClickUpOutput {
            message: format!("Deleted task {task_id}"),
            tasks: vec![],
            items: vec![],
        })
    }
}

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
                    })
                } else {
                    self.delete_task(&api_key, &input.task_id).await
                }
            }
        }
    }
}

pub fn clickup_metadata() -> ToolFunctionMetadata {
    let (name, class_name) = parse_tool_name_and_class("support/clickup")
        .expect("support/clickup is a compile-time constant");

    let baml_decl = [
        ListTeamsInput::baml_decl(),
        ListSpacesInput::baml_decl(),
        ListListsInput::baml_decl(),
        ListTasksInput::baml_decl(),
        GetTaskInput::baml_decl(),
        CreateTaskInput::baml_decl(),
        UpdateTaskInput::baml_decl(),
        DeleteTaskInput::baml_decl(),
        ClickUpInput::baml_decl(),
        ClickUpTaskSummary::baml_decl(),
        ClickUpItem::baml_decl(),
        ClickUpOutput::baml_decl(),
    ]
    .join("\n\n");

    TypeBasedMetadataBuilder::<(), ClickUpInput, ClickUpOutput>::new(
        name,
        class_name,
        "Interact with ClickUp: navigate workspaces (teams, spaces, lists) and manage tasks."
            .to_string(),
    )
    .with_baml_decl(baml_decl)
    .with_tags(vec!["support".to_string(), "clickup".to_string()])
    .with_secrets(vec![ToolSecretRequirement {
        name: "CLICKUP_API_KEY".to_string(),
        description: "ClickUp personal API token (pk_...)".to_string(),
        reason: "Required to authenticate with the ClickUp v2 API".to_string(),
    }])
    .build_metadata()
}

fn clickup_build() -> Result<Arc<dyn ToolHandler>> {
    create_tool_handler(ClickUpTool::new()).map(|(_, h)| h)
}

register_tool!(clickup_metadata, clickup_build);

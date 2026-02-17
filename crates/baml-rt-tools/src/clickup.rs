//! ClickUp tools — grouped and legacy.
//!
//! Provides a [`BamlTool`] implementation that calls the ClickUp v2 REST API.
//! Supports listing, getting, creating, and updating tasks.

use crate::bundles::Support;
use crate::register_tool_metadata;
use crate::tools::{BamlTool, ToolFunctionMetadata, ToolSecretRequirement};
use async_trait::async_trait;
use baml_derive::BamlType;
use baml_derive_core::BamlType as BamlTypeTrait;
use baml_rt_core::{BamlRtError, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// ClickUp v2 REST API base URL.
pub const BASE_URL: &str = "https://api.clickup.com/api/v2";

/// Which ClickUp action to perform.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub enum ClickUpAction {
    ListTeams,
    ListSpaces,
    ListLists,
    ListTasks,
    GetTask,
    CreateTask,
    UpdateTask,
}

/// Input for the ClickUp tool.
///
/// Uses a flat struct with an `action` discriminator instead of a Rust enum
/// so that BAML (which lacks sum types) can represent it as a single class
/// with an enum field. Per-action field requirements are validated at runtime
/// in [`ClickUpTool::execute`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpInput {
    pub action: ClickUpAction,
    /// Required for `ListSpaces`.
    #[baml(description = "Required for ListSpaces.")]
    pub team_id: Option<String>,
    /// Required for `ListLists`.
    #[baml(description = "Required for ListLists.")]
    pub space_id: Option<String>,
    /// Required for `ListTasks` and `CreateTask`.
    #[baml(description = "Required for ListTasks and CreateTask.")]
    pub list_id: Option<String>,
    /// Required for `GetTask` and `UpdateTask`.
    #[baml(description = "Required for GetTask and UpdateTask.")]
    pub task_id: Option<String>,
    /// Required for `CreateTask`.
    #[baml(description = "Required for CreateTask.")]
    pub name: Option<String>,
    /// The clickup-task description
    pub description: Option<String>,
    /// ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.
    #[baml(description = "ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")]
    pub priority: Option<u8>,
    /// Task status string (e.g. \"in progress\"). Used by `UpdateTask`.
    #[baml(description = "Task status string. Used by UpdateTask.")]
    pub status: Option<String>,
}

/// Navigation-only ClickUp actions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub enum ClickUpNavigateAction {
    ListTeams,
    ListSpaces,
    ListLists,
}

/// Input for grouped navigation tool `support/clickupNavigate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpNavigateInput {
    pub navigate_action: ClickUpNavigateAction,
    /// Required for `ListSpaces`.
    #[baml(description = "Required for ListSpaces.")]
    pub team_id: Option<String>,
    /// Required for `ListLists`.
    #[baml(description = "Required for ListLists.")]
    pub space_id: Option<String>,
}

/// Task-read ClickUp actions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub enum ClickUpTasksAction {
    ListTasks,
    GetTask,
}

/// Input for grouped task-read tool `support/clickupTasks`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpTasksInput {
    pub tasks_action: ClickUpTasksAction,
    /// Required for `ListTasks`.
    #[baml(description = "Required for ListTasks.")]
    pub list_id: Option<String>,
    /// Required for `GetTask`.
    #[baml(description = "Required for GetTask.")]
    pub task_id: Option<String>,
}

/// Task-mutation ClickUp actions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub enum ClickUpMutateAction {
    CreateTask,
    UpdateTask,
}

/// Input for grouped task-mutation tool `support/clickupMutate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct ClickUpMutateInput {
    pub mutate_action: ClickUpMutateAction,
    /// Required for `CreateTask`.
    #[baml(description = "Required for CreateTask.")]
    pub list_id: Option<String>,
    /// Required for `UpdateTask`.
    #[baml(description = "Required for UpdateTask.")]
    pub task_id: Option<String>,
    /// Required for `CreateTask`.
    #[baml(description = "Required for CreateTask.")]
    pub name: Option<String>,
    /// The clickup-task description.
    pub description: Option<String>,
    /// ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.
    #[baml(description = "ClickUp priority: 1 = urgent, 2 = high, 3 = normal, 4 = low.")]
    pub priority: Option<u8>,
    /// Task status string (e.g. \"in progress\"). Used by `UpdateTask`.
    #[baml(description = "Task status string. Used by UpdateTask.")]
    pub status: Option<String>,
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

    fn api_key() -> std::result::Result<String, ClickUpError> {
        std::env::var("CLICKUP_API_KEY").map_err(ClickUpError::MissingApiKey)
    }

    fn require<'a>(value: &'a Option<String>, field: &str, action: &str) -> Result<&'a str> {
        value.as_deref().ok_or_else(|| {
            BamlRtError::InvalidArgument(format!("{field} is required for {action}"))
        })
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
        let json = self
            .send_request(
                self.client
                    .get(format!("{BASE_URL}/team"))
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
        let json = self
            .send_request(
                self.client
                    .get(format!("{BASE_URL}/team/{team_id}/space"))
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
        let json = self
            .send_request(
                self.client
                    .get(format!("{BASE_URL}/space/{space_id}/list"))
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
        let json = self
            .send_request(
                self.client
                    .get(format!("{BASE_URL}/list/{list_id}/task"))
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
        let json = self
            .send_request(
                self.client
                    .get(format!("{BASE_URL}/task/{task_id}"))
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
                    .post(format!("{BASE_URL}/list/{list_id}/task"))
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
                    .put(format!("{BASE_URL}/task/{task_id}"))
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

    #[tracing::instrument(skip(self), fields(action = ?args.action))]
    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = Self::api_key()?;
        match args.action {
            ClickUpAction::ListTeams => self.list_teams(&api_key).await,
            ClickUpAction::ListSpaces => {
                let team_id = Self::require(&args.team_id, "team_id", "ListSpaces")?;
                self.list_spaces(&api_key, team_id).await
            }
            ClickUpAction::ListLists => {
                let space_id = Self::require(&args.space_id, "space_id", "ListLists")?;
                self.list_lists(&api_key, space_id).await
            }
            ClickUpAction::ListTasks => {
                let list_id = Self::require(&args.list_id, "list_id", "ListTasks")?;
                self.list_tasks(&api_key, list_id).await
            }
            ClickUpAction::GetTask => {
                let task_id = Self::require(&args.task_id, "task_id", "GetTask")?;
                self.get_task(&api_key, task_id).await
            }
            ClickUpAction::CreateTask => {
                let list_id = Self::require(&args.list_id, "list_id", "CreateTask")?;
                let name = Self::require(&args.name, "name", "CreateTask")?;
                self.create_task(
                    &api_key,
                    list_id,
                    name,
                    args.description.as_deref(),
                    args.priority,
                )
                .await
            }
            ClickUpAction::UpdateTask => {
                let task_id = Self::require(&args.task_id, "task_id", "UpdateTask")?;
                self.update_task(
                    &api_key,
                    task_id,
                    args.status.as_deref(),
                    args.description.as_deref(),
                    args.priority,
                )
                .await
            }
        }
    }
}

pub struct ClickUpNavigateTool {
    inner: ClickUpTool,
}

impl Default for ClickUpNavigateTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickUpNavigateTool {
    pub fn new() -> Self {
        Self {
            inner: ClickUpTool::new(),
        }
    }
}

#[async_trait]
impl BamlTool for ClickUpNavigateTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "clickupNavigate";
    type OpenInput = ();
    type Input = ClickUpNavigateInput;
    type Output = ClickUpOutput;

    fn description(&self) -> &'static str {
        "ClickUp navigation: list teams, spaces, and lists."
    }

    #[tracing::instrument(skip(self), fields(action = ?args.navigate_action))]
    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = ClickUpTool::api_key()?;
        match args.navigate_action {
            ClickUpNavigateAction::ListTeams => self.inner.list_teams(&api_key).await,
            ClickUpNavigateAction::ListSpaces => {
                let team_id = ClickUpTool::require(&args.team_id, "team_id", "ListSpaces")?;
                self.inner.list_spaces(&api_key, team_id).await
            }
            ClickUpNavigateAction::ListLists => {
                let space_id = ClickUpTool::require(&args.space_id, "space_id", "ListLists")?;
                self.inner.list_lists(&api_key, space_id).await
            }
        }
    }
}

pub struct ClickUpTasksTool {
    inner: ClickUpTool,
}

impl Default for ClickUpTasksTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickUpTasksTool {
    pub fn new() -> Self {
        Self {
            inner: ClickUpTool::new(),
        }
    }
}

#[async_trait]
impl BamlTool for ClickUpTasksTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "clickupTasks";
    type OpenInput = ();
    type Input = ClickUpTasksInput;
    type Output = ClickUpOutput;

    fn description(&self) -> &'static str {
        "ClickUp task reads: list tasks and get task details."
    }

    #[tracing::instrument(skip(self), fields(action = ?args.tasks_action))]
    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = ClickUpTool::api_key()?;
        match args.tasks_action {
            ClickUpTasksAction::ListTasks => {
                let list_id = ClickUpTool::require(&args.list_id, "list_id", "ListTasks")?;
                self.inner.list_tasks(&api_key, list_id).await
            }
            ClickUpTasksAction::GetTask => {
                let task_id = ClickUpTool::require(&args.task_id, "task_id", "GetTask")?;
                self.inner.get_task(&api_key, task_id).await
            }
        }
    }
}

pub struct ClickUpMutateTool {
    inner: ClickUpTool,
}

impl Default for ClickUpMutateTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ClickUpMutateTool {
    pub fn new() -> Self {
        Self {
            inner: ClickUpTool::new(),
        }
    }
}

#[async_trait]
impl BamlTool for ClickUpMutateTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "clickupMutate";
    type OpenInput = ();
    type Input = ClickUpMutateInput;
    type Output = ClickUpOutput;

    fn description(&self) -> &'static str {
        "ClickUp task mutations: create and update tasks."
    }

    #[tracing::instrument(skip(self), fields(action = ?args.mutate_action))]
    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = ClickUpTool::api_key()?;
        match args.mutate_action {
            ClickUpMutateAction::CreateTask => {
                let list_id = ClickUpTool::require(&args.list_id, "list_id", "CreateTask")?;
                let name = ClickUpTool::require(&args.name, "name", "CreateTask")?;
                self.inner
                    .create_task(
                        &api_key,
                        list_id,
                        name,
                        args.description.as_deref(),
                        args.priority,
                    )
                    .await
            }
            ClickUpMutateAction::UpdateTask => {
                let task_id = ClickUpTool::require(&args.task_id, "task_id", "UpdateTask")?;
                self.inner
                    .update_task(
                        &api_key,
                        task_id,
                        args.status.as_deref(),
                        args.description.as_deref(),
                        args.priority,
                    )
                    .await
            }
        }
    }
}

pub fn clickup_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/clickup")
        .expect("support/clickup is a compile-time constant");

    let baml_decl = [
        ClickUpAction::baml_decl(),
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

pub fn clickup_navigate_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/clickupNavigate")
        .expect("support/clickupNavigate is a compile-time constant");

    let baml_decl = [
        ClickUpNavigateAction::baml_decl(),
        ClickUpNavigateInput::baml_decl(),
        ClickUpTaskSummary::baml_decl(),
        ClickUpItem::baml_decl(),
        ClickUpOutput::baml_decl(),
    ]
    .join("\n\n");

    TypeBasedMetadataBuilder::<(), ClickUpNavigateInput, ClickUpOutput>::new(
        name,
        class_name,
        "ClickUp navigation: list teams, spaces, and lists.".to_string(),
    )
    .with_session_plan_group("SupportClickup")
    .with_baml_decl(baml_decl)
    .with_tags(vec![
        "support".to_string(),
        "clickup".to_string(),
        "navigate".to_string(),
    ])
    .with_secrets(vec![ToolSecretRequirement {
        name: "CLICKUP_API_KEY".to_string(),
        description: "ClickUp personal API token (pk_...)".to_string(),
        reason: "Required to authenticate with the ClickUp v2 API".to_string(),
    }])
    .build_metadata()
}

pub fn clickup_tasks_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/clickupTasks")
        .expect("support/clickupTasks is a compile-time constant");

    let baml_decl = [
        ClickUpTasksAction::baml_decl(),
        ClickUpTasksInput::baml_decl(),
    ]
    .join("\n\n");

    TypeBasedMetadataBuilder::<(), ClickUpTasksInput, ClickUpOutput>::new(
        name,
        class_name,
        "ClickUp task reads: list tasks and get task details.".to_string(),
    )
    .with_session_plan_group("SupportClickup")
    .with_baml_decl(baml_decl)
    .with_tags(vec![
        "support".to_string(),
        "clickup".to_string(),
        "tasks".to_string(),
        "read".to_string(),
    ])
    .with_secrets(vec![ToolSecretRequirement {
        name: "CLICKUP_API_KEY".to_string(),
        description: "ClickUp personal API token (pk_...)".to_string(),
        reason: "Required to authenticate with the ClickUp v2 API".to_string(),
    }])
    .build_metadata()
}

pub fn clickup_mutate_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/clickupMutate")
        .expect("support/clickupMutate is a compile-time constant");

    let baml_decl = [
        ClickUpMutateAction::baml_decl(),
        ClickUpMutateInput::baml_decl(),
    ]
    .join("\n\n");

    TypeBasedMetadataBuilder::<(), ClickUpMutateInput, ClickUpOutput>::new(
        name,
        class_name,
        "ClickUp task mutations: create and update tasks.".to_string(),
    )
    .with_session_plan_group("SupportClickup")
    .with_baml_decl(baml_decl)
    .with_tags(vec![
        "support".to_string(),
        "clickup".to_string(),
        "mutate".to_string(),
    ])
    .with_secrets(vec![ToolSecretRequirement {
        name: "CLICKUP_API_KEY".to_string(),
        description: "ClickUp personal API token (pk_...)".to_string(),
        reason: "Required to authenticate with the ClickUp v2 API".to_string(),
    }])
    .build_metadata()
}

register_tool_metadata!(clickup_metadata);
register_tool_metadata!(clickup_navigate_metadata);
register_tool_metadata!(clickup_tasks_metadata);
register_tool_metadata!(clickup_mutate_metadata);

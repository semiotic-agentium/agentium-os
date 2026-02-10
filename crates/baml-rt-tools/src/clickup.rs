//! ClickUp tool — `support/clickup`.
//!
//! Provides a [`BamlTool`] implementation that calls the ClickUp v2 REST API.
//! Supports listing, getting, creating, and updating tasks.

use crate::bundles::Support;
use crate::register_tool_metadata;
use crate::tools::{BamlTool, ToolFunctionMetadata, ToolSecretRequirement};
use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// ClickUp v2 REST API base URL.
pub const BASE_URL: &str = "https://api.clickup.com/api/v2";

// ---------------------------------------------------------------------------
// Newtype wrappers — enforce semantic meaning at the type level
// ---------------------------------------------------------------------------

/// A ClickUp list identifier.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct ListId(pub String);

/// A ClickUp task identifier.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct TaskId(pub String);

/// A human-readable task name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct TaskName(pub String);

/// ClickUp task priority (1 = urgent, 2 = high, 3 = normal, 4 = low).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct Priority(pub u8);

// ---------------------------------------------------------------------------
// Input — enum variants make invalid field combinations unrepresentable
// ---------------------------------------------------------------------------

/// Typed input for the ClickUp tool.
///
/// Each variant carries exactly the fields required for its action;
/// deserialization rejects missing or extraneous fields at the boundary.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub enum ClickUpInput {
    /// List tasks in a ClickUp list (paginated, 100/page).
    ListTasks { list_id: ListId },

    /// Get full details for a single task.
    GetTask { task_id: TaskId },

    /// Create a new task in a list.
    CreateTask {
        list_id: ListId,
        name: TaskName,
        description: Option<String>,
        priority: Option<Priority>,
    },

    /// Update an existing task's status, description, or priority.
    UpdateTask {
        task_id: TaskId,
        status: Option<String>,
        description: Option<String>,
        priority: Option<Priority>,
    },
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Summary of a single ClickUp task, projected from the API response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct ClickUpTaskSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub url: String,
    pub assignees: Vec<String>,
    pub priority: Option<String>,
    pub due_date: Option<String>,
}

/// Output returned by every ClickUp tool action.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct ClickUpOutput {
    pub tasks: Vec<ClickUpTaskSummary>,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Raw deserialization structs — match the ClickUp v2 API shape
// ---------------------------------------------------------------------------

/// Raw task object from the ClickUp v2 API.
///
/// Required fields (`id`, `name`, `status`, `url`) are non-`Option` so that
/// serde fails loudly if the API contract changes, instead of silently
/// producing incomplete summaries.
#[derive(Debug, Deserialize)]
pub(crate) struct RawClickUpTask {
    pub id: String,
    pub name: String,
    pub status: RawStatus,
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

/// Wrapper for the top-level `{ "tasks": [...] }` response shape.
#[derive(Debug, Deserialize)]
pub(crate) struct RawTaskList {
    #[serde(default)]
    pub tasks: Vec<serde_json::Value>,
}

impl From<RawClickUpTask> for ClickUpTaskSummary {
    fn from(raw: RawClickUpTask) -> Self {
        Self {
            id: raw.id,
            name: raw.name,
            status: raw.status.status,
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

// ---------------------------------------------------------------------------
// Error type — preserves the full source chain via #[source]
// ---------------------------------------------------------------------------

/// Errors specific to the ClickUp tool.
///
/// Converted to [`BamlRtError`] at the tool boundary via the [`From`] impl,
/// keeping the `#[source]` chain accessible for tracing and debugging.
#[derive(Debug, thiserror::Error)]
pub enum ClickUpError {
    /// Network-level failure (DNS, timeout, TLS, etc.).
    #[error("ClickUp HTTP request failed")]
    Http(#[source] reqwest::Error),

    /// 401 — bad or expired API key.
    #[error("ClickUp API authentication failed (401): {body}")]
    Unauthorized { body: String },

    /// 404 — the requested resource does not exist.
    #[error("ClickUp resource not found (404): {body}")]
    NotFound { body: String },

    /// 429 — rate limit exceeded.
    #[error("ClickUp rate limit exceeded (429), resets at {reset_at}: {body}")]
    RateLimited { body: String, reset_at: String },

    /// Any other non-2xx status code from the ClickUp API.
    #[error("ClickUp API returned {status}: {body}")]
    Api { status: u16, body: String },

    /// Response body could not be deserialized as JSON.
    #[error("Failed to deserialize ClickUp response")]
    Deserialize(#[source] reqwest::Error),

    /// `CLICKUP_API_KEY` environment variable is not set.
    #[error("CLICKUP_API_KEY environment variable not set")]
    MissingApiKey(#[source] std::env::VarError),
}

// ---------------------------------------------------------------------------
// Boundary conversion — ClickUpError → BamlRtError
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tool struct and helpers
// ---------------------------------------------------------------------------

/// ClickUp tool — executes actions against the ClickUp v2 REST API.
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

    /// Read the ClickUp personal API token from the environment.
    fn api_key() -> std::result::Result<String, ClickUpError> {
        std::env::var("CLICKUP_API_KEY").map_err(ClickUpError::MissingApiKey)
    }

    /// Send an HTTP request to ClickUp and map non-2xx responses to
    /// [`ClickUpError`] variants.
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
            return Err(match code {
                401 => ClickUpError::Unauthorized { body },
                404 => ClickUpError::NotFound { body },
                429 => ClickUpError::RateLimited { body, reset_at },
                _ => ClickUpError::Api { status: code, body },
            });
        }

        resp.json().await.map_err(ClickUpError::Deserialize)
    }

    // -- Action methods ----------------------------------------------------

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

        let raw: RawClickUpTask =
            serde_json::from_value(json).map_err(|e| ClickUpError::Api {
                status: 0,
                body: format!("unexpected task shape: {e}"),
            })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Task {task_id}: {}", summary.name),
            tasks: vec![summary],
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn create_task(
        &self,
        api_key: &str,
        list_id: &str,
        name: &str,
        description: Option<&str>,
        priority: Option<Priority>,
    ) -> Result<ClickUpOutput> {
        let mut body = serde_json::json!({ "name": name });
        if let Some(desc) = description {
            body["description"] = serde_json::Value::String(desc.to_string());
        }
        if let Some(p) = priority {
            body["priority"] = serde_json::json!(p.0);
        }

        let json = self
            .send_request(
                self.client
                    .post(format!("{BASE_URL}/list/{list_id}/task"))
                    .header("Authorization", api_key)
                    .json(&body),
            )
            .await?;

        let raw: RawClickUpTask =
            serde_json::from_value(json).map_err(|e| ClickUpError::Api {
                status: 0,
                body: format!("unexpected task shape: {e}"),
            })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Created task '{}' in list {list_id}", summary.name),
            tasks: vec![summary],
        })
    }

    #[tracing::instrument(skip(self, api_key))]
    async fn update_task(
        &self,
        api_key: &str,
        task_id: &str,
        status: Option<&str>,
        description: Option<&str>,
        priority: Option<Priority>,
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
            body.insert("priority".to_string(), serde_json::json!(p.0));
        }

        let json = self
            .send_request(
                self.client
                    .put(format!("{BASE_URL}/task/{task_id}"))
                    .header("Authorization", api_key)
                    .json(&serde_json::Value::Object(body)),
            )
            .await?;

        let raw: RawClickUpTask =
            serde_json::from_value(json).map_err(|e| ClickUpError::Api {
                status: 0,
                body: format!("unexpected task shape: {e}"),
            })?;

        let summary = ClickUpTaskSummary::from(raw);
        Ok(ClickUpOutput {
            message: format!("Updated task {task_id}"),
            tasks: vec![summary],
        })
    }
}

// ---------------------------------------------------------------------------
// BamlTool implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl BamlTool for ClickUpTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "clickup";
    type OpenInput = ();
    type Input = ClickUpInput;
    type Output = ClickUpOutput;

    fn description(&self) -> &'static str {
        "Interact with ClickUp: list, get, create, and update tasks."
    }

    #[tracing::instrument(skip(self), fields(action))]
    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let api_key = Self::api_key()?;
        match args {
            ClickUpInput::ListTasks { list_id } => {
                self.list_tasks(&api_key, &list_id.0).await
            }
            ClickUpInput::GetTask { task_id } => self.get_task(&api_key, &task_id.0).await,
            ClickUpInput::CreateTask {
                list_id,
                name,
                description,
                priority,
            } => {
                self.create_task(&api_key, &list_id.0, &name.0, description.as_deref(), priority)
                    .await
            }
            ClickUpInput::UpdateTask {
                task_id,
                status,
                description,
                priority,
            } => {
                self.update_task(
                    &api_key,
                    &task_id.0,
                    status.as_deref(),
                    description.as_deref(),
                    priority,
                )
                .await
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Metadata registration (compile-time, for codegen)
// ---------------------------------------------------------------------------

pub fn clickup_metadata() -> ToolFunctionMetadata {
    use crate::{ToolMetadataBuilder, TypeBasedMetadataBuilder, parse_tool_name_and_class};
    let (name, class_name) = parse_tool_name_and_class("support/clickup")
        .expect("support/clickup is a compile-time constant");
    TypeBasedMetadataBuilder::<(), ClickUpInput, ClickUpOutput>::new(
        name,
        class_name,
        "Interact with ClickUp: list, get, create, and update tasks.".to_string(),
    )
    .with_tags(vec!["support".to_string(), "clickup".to_string()])
    .with_secrets(vec![ToolSecretRequirement {
        name: "CLICKUP_API_KEY".to_string(),
        description: "ClickUp personal API token (pk_...)".to_string(),
        reason: "Required to authenticate with the ClickUp v2 API".to_string(),
    }])
    .build_metadata()
}

register_tool_metadata!(clickup_metadata);

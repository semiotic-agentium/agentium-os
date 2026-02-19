//! Unified task store backed by the same GraphQLite DB as provenance.
//!
//! Persists tasks, messages, and update queue in SQL tables in the same SQLite
//! file; conversation context is read from the provenance graph.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Result,
    ids::{ContextId, ExternalId, TaskId},
};
use baml_rt_provenance::{
    GraphqliteProvenanceStore, ProvenanceContextReader, ProvenanceConversationContextItem,
};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::{
    a2a_store::{
        ConversationContextSource, TaskChunkApplier, TaskEventRecorder, TaskRepository,
        TaskUpdateEvent, TaskUpdateQueue,
    },
    a2a_types::{
        Artifact, ListTasksRequest, ListTasksResponse, Message, Task, TaskArtifactUpdateEvent,
        TaskState, TaskStatus, TaskStatusUpdateEvent,
    },
};

/// SQL schema: tasks table (id, context_id, status_json, metadata_json, extra_json, artifacts_json, ord).
const SCHEMA_TASKS: &str = r#"
CREATE TABLE IF NOT EXISTS a2a_tasks (
    id TEXT PRIMARY KEY,
    context_id TEXT NOT NULL,
    status_json TEXT,
    metadata_json TEXT,
    extra_json TEXT,
    artifacts_json TEXT,
    ord INTEGER NOT NULL DEFAULT 0
)"#;

/// SQL schema: task_messages (task_id, seq, message_json).
const SCHEMA_TASK_MESSAGES: &str = r#"
CREATE TABLE IF NOT EXISTS a2a_task_messages (
    task_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    message_json TEXT NOT NULL,
    PRIMARY KEY (task_id, seq),
    FOREIGN KEY (task_id) REFERENCES a2a_tasks(id)
)"#;

/// SQL schema: task_updates (task_id, seq, kind, payload_json).
const SCHEMA_TASK_UPDATES: &str = r#"
CREATE TABLE IF NOT EXISTS a2a_task_updates (
    task_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (task_id, seq),
    FOREIGN KEY (task_id) REFERENCES a2a_tasks(id)
)"#;

// FSM: same as a2a_store (task lifecycle).
const S_SUBMITTED: &str = "TASK_STATE_SUBMITTED";
const S_WORKING: &str = "TASK_STATE_WORKING";
const S_COMPLETED: &str = "TASK_STATE_COMPLETED";
const S_FAILED: &str = "TASK_STATE_FAILED";
const S_CANCELED: &str = "TASK_STATE_CANCELED";
const S_REJECTED: &str = "TASK_STATE_REJECTED";
const S_INPUT_REQUIRED: &str = "TASK_STATE_INPUT_REQUIRED";
const S_AUTH_REQUIRED: &str = "TASK_STATE_AUTH_REQUIRED";

fn is_terminal_state(s: &str) -> bool {
    matches!(s, S_COMPLETED | S_FAILED | S_CANCELED | S_REJECTED)
}

fn is_allowed_transition(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    if is_terminal_state(from) {
        return false;
    }
    matches!(
        (from, to),
        (S_SUBMITTED, S_WORKING)
            | (S_SUBMITTED, S_COMPLETED)
            | (S_SUBMITTED, S_FAILED)
            | (S_SUBMITTED, S_CANCELED)
            | (S_SUBMITTED, S_REJECTED)
            | (S_SUBMITTED, S_INPUT_REQUIRED)
            | (S_SUBMITTED, S_AUTH_REQUIRED)
            | (S_WORKING, S_INPUT_REQUIRED)
            | (S_WORKING, S_AUTH_REQUIRED)
            | (S_WORKING, S_COMPLETED)
            | (S_WORKING, S_FAILED)
            | (S_WORKING, S_CANCELED)
            | (S_WORKING, S_REJECTED)
            | (S_INPUT_REQUIRED, S_WORKING)
            | (S_INPUT_REQUIRED, S_CANCELED)
            | (S_INPUT_REQUIRED, S_REJECTED)
            | (S_INPUT_REQUIRED, S_COMPLETED)
            | (S_INPUT_REQUIRED, S_FAILED)
            | (S_AUTH_REQUIRED, S_WORKING)
            | (S_AUTH_REQUIRED, S_CANCELED)
            | (S_AUTH_REQUIRED, S_REJECTED)
            | (S_AUTH_REQUIRED, S_COMPLETED)
            | (S_AUTH_REQUIRED, S_FAILED)
    )
}

fn status_to_string(status: &TaskStatus) -> Option<String> {
    status.state.as_ref().map(|state| match state {
        TaskState::String(value) => value.clone(),
        TaskState::Integer(value) => value.to_string(),
    })
}

/// Unified store: task state in SQL tables in the same DB as GraphQLite provenance.
/// Implements [TaskStoreBackend]; use the same [Arc] as provenance writer for a single persistence config.
pub struct GraphqliteUnifiedStore {
    store: Arc<GraphqliteProvenanceStore>,
    schema_initialized: OnceCell<()>,
}

impl GraphqliteUnifiedStore {
    pub fn new(store: Arc<GraphqliteProvenanceStore>) -> Self {
        Self {
            store,
            schema_initialized: OnceCell::new(),
        }
    }

    async fn ensure_schema(&self) -> Result<()> {
        self.schema_initialized
            .get_or_init(|| async {
                self.store
                    .run_sql_execute(SCHEMA_TASKS, &[])
                    .await
                    .expect("create a2a_tasks");
                self.store
                    .run_sql_execute(SCHEMA_TASK_MESSAGES, &[])
                    .await
                    .expect("create a2a_task_messages");
                self.store
                    .run_sql_execute(SCHEMA_TASK_UPDATES, &[])
                    .await
                    .expect("create a2a_task_updates");
            })
            .await;
        Ok(())
    }

    fn task_to_row(task: &Task) -> Result<(String, String, String, String, String, String, i64)> {
        let id = task
            .id
            .as_ref()
            .map(|id| id.as_str().to_string())
            .ok_or_else(|| BamlRtError::InvalidArgument("task.id required".into()))?;
        let context_id = task
            .context_id
            .as_ref()
            .map(|c| c.as_str().to_string())
            .unwrap_or_default();
        let status_json = task
            .status
            .as_ref()
            .map(|s| serde_json::to_string(s).unwrap_or_default())
            .unwrap_or_default();
        let metadata_json = task
            .metadata
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_default())
            .unwrap_or_default();
        let extra_json = serde_json::to_string(&task.extra).unwrap_or_default();
        let artifacts_json = serde_json::to_string(&task.artifacts).unwrap_or_default();
        let ord = 0_i64;
        Ok((
            id,
            context_id,
            status_json,
            metadata_json,
            extra_json,
            artifacts_json,
            ord,
        ))
    }

    fn row_to_task(row: &HashMap<String, Value>, history: Vec<Message>) -> Option<Task> {
        let id = row.get("id")?.as_str()?;
        let context_id = row.get("context_id").and_then(|v| v.as_str());
        let status_json = row
            .get("status_json")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let metadata_json = row
            .get("metadata_json")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let extra_json = row
            .get("extra_json")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        let artifacts_json = row
            .get("artifacts_json")
            .and_then(|v| v.as_str())
            .unwrap_or("[]");
        let status = if status_json.is_empty() {
            None
        } else {
            serde_json::from_str(status_json).ok()
        };
        let metadata = serde_json::from_str(metadata_json).ok();
        let extra = serde_json::from_str(extra_json).unwrap_or_default();
        let artifacts = serde_json::from_str(artifacts_json).unwrap_or_default();
        Some(Task {
            id: Some(TaskId::from_external(ExternalId::new(id))),
            context_id: context_id.and_then(ContextId::parse_temporal),
            artifacts,
            history,
            status,
            metadata,
            extra,
        })
    }
}

#[async_trait]
impl TaskRepository for GraphqliteUnifiedStore {
    async fn upsert(&self, task: Task) -> Result<Option<Task>> {
        self.ensure_schema().await?;
        let (id, context_id, status_json, metadata_json, extra_json, artifacts_json, _ord) =
            Self::task_to_row(&task)?;
        let existing = self
            .store
            .run_sql_query(
                "SELECT status_json FROM a2a_tasks WHERE id = ?1",
                &[Value::String(id.clone())],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let preserve_status = existing
            .first()
            .and_then(|r| r.get("status_json"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let status_json = preserve_status.map(String::from).unwrap_or(status_json);
        let next_ord = if existing.is_empty() {
            let max: Vec<HashMap<String, Value>> = self
                .store
                .run_sql_query("SELECT COALESCE(MAX(ord),0) as m FROM a2a_tasks", &[])
                .await
                .map_err(|e| BamlRtError::ProvenanceContextRead {
                    source: Box::new(e),
                })?;
            let m = max
                .first()
                .and_then(|r| r.get("m"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            m + 1
        } else {
            0
        };
        self.store
            .run_sql_execute(
                r#"
                INSERT INTO a2a_tasks (id, context_id, status_json, metadata_json, extra_json, artifacts_json, ord)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    context_id = excluded.context_id,
                    status_json = excluded.status_json,
                    metadata_json = excluded.metadata_json,
                    extra_json = excluded.extra_json,
                    artifacts_json = excluded.artifacts_json
                "#,
                &[
                    Value::String(id.clone()),
                    Value::String(context_id),
                    Value::String(status_json),
                    Value::String(metadata_json),
                    Value::String(extra_json),
                    Value::String(artifacts_json),
                    Value::Number(serde_json::Number::from(next_ord)),
                ],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let mut out = task;
        out.status = preserve_status.and_then(|s| serde_json::from_str(s).ok());
        Ok(Some(out))
    }

    async fn ensure_task_exists(
        &self,
        task_id: &TaskId,
        context_id: Option<&ContextId>,
    ) -> Result<()> {
        self.ensure_schema().await?;
        let context_id = context_id
            .map(|c| c.as_str().to_string())
            .unwrap_or_default();
        self.store
            .run_sql_execute(
                "INSERT OR IGNORE INTO a2a_tasks \
                 (id, context_id, status_json, metadata_json, extra_json, artifacts_json, ord) \
                 VALUES (?1, ?2, '', '{}', '{}', '[]', \
                         (SELECT COALESCE(MAX(ord),0)+1 FROM a2a_tasks))",
                &[
                    Value::String(task_id.as_str().to_string()),
                    Value::String(context_id),
                ],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        Ok(())
    }

    async fn get(&self, id: &str, history_length: Option<usize>) -> Option<Task> {
        let _ = self.ensure_schema().await.ok()?;
        let rows = self
            .store
            .run_sql_query(
                "SELECT * FROM a2a_tasks WHERE id = ?1",
                &[Value::String(id.to_string())],
            )
            .await
            .ok()?;
        let row = rows.into_iter().next()?;
        let msg_rows = self
            .store
            .run_sql_query(
                "SELECT message_json FROM a2a_task_messages WHERE task_id = ?1 ORDER BY seq",
                &[Value::String(id.to_string())],
            )
            .await
            .ok()?;
        let mut history: Vec<Message> = msg_rows
            .into_iter()
            .filter_map(|r| {
                r.get("message_json")
                    .and_then(|v| v.as_str().map(String::from))
            })
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        if let Some(limit) = history_length {
            if limit == 0 {
                history.clear();
            } else if history.len() > limit {
                let start = history.len() - limit;
                history = history.into_iter().skip(start).collect();
            }
        }
        Self::row_to_task(&row, history)
    }

    async fn list(&self, request: &ListTasksRequest) -> ListTasksResponse {
        let _ = match self.ensure_schema().await {
            Ok(()) => (),
            Err(_) => {
                return ListTasksResponse {
                    tasks: vec![],
                    next_page_token: None,
                    total_size: Some(0),
                    page_size: None,
                    extra: HashMap::new(),
                };
            }
        };
        let mut sql = "SELECT * FROM a2a_tasks".to_string();
        let mut params: Vec<Value> = vec![];
        if let Some(ref cid) = request.context_id {
            sql += " WHERE context_id = ?1";
            params.push(Value::String(cid.as_str().to_string()));
        }
        sql += " ORDER BY ord";
        let rows = match self.store.run_sql_query(&sql, &params).await {
            Ok(r) => r,
            Err(_) => {
                return ListTasksResponse {
                    tasks: vec![],
                    next_page_token: None,
                    total_size: Some(0),
                    page_size: None,
                    extra: HashMap::new(),
                };
            }
        };
        let mut tasks: Vec<Task> = Vec::new();
        for row in rows {
            let Some(id) = row.get("id").and_then(|v| v.as_str()).map(String::from) else {
                continue;
            };
            let msg_rows = match self
                .store
                .run_sql_query(
                    "SELECT message_json FROM a2a_task_messages WHERE task_id = ?1 ORDER BY seq",
                    &[Value::String(id)],
                )
                .await
            {
                Ok(r) => r,
                Err(_) => continue,
            };
            let history: Vec<Message> = msg_rows
                .into_iter()
                .filter_map(|r| {
                    r.get("message_json")
                        .and_then(|v| v.as_str().map(String::from))
                })
                .filter_map(|s| serde_json::from_str(&s).ok())
                .collect();
            if let Some(task) = Self::row_to_task(&row, history) {
                tasks.push(task);
            }
        }
        if let Some(status) = &request.status {
            tasks.retain(|task| {
                task.status
                    .as_ref()
                    .and_then(|s| s.state.as_ref())
                    .map(|s| match (s, status) {
                        (TaskState::String(a), TaskState::String(b)) => a == b,
                        (TaskState::Integer(a), TaskState::Integer(b)) => a == b,
                        _ => false,
                    })
                    .unwrap_or(false)
            });
        }
        let include_artifacts = request.include_artifacts.unwrap_or(false);
        if !include_artifacts {
            for task in &mut tasks {
                task.artifacts.clear();
            }
        }
        let history_limit = request.history_length.as_ref().and_then(|v| v.as_usize());
        if let Some(limit) = history_limit {
            for task in &mut tasks {
                if limit == 0 {
                    task.history.clear();
                } else if task.history.len() > limit {
                    let start = task.history.len() - limit;
                    task.history = task.history.split_off(start);
                }
            }
        }
        let total_size = tasks.len() as u64;
        let page_size = request
            .page_size
            .as_ref()
            .and_then(|v| v.as_usize())
            .unwrap_or(50);
        let start = request
            .page_token
            .as_ref()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let end = std::cmp::min(start + page_size, tasks.len());
        let page_tasks = if start < tasks.len() {
            tasks[start..end].to_vec()
        } else {
            vec![]
        };
        let next_page_token = if end < tasks.len() {
            Some(end.to_string())
        } else {
            None
        };
        ListTasksResponse {
            tasks: page_tasks,
            next_page_token,
            total_size: Some(total_size),
            page_size: Some(page_size as u64),
            extra: HashMap::new(),
        }
    }

    async fn cancel(&self, id: &str) -> Option<Task> {
        let task = self.get(id, None).await?;
        let task_id = task.id.clone()?;
        let context_id = task.context_id.clone();
        let status = TaskStatus {
            state: Some(TaskState::String("TASK_STATE_CANCELED".to_string())),
            message: None,
            timestamp: None,
            extra: HashMap::new(),
        };
        let _ = self
            .record_status_update(Some(task_id), context_id, status)
            .await;
        self.get(id, None).await
    }

    async fn insert_message(&self, message: &Message) -> Result<()> {
        self.ensure_schema().await?;
        let task_id = message.task_id.as_ref().map(|t| t.as_str().to_string());
        let Some(task_id) = task_id else {
            return Ok(());
        };
        let seq_rows = self
            .store
            .run_sql_query(
                "SELECT COALESCE(MAX(seq),0) as m FROM a2a_task_messages WHERE task_id = ?1",
                &[Value::String(task_id.clone())],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let seq = seq_rows
            .first()
            .and_then(|r| r.get("m"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            + 1;
        let message_json = serde_json::to_string(message).unwrap_or_default();
        self.store
            .run_sql_execute(
                "INSERT INTO a2a_task_messages (task_id, seq, message_json) VALUES (?1, ?2, ?3)",
                &[
                    Value::String(task_id),
                    Value::Number(serde_json::Number::from(seq)),
                    Value::String(message_json),
                ],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        Ok(())
    }
}

#[async_trait]
impl TaskEventRecorder for GraphqliteUnifiedStore {
    async fn record_status_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        status: TaskStatus,
    ) -> Result<Option<TaskUpdateEvent>> {
        let task_id = match task_id {
            Some(t) => t,
            None => return Ok(None),
        };
        let new_state = match status_to_string(&status) {
            Some(s) => s,
            None => return Ok(None),
        };
        let _ = self.ensure_schema().await?;
        let rows = self
            .store
            .run_sql_query(
                "SELECT status_json FROM a2a_tasks WHERE id = ?1",
                &[Value::String(task_id.as_str().to_string())],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let current_state_str = rows
            .first()
            .and_then(|r| r.get("status_json"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let current_state = current_state_str;
        let allowed = match current_state {
            None => new_state == S_SUBMITTED,
            Some(current) if is_terminal_state(current) => false,
            Some(current) => is_allowed_transition(current, &new_state),
        };
        if !allowed {
            return Ok(None);
        }
        let status_json = serde_json::to_string(&status).unwrap_or_default();
        self.store
            .run_sql_execute(
                "UPDATE a2a_tasks SET status_json = ?1 WHERE id = ?2",
                &[
                    Value::String(status_json),
                    Value::String(task_id.as_str().to_string()),
                ],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let seq_rows = self
            .store
            .run_sql_query(
                "SELECT COALESCE(MAX(seq),0) as m FROM a2a_task_updates WHERE task_id = ?1",
                &[Value::String(task_id.as_str().to_string())],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let seq = seq_rows
            .first()
            .and_then(|r| r.get("m"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            + 1;
        let payload = serde_json::json!({
            "context_id": context_id.as_ref().map(|c| c.as_str()),
            "task_id": task_id.as_str(),
            "status": status
        });
        self.store
            .run_sql_execute(
                "INSERT INTO a2a_task_updates (task_id, seq, kind, payload_json) VALUES (?1, ?2, ?3, ?4)",
                &[
                    Value::String(task_id.as_str().to_string()),
                    Value::Number(serde_json::Number::from(seq)),
                    Value::String("status".to_string()),
                    Value::String(serde_json::to_string(&payload).unwrap_or_default()),
                ],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let update = TaskStatusUpdateEvent {
            context_id,
            task_id: Some(task_id),
            status: Some(status),
            metadata: None,
            extra: HashMap::new(),
        };
        Ok(Some(TaskUpdateEvent::Status(update)))
    }

    async fn record_artifact_update(
        &self,
        task_id: Option<TaskId>,
        context_id: Option<ContextId>,
        artifact: Artifact,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> Result<Option<TaskUpdateEvent>> {
        let task_id = match task_id {
            Some(t) => t,
            None => return Ok(None),
        };
        self.ensure_schema().await?;
        let seq_rows = self
            .store
            .run_sql_query(
                "SELECT COALESCE(MAX(seq),0) as m FROM a2a_task_updates WHERE task_id = ?1",
                &[Value::String(task_id.as_str().to_string())],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let seq = seq_rows
            .first()
            .and_then(|r| r.get("m"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            + 1;
        let payload = serde_json::json!({
            "context_id": context_id.as_ref().map(|c| c.as_str()),
            "task_id": task_id.as_str(),
            "last_chunk": last_chunk,
            "append": append,
            "artifact": artifact
        });
        self.store
            .run_sql_execute(
                "INSERT INTO a2a_task_updates (task_id, seq, kind, payload_json) VALUES (?1, ?2, ?3, ?4)",
                &[
                    Value::String(task_id.as_str().to_string()),
                    Value::Number(serde_json::Number::from(seq)),
                    Value::String("artifact".to_string()),
                    Value::String(serde_json::to_string(&payload).unwrap_or_default()),
                ],
            )
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let update = TaskArtifactUpdateEvent {
            context_id,
            task_id: Some(task_id),
            last_chunk,
            append,
            artifact: Some(artifact),
            metadata: None,
            extra: HashMap::new(),
        };
        Ok(Some(TaskUpdateEvent::Artifact(update)))
    }
}

#[async_trait]
impl TaskUpdateQueue for GraphqliteUnifiedStore {
    async fn drain_updates(&self, task_id: &str) -> Vec<TaskUpdateEvent> {
        let _ = self.ensure_schema().await.ok();
        let rows = self
            .store
            .run_sql_query(
                "SELECT kind, payload_json FROM a2a_task_updates WHERE task_id = ?1 ORDER BY seq",
                &[Value::String(task_id.to_string())],
            )
            .await
            .ok()
            .unwrap_or_default();
        let events: Vec<TaskUpdateEvent> = rows
            .into_iter()
            .filter_map(|row| {
                let kind = row.get("kind")?.as_str()?;
                let payload_str = row.get("payload_json")?.as_str()?;
                match kind {
                    "status" => serde_json::from_str::<TaskStatusUpdateEvent>(payload_str)
                        .ok()
                        .map(TaskUpdateEvent::Status),
                    "artifact" => serde_json::from_str::<TaskArtifactUpdateEvent>(payload_str)
                        .ok()
                        .map(TaskUpdateEvent::Artifact),
                    _ => None,
                }
            })
            .collect();
        if !events.is_empty() {
            let _ = self
                .store
                .run_sql_execute(
                    "DELETE FROM a2a_task_updates WHERE task_id = ?1",
                    &[Value::String(task_id.to_string())],
                )
                .await;
        }
        events
    }
}

#[async_trait]
impl TaskChunkApplier for GraphqliteUnifiedStore {
    async fn apply_task_delta(
        &self,
        task: Option<Task>,
        message: Option<Message>,
        status_update: Option<TaskStatusUpdateEvent>,
        artifact_update: Option<TaskArtifactUpdateEvent>,
    ) -> Result<Vec<TaskUpdateEvent>> {
        if task.is_none() && (status_update.is_some() || artifact_update.is_some()) {
            return Err(BamlRtError::InvalidArgument(
                "status_update or artifact_update requires task in chunk".into(),
            ));
        }
        let mut out = Vec::new();
        if let Some(mut t) = task {
            let status = t.status.take();
            let context_id = t.context_id.clone();
            let task_id = t.id.clone();
            let artifacts = std::mem::take(&mut t.artifacts);
            let _ = self.upsert(t).await?;
            if let Some(status) = status
                && let Some(tid) = &task_id
                && let Some(ev) = self
                    .record_status_update(Some(tid.clone()), context_id.clone(), status)
                    .await?
            {
                out.push(ev);
            }
            if let Some(tid) = task_id {
                for artifact in artifacts {
                    if let Some(ev) = self
                        .record_artifact_update(
                            Some(tid.clone()),
                            None,
                            artifact,
                            Some(false),
                            Some(true),
                        )
                        .await?
                    {
                        out.push(ev);
                    }
                }
            }
        }
        if let Some(msg) = message {
            self.insert_message(&msg).await?;
        }
        if let Some(ref up) = status_update
            && let Some(status) = up.status.clone()
            && let Some(ev) = self
                .record_status_update(up.task_id.clone(), up.context_id.clone(), status)
                .await?
        {
            out.push(ev);
        }
        if let Some(ref up) = artifact_update
            && let Some(ev) = self
                .record_artifact_update(
                    up.task_id.clone(),
                    up.context_id.clone(),
                    up.artifact.clone().unwrap_or_default(),
                    up.append,
                    up.last_chunk,
                )
                .await?
        {
            out.push(ev);
        }
        Ok(out)
    }
}

#[async_trait]
impl ConversationContextSource for GraphqliteUnifiedStore {
    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.store
            .conversation_context(context_id, limit)
            .await
            .map_err(|e| BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })
    }
}

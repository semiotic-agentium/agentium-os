//! Strongly-typed recording of status and artifact updates into the A2A task subgraph.
//! The boundary is here: required task_id, distinct context variants (not Option).
//! All IDs are sourced from the ID layer (baml-rt-core re-exporting baml-rt-id semantics).

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::ids::{ContextId, TaskId};
use baml_rt_vocabulary::{A2aGraphStore, A2aGraphStoreResult};
use serde_json::Value;

/// Context for a status update: scoped (task + context) or task-only. Distinct semantics.
#[derive(Debug, Clone)]
pub enum StatusUpdateContext {
    Scoped { context_id: ContextId },
    TaskOnly,
}

/// Context for an artifact update: scoped or task-only.
#[derive(Debug, Clone)]
pub enum ArtifactUpdateContext {
    Scoped { context_id: ContextId },
    TaskOnly,
}

/// Records status and artifact updates into the graph. Strongly typed: task_id and context
/// use ID types from the ID layer ([TaskId], [ContextId]); payloads are JSON strings.
#[async_trait]
pub trait A2aGraphEventRecorder: Send + Sync {
    async fn record_status_update(
        &self,
        task_id: &TaskId,
        context: &StatusUpdateContext,
        status_json: &str,
    ) -> A2aGraphStoreResult<()>;

    async fn record_artifact_update(
        &self,
        task_id: &TaskId,
        context: &ArtifactUpdateContext,
        artifact_json: &str,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> A2aGraphStoreResult<()>;
}

#[async_trait]
impl<T: A2aGraphStore + Send + Sync> A2aGraphEventRecorder for Arc<T> {
    async fn record_status_update(
        &self,
        task_id: &TaskId,
        context: &StatusUpdateContext,
        status_json: &str,
    ) -> A2aGraphStoreResult<()> {
        record_status_update(self.as_ref(), task_id, context, status_json).await
    }

    async fn record_artifact_update(
        &self,
        task_id: &TaskId,
        context: &ArtifactUpdateContext,
        artifact_json: &str,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> A2aGraphStoreResult<()> {
        record_artifact_update(
            self.as_ref(),
            task_id,
            context,
            artifact_json,
            append,
            last_chunk,
        )
        .await
    }
}

#[async_trait]
impl A2aGraphEventRecorder for Arc<dyn A2aGraphStore + Send + Sync + 'static> {
    async fn record_status_update(
        &self,
        task_id: &TaskId,
        context: &StatusUpdateContext,
        status_json: &str,
    ) -> A2aGraphStoreResult<()> {
        record_status_update(self.as_ref(), task_id, context, status_json).await
    }

    async fn record_artifact_update(
        &self,
        task_id: &TaskId,
        context: &ArtifactUpdateContext,
        artifact_json: &str,
        append: Option<bool>,
        last_chunk: Option<bool>,
    ) -> A2aGraphStoreResult<()> {
        record_artifact_update(
            self.as_ref(),
            task_id,
            context,
            artifact_json,
            append,
            last_chunk,
        )
        .await
    }
}

/// Writes a status update to the graph. ID types from the ID layer ([TaskId], [ContextId]).
pub async fn record_status_update(
    graph: &dyn A2aGraphStore,
    task_id: &TaskId,
    context: &StatusUpdateContext,
    status_json: &str,
) -> A2aGraphStoreResult<()> {
    let task_id_str = task_id.as_str();
    graph.set_task_status_json(task_id_str, status_json).await?;
    let seq = graph.max_update_seq(task_id_str).await? + 1;
    let context_id_str = match context {
        StatusUpdateContext::Scoped { context_id } => context_id.as_str(),
        StatusUpdateContext::TaskOnly => "",
    };
    let payload_json = serde_json::to_string(&serde_json::json!({
        "context_id": context_id_str,
        "task_id": task_id_str,
        "status": serde_json::from_str::<Value>(status_json).unwrap_or(Value::Null)
    }))
    .map_err(|e| e.to_string())?;
    let node_id = format!("{task_id_str}:update:{seq}");
    graph
        .insert_update_node(&node_id, task_id_str, seq, "status", &payload_json)
        .await
}

/// Writes an artifact update to the graph. ID types from the ID layer ([TaskId], [ContextId]).
pub async fn record_artifact_update(
    graph: &dyn A2aGraphStore,
    task_id: &TaskId,
    context: &ArtifactUpdateContext,
    artifact_json: &str,
    append: Option<bool>,
    last_chunk: Option<bool>,
) -> A2aGraphStoreResult<()> {
    let task_id_str = task_id.as_str();
    let context_id_str = match context {
        ArtifactUpdateContext::Scoped { context_id } => context_id.as_str(),
        ArtifactUpdateContext::TaskOnly => "",
    };
    let seq = graph.max_update_seq(task_id_str).await? + 1;
    let payload_json = serde_json::to_string(&serde_json::json!({
        "context_id": context_id_str,
        "task_id": task_id_str,
        "last_chunk": last_chunk,
        "append": append,
        "artifact": serde_json::from_str::<Value>(artifact_json).unwrap_or(Value::Null)
    }))
    .map_err(|e| e.to_string())?;
    let node_id = format!("{task_id_str}:update:{seq}");
    graph
        .insert_update_node(&node_id, task_id_str, seq, "artifact", &payload_json)
        .await
}

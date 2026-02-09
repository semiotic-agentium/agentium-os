use crate::a2a_store::TaskStoreBackend;
use crate::a2a_types::{
    Message, SendMessageResponse, StreamResponse, Task, TaskArtifactUpdateEvent,
    TaskStatusUpdateEvent,
};
use crate::events::EventEmitter;
use baml_rt_core::Result;
use std::sync::Arc;

pub struct TaskProcessor {
    task_store: Arc<dyn TaskStoreBackend>,
    emitter: Arc<dyn EventEmitter>,
}

impl TaskProcessor {
    pub fn new(task_store: Arc<dyn TaskStoreBackend>, emitter: Arc<dyn EventEmitter>) -> Self {
        Self {
            task_store,
            emitter,
        }
    }

    pub async fn process_stream_response(&self, stream: StreamResponse) -> Result<()> {
        self.process(
            stream.task,
            stream.message,
            stream.status_update,
            stream.artifact_update,
        )
        .await
    }

    pub async fn process_send_message_response(&self, response: SendMessageResponse) -> Result<()> {
        self.process(response.task, response.message, None, None)
            .await
    }

    pub async fn process_task(&self, task: Task) -> Result<()> {
        self.process(Some(task), None, None, None).await
    }

    async fn process(
        &self,
        task: Option<Task>,
        message: Option<Message>,
        status_update: Option<TaskStatusUpdateEvent>,
        artifact_update: Option<TaskArtifactUpdateEvent>,
    ) -> Result<()> {
        // Extract agent_id from message metadata for injection into tasks if needed
        let agent_id_from_message = message
            .as_ref()
            .and_then(|msg| msg.metadata.as_ref())
            .and_then(|meta| meta.get("agent_id"))
            .cloned();

        // Extract agent_id from task metadata (after potential injection) for messages
        let mut task_agent_id_opt = None;

        if let Some(mut task) = task {
            // Inject agent_id into task metadata if missing (from request metadata)
            // This ensures tasks created from JS responses have agent_id for provenance
            if !task
                .metadata
                .as_ref()
                .is_some_and(|m| m.contains_key("agent_id"))
                && let Some(agent_id_value) = agent_id_from_message.clone()
            {
                let mut metadata = task.metadata.unwrap_or_default();
                metadata.insert("agent_id".to_string(), agent_id_value);
                task.metadata = Some(metadata);
            }

            // Extract agent_id from task for later use with messages
            task_agent_id_opt = task
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("agent_id"))
                .cloned();

            let status = task.status.clone();
            let context_id = task.context_id.clone();
            let task_id = task.id.clone();
            let artifacts = task.artifacts.clone();
            self.task_store.upsert(task).await;
            if let Some(status) = status
                && let Some(event) = self
                    .task_store
                    .record_status_update(task_id.clone(), context_id.clone(), status)
                    .await
            {
                self.emitter.emit(event).await;
            }
            if let Some(task_id) = task_id {
                for artifact in artifacts {
                    if let Some(event) = self
                        .task_store
                        .record_artifact_update(
                            Some(task_id.clone()),
                            context_id.clone(),
                            artifact,
                            Some(false),
                            Some(true),
                        )
                        .await
                    {
                        self.emitter.emit(event).await;
                    }
                }
            }
        }
        if let Some(mut msg) = message {
            // Ensure message has agent_id in metadata for provenance
            // If missing, try to get it from task metadata (from above) or use message's own
            if !msg
                .metadata
                .as_ref()
                .is_some_and(|m| m.contains_key("agent_id"))
                && let Some(agent_id_value) = task_agent_id_opt.or(agent_id_from_message)
            {
                let mut metadata = msg.metadata.unwrap_or_default();
                metadata.insert("agent_id".to_string(), agent_id_value);
                msg.metadata = Some(metadata);
            }
            self.task_store.insert_message(&msg).await;
        }
        if let Some(update) = status_update
            && let Some(status) = update.status
            && let Some(event) = self
                .task_store
                .record_status_update(update.task_id.clone(), update.context_id.clone(), status)
                .await
        {
            self.emitter.emit(event).await;
        }
        if let Some(update) = artifact_update
            && let Some(event) = self
                .task_store
                .record_artifact_update(
                    update.task_id.clone(),
                    update.context_id.clone(),
                    update.artifact.unwrap_or_default(),
                    update.append,
                    update.last_chunk,
                )
                .await
        {
            self.emitter.emit(event).await;
        }
        Ok(())
    }
}

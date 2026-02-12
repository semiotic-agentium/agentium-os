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

    /// I2: Single atomic apply per chunk; no interleaving of upsert/status/artifact/message.
    async fn process(
        &self,
        task: Option<Task>,
        message: Option<Message>,
        status_update: Option<TaskStatusUpdateEvent>,
        artifact_update: Option<TaskArtifactUpdateEvent>,
    ) -> Result<()> {
        // Agent_id injection is done inside the store (ProvenanceTaskStore) when using apply_task_delta.
        let mut message = message;
        if let Some(ref mut msg) = message {
            // Ensure message has agent_id when store does not inject (e.g. Mutex<TaskStore>)
            if msg
                .metadata
                .as_ref()
                .is_none_or(|m| !m.contains_key("agent_id"))
            {
                let agent_id_value = task
                    .as_ref()
                    .and_then(|t| t.metadata.as_ref().and_then(|m| m.get("agent_id").cloned()));
                if let Some(agent_id_value) = agent_id_value {
                    let mut metadata = msg.metadata.clone().unwrap_or_default();
                    metadata.insert("agent_id".to_string(), agent_id_value);
                    msg.metadata = Some(metadata);
                }
            }
        }
        let events = self
            .task_store
            .apply_task_delta(task, message, status_update, artifact_update)
            .await?;
        for event in events {
            self.emitter.emit(event).await;
        }
        Ok(())
    }
}

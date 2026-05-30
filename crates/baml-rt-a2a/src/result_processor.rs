// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use baml_rt_core::Result;

use crate::{
    a2a_store::TaskStoreBackend,
    a2a_types::{SendMessageResponse, StreamResponse, Task, ValidatedTaskChunk},
    events::EventEmitter,
};

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
        let chunk = ValidatedTaskChunk::try_from(stream)?;
        self.process_validated_chunk(chunk).await
    }

    pub async fn process_send_message_response(&self, response: SendMessageResponse) -> Result<()> {
        let chunk = ValidatedTaskChunk::try_from(response)?;
        self.process_validated_chunk(chunk).await
    }

    pub async fn process_task(&self, task: Task) -> Result<()> {
        let stream = StreamResponse {
            task: Some(task),
            ..Default::default()
        };
        let chunk = ValidatedTaskChunk::try_from(stream)?;
        self.process_validated_chunk(chunk).await
    }

    /// I2: Single atomic apply per chunk; no interleaving of upsert/status/artifact/message.
    pub async fn process_validated_chunk(&self, mut chunk: ValidatedTaskChunk) -> Result<()> {
        let mut msg_owned = chunk.message().cloned();
        if let Some(ref mut msg) = msg_owned
            && msg
                .metadata
                .as_ref()
                .is_none_or(|m| !m.contains_key("agent_id"))
            && let Some(agent_id_value) = chunk
                .task()
                .and_then(|t| t.metadata.as_ref().and_then(|m| m.get("agent_id").cloned()))
        {
            let mut metadata = msg.metadata.clone().unwrap_or_default();
            metadata.insert("agent_id".to_string(), agent_id_value);
            msg.metadata = Some(metadata);
        }
        let mut sr = chunk.into_stream_response();
        sr.message = msg_owned;
        chunk = ValidatedTaskChunk::try_from(sr)?;

        let events = self.task_store.apply_task_chunk(chunk).await?;
        for event in events {
            self.emitter.emit(event).await;
        }
        Ok(())
    }
}

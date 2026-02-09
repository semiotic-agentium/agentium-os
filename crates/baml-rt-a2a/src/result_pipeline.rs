use crate::a2a_store::TaskStoreBackend;
use crate::events::EventEmitter;
use crate::result_extractor::{A2aResultExtractor, ResultExtractor};
use crate::result_processor::TaskProcessor;
use baml_rt_core::Result;
use serde_json::Value;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait ResultStoragePipeline: Send + Sync {
    async fn store_result(&self, value: &Value) -> Result<()>;
}

pub struct A2aResultPipeline {
    extractor: Arc<dyn ResultExtractor>,
    processor: Arc<TaskProcessor>,
}

impl A2aResultPipeline {
    pub fn new(task_store: Arc<dyn TaskStoreBackend>, emitter: Arc<dyn EventEmitter>) -> Self {
        let extractor: Arc<dyn ResultExtractor> = Arc::new(A2aResultExtractor);
        let processor = Arc::new(TaskProcessor::new(task_store, emitter));
        Self {
            extractor,
            processor,
        }
    }
}

#[async_trait::async_trait]
impl ResultStoragePipeline for A2aResultPipeline {
    async fn store_result(&self, value: &Value) -> Result<()> {
        if (value.get("statusUpdate").is_some() || value.get("artifactUpdate").is_some())
            && let Some(stream) = self.extractor.extract_stream_response(value)?
        {
            return self.processor.process_stream_response(stream).await;
        }

        if let Some(response) = self.extractor.extract_send_message_response(value)? {
            return self.processor.process_send_message_response(response).await;
        }

        if let Some(stream) = self.extractor.extract_stream_response(value)? {
            return self.processor.process_stream_response(stream).await;
        }

        if let Some(task) = self.extractor.extract_task(value)? {
            return self.processor.process_task(task).await;
        }

        Ok(())
    }
}

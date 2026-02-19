use baml_rt_core::Result;
use serde_json::Value;

use crate::a2a_types::{SendMessageResponse, StreamResponse, Task};

pub trait ResultExtractor: Send + Sync {
    fn extract_stream_response(&self, value: &Value) -> Result<Option<StreamResponse>>;
    fn extract_send_message_response(&self, value: &Value) -> Result<Option<SendMessageResponse>>;
    fn extract_task(&self, value: &Value) -> Result<Option<Task>>;
}

pub struct A2aResultExtractor;

impl ResultExtractor for A2aResultExtractor {
    fn extract_stream_response(&self, value: &Value) -> Result<Option<StreamResponse>> {
        let stream = serde_json::from_value::<StreamResponse>(value.clone()).ok();
        Ok(stream)
    }

    fn extract_send_message_response(&self, value: &Value) -> Result<Option<SendMessageResponse>> {
        let response = serde_json::from_value::<SendMessageResponse>(value.clone()).ok();
        Ok(response)
    }

    fn extract_task(&self, value: &Value) -> Result<Option<Task>> {
        let task = serde_json::from_value::<Task>(value.clone()).ok();
        Ok(task)
    }
}

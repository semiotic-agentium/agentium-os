use crate::events::ProvEvent;
use crate::store::ProvenanceWriter;
use async_trait::async_trait;
use baml_rt_core::context;
use baml_rt_core::ids::{ExternalId, MessageId};
use baml_rt_core::{BamlRtError, Result};
use baml_rt_interceptor::{
    InterceptorDecision, LLMCallContext, LLMInterceptor, ToolCallContext, ToolInterceptor,
};
use serde_json::Value;
use std::sync::Arc;

pub struct ProvenanceInterceptor {
    writer: Arc<dyn ProvenanceWriter>,
}

impl ProvenanceInterceptor {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl LLMInterceptor for ProvenanceInterceptor {
    async fn intercept_llm_call(&self, context: &LLMCallContext) -> Result<InterceptorDecision> {
        let task_id = context::current_task_id();
        let message_id = message_id_from_metadata(&context.metadata);
        if task_id.is_none() && message_id.is_none() {
            return Err(BamlRtError::InvalidArgument(
                "LLM call missing metadata.message_id".to_string(),
            ));
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::llm_call_started_task(
                context.context_id.clone(),
                task_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                context.prompt.clone(),
                context.metadata.clone(),
            )
        } else {
            let message_id = match message_id {
                Some(message_id) => message_id,
                None => {
                    return Err(BamlRtError::InvalidArgument(
                        "LLM call missing metadata.message_id".to_string(),
                    ));
                }
            };
            ProvEvent::llm_call_started_global(
                context.context_id.clone(),
                message_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                context.prompt.clone(),
                context.metadata.clone(),
            )
        };
        self.writer
            .add_event_with_logging(event, "LLM call start")
            .await;
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        context: &LLMCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        let success = result.is_ok();
        let task_id = context::current_task_id();
        let message_id = message_id_from_metadata(&context.metadata);
        if task_id.is_none() && message_id.is_none() {
            tracing::error!("LLM call completion missing metadata.message_id");
            return;
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::llm_call_completed_task(
                context.context_id.clone(),
                task_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                context.prompt.clone(),
                context.metadata.clone(),
                crate::events::LlmUsage::Unknown,
                duration_ms,
                success,
            )
        } else {
            let message_id = match message_id {
                Some(message_id) => message_id,
                None => {
                    tracing::error!("LLM call completion missing metadata.message_id");
                    return;
                }
            };
            ProvEvent::llm_call_completed_global(
                context.context_id.clone(),
                message_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                context.prompt.clone(),
                context.metadata.clone(),
                crate::events::LlmUsage::Unknown,
                duration_ms,
                success,
            )
        };
        self.writer
            .add_event_with_logging(event, "LLM call completion")
            .await;
    }
}

#[async_trait]
impl ToolInterceptor for ProvenanceInterceptor {
    async fn intercept_tool_call(&self, context: &ToolCallContext) -> Result<InterceptorDecision> {
        let task_id = context::current_task_id();
        let message_id = message_id_from_metadata(&context.metadata);
        if task_id.is_none() && message_id.is_none() {
            return Err(BamlRtError::InvalidArgument(
                "Tool call missing metadata.message_id".to_string(),
            ));
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::tool_call_started_task(
                context.context_id.clone(),
                task_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                context.metadata.clone(),
            )
        } else {
            let message_id = match message_id {
                Some(message_id) => message_id,
                None => {
                    return Err(BamlRtError::InvalidArgument(
                        "Tool call missing metadata.message_id".to_string(),
                    ));
                }
            };
            ProvEvent::tool_call_started_global(
                context.context_id.clone(),
                message_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                context.metadata.clone(),
            )
        };
        self.writer
            .add_event_with_logging(event, "tool call start")
            .await;
        Ok(InterceptorDecision::Allow)
    }

    async fn on_tool_call_complete(
        &self,
        context: &ToolCallContext,
        result: &Result<Value>,
        duration_ms: u64,
    ) {
        let success = result.is_ok();
        let task_id = context::current_task_id();
        let message_id = message_id_from_metadata(&context.metadata);
        if task_id.is_none() && message_id.is_none() {
            tracing::error!("Tool call completion missing metadata.message_id");
            return;
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::tool_call_completed_task(
                context.context_id.clone(),
                task_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                context.metadata.clone(),
                duration_ms,
                success,
            )
        } else {
            let message_id = match message_id {
                Some(message_id) => message_id,
                None => {
                    tracing::error!("Tool call completion missing metadata.message_id");
                    return;
                }
            };
            ProvEvent::tool_call_completed_global(
                context.context_id.clone(),
                message_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                context.metadata.clone(),
                duration_ms,
                success,
            )
        };
        self.writer
            .add_event_with_logging(event, "tool call completion")
            .await;
    }
}

fn message_id_from_metadata(metadata: &Value) -> Option<MessageId> {
    metadata
        .get("message_id")
        .and_then(|value| value.as_str())
        .map(|value| MessageId::from_external(ExternalId::new(value.to_string())))
}

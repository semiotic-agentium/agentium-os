use crate::events::ProvEvent;
use crate::store::ProvenanceWriter;
use async_trait::async_trait;
use baml_rt_core::context::RuntimeScope;
use baml_rt_core::ids::MessageId;
use baml_rt_core::{BamlRtError, Outcome, Result};
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
        let task_id = context.runtime_scope.task_id_opt().cloned();
        let metadata = metadata_with_runtime_scope(&context.metadata, &context.runtime_scope);
        let prompt = normalized_prompt(&context.prompt);
        let message_id = message_id_from_scope(&context.runtime_scope);
        if task_id.is_none() && message_id.is_none() {
            return Err(BamlRtError::InvalidArgument(
                "LLM call missing metadata.message_id".to_string(),
            ));
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::llm_call_started_task(
                context.runtime_scope.context_id().clone(),
                task_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                prompt.clone(),
                metadata.clone(),
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
                context.runtime_scope.context_id().clone(),
                message_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                prompt.clone(),
                metadata.clone(),
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
        let outcome = Outcome::from(result.is_ok());
        let task_id = context.runtime_scope.task_id_opt().cloned();
        let metadata = metadata_with_runtime_scope(&context.metadata, &context.runtime_scope);
        let prompt = normalized_prompt(&context.prompt);
        let message_id = message_id_from_scope(&context.runtime_scope);
        if task_id.is_none() && message_id.is_none() {
            tracing::error!("LLM call completion missing metadata.message_id");
            return;
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::llm_call_completed_task(
                context.runtime_scope.context_id().clone(),
                task_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                prompt.clone(),
                metadata.clone(),
                crate::events::LlmUsage::Unknown,
                duration_ms,
                outcome,
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
                context.runtime_scope.context_id().clone(),
                message_id,
                context.client.clone(),
                context.model.clone(),
                context.function_name.clone(),
                prompt.clone(),
                metadata.clone(),
                crate::events::LlmUsage::Unknown,
                duration_ms,
                outcome,
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
        let task_id = context.runtime_scope.task_id_opt().cloned();
        let metadata = metadata_with_runtime_scope(&context.metadata, &context.runtime_scope);
        let message_id = message_id_from_scope(&context.runtime_scope);
        if task_id.is_none() && message_id.is_none() {
            return Err(BamlRtError::InvalidArgument(
                "Tool call missing metadata.message_id".to_string(),
            ));
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::tool_call_started_task(
                context.runtime_scope.context_id().clone(),
                task_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                metadata.clone(),
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
                context.runtime_scope.context_id().clone(),
                message_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                metadata.clone(),
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
        let outcome = Outcome::from(result.is_ok());
        let task_id = context.runtime_scope.task_id_opt().cloned();
        let metadata = metadata_with_tool_result(&context.metadata, &context.runtime_scope, result);
        let message_id = message_id_from_scope(&context.runtime_scope);
        if task_id.is_none() && message_id.is_none() {
            tracing::error!("Tool call completion missing metadata.message_id");
            return;
        }
        let event = if let Some(task_id) = task_id {
            ProvEvent::tool_call_completed_task(
                context.runtime_scope.context_id().clone(),
                task_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                metadata.clone(),
                duration_ms,
                outcome,
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
                context.runtime_scope.context_id().clone(),
                message_id,
                context.tool_name.clone(),
                context.function_name.clone(),
                context.args.clone(),
                metadata.clone(),
                duration_ms,
                outcome,
            )
        };

        self.writer
            .add_event_with_logging(event, "tool call completion")
            .await;
    }
}

fn message_id_from_scope(scope: &RuntimeScope) -> Option<MessageId> {
    Some(scope.message_id().clone())
}

fn metadata_with_runtime_scope(metadata: &Value, scope: &RuntimeScope) -> Value {
    let mut out = match metadata {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    out.insert(
        "agent_id".to_string(),
        Value::String(scope.agent_id().as_str().to_string()),
    );
    out.insert(
        "message_id".to_string(),
        Value::String(scope.message_id().as_str().to_string()),
    );
    if let Some(task_id) = scope.task_id_opt() {
        out.insert(
            "task_id".to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
    Value::Object(out)
}

fn metadata_with_tool_result(
    metadata: &Value,
    scope: &RuntimeScope,
    result: &Result<Value>,
) -> Value {
    let mut out = match metadata_with_runtime_scope(metadata, scope) {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    match result {
        Ok(value) => {
            out.insert("result".to_string(), value.clone());
        }
        Err(error) => {
            out.insert("error".to_string(), Value::String(error.to_string()));
        }
    }
    Value::Object(out)
}

fn normalized_prompt(prompt: &Value) -> Value {
    if prompt.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        prompt.clone()
    }
}

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Outcome, Result,
    context::RuntimeScope,
    ids::{ActivityAnchorId, MessageId},
};
use baml_rt_interceptor::{
    InterceptorDecision, LLMCallContext, LLMInterceptor, ToolCallContext, ToolInterceptor,
};
use baml_rt_observability::metrics;
use serde_json::Value;

use crate::{
    events::{BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR, LlmUsage, ProvEvent},
    store::ProvenanceWriter,
};

pub struct ProvenanceInterceptor {
    writer: Arc<dyn ProvenanceWriter>,
}

impl ProvenanceInterceptor {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self { writer }
    }
}

fn prompt_bytes(prompt: &Value) -> usize {
    prompt.to_string().len()
}

fn usage_tokens(usage: &LlmUsage) -> (Option<u64>, Option<u64>) {
    match usage {
        LlmUsage::Known {
            prompt_tokens,
            completion_tokens,
            ..
        } => (Some(*prompt_tokens), Some(*completion_tokens)),
        LlmUsage::Unknown => (None, None),
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
                context.function_id.full_name(),
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
                context.function_id.full_name(),
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
        let metadata = metadata_with_llm_result(&context.metadata, &context.runtime_scope, result);
        let prompt = normalized_prompt(&context.prompt);
        let message_id = message_id_from_scope(&context.runtime_scope);
        let usage = extract_usage_from_metadata(&context.metadata);
        if task_id.is_none() && message_id.is_none() {
            tracing::error!("LLM call completion missing metadata.message_id");
            return;
        }

        let function_name = context.function_id.full_name();
        let result_label = if result.is_ok() { "success" } else { "error" };
        let prompt_size = prompt_bytes(&context.prompt);
        let (tokens_in, tokens_out) = usage_tokens(&usage);
        metrics::record_llm_call(
            &function_name,
            &context.client,
            &context.model,
            result_label,
            std::time::Duration::from_millis(duration_ms),
            prompt_size,
            tokens_in,
            tokens_out,
        );
        tracing::info!(
            event = "llm_call_attribution",
            function_name = %function_name,
            client = %context.client,
            model = %context.model,
            duration_ms,
            prompt_bytes = prompt_size,
            tokens_in,
            tokens_out,
            context_id = %context.runtime_scope.context_id(),
            task_id = ?context.runtime_scope.task_id_opt(),
            message_id = %context.runtime_scope.message_id(),
            result = result_label,
        );

        let event = if let Some(task_id) = task_id {
            ProvEvent::llm_call_completed_task(
                context.runtime_scope.context_id().clone(),
                task_id,
                context.client.clone(),
                context.model.clone(),
                context.function_id.full_name(),
                prompt.clone(),
                metadata.clone(),
                usage,
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
                context.function_id.full_name(),
                prompt.clone(),
                metadata.clone(),
                usage,
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
                context.delegation_target.clone(),
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
                context.delegation_target.clone(),
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
        let reserved_anchor = context
            .metadata
            .get(BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR)
            .and_then(Value::as_str)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(ActivityAnchorId::from);
        tracing::debug!(
            tool_name = %context.tool_name,
            has_reserved_anchor = reserved_anchor.is_some(),
            "on_tool_call_complete: reserved anchor extraction"
        );
        let metadata = metadata_with_tool_result(&context.metadata, &context.runtime_scope, result);
        let message_id = message_id_from_scope(&context.runtime_scope);
        if task_id.is_none() && message_id.is_none() {
            tracing::error!("Tool call completion missing metadata.message_id");
            return;
        }

        let result_label = if result.is_ok() { "success" } else { "error" };
        tracing::info!(
            event = "tool_call_attribution",
            tool_name = %context.tool_name,
            function_name = ?context.function_name,
            duration_ms,
            context_id = %context.runtime_scope.context_id(),
            task_id = ?context.runtime_scope.task_id_opt(),
            message_id = %context.runtime_scope.message_id(),
            result = result_label,
        );

        let event = if let Some(task_id) = task_id {
            if let Some(id) = reserved_anchor {
                ProvEvent::tool_call_completed_task_with_id(
                    id,
                    context.runtime_scope.context_id().clone(),
                    task_id,
                    context.tool_name.clone(),
                    context.function_name.clone(),
                    context.args.clone(),
                    metadata.clone(),
                    duration_ms,
                    outcome,
                    context.delegation_target.clone(),
                )
            } else {
                ProvEvent::tool_call_completed_task(
                    context.runtime_scope.context_id().clone(),
                    task_id,
                    context.tool_name.clone(),
                    context.function_name.clone(),
                    context.args.clone(),
                    metadata.clone(),
                    duration_ms,
                    outcome,
                    context.delegation_target.clone(),
                )
            }
        } else {
            let message_id = match message_id {
                Some(message_id) => message_id,
                None => {
                    tracing::error!("Tool call completion missing metadata.message_id");
                    return;
                }
            };
            if let Some(id) = reserved_anchor {
                ProvEvent::tool_call_completed_global_with_id(
                    id,
                    context.runtime_scope.context_id().clone(),
                    message_id,
                    context.tool_name.clone(),
                    context.function_name.clone(),
                    context.args.clone(),
                    metadata.clone(),
                    duration_ms,
                    outcome,
                    context.delegation_target.clone(),
                )
            } else {
                ProvEvent::tool_call_completed_global(
                    context.runtime_scope.context_id().clone(),
                    message_id,
                    context.tool_name.clone(),
                    context.function_name.clone(),
                    context.args.clone(),
                    metadata.clone(),
                    duration_ms,
                    outcome,
                    context.delegation_target.clone(),
                )
            }
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
    out.remove(BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR);
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

fn metadata_with_llm_result(
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

/// Extract LLM token usage from interceptor call metadata.
///
/// `BamlLLMCollector::extract_context_from_llm_call` places the BAML trace
/// `call.usage` value under `metadata["usage"]`. This function attempts to
/// parse that JSON into `LlmUsage::Known`; if the field is absent or
/// malformed, it falls back to `LlmUsage::Unknown`.
fn extract_usage_from_metadata(metadata: &Value) -> LlmUsage {
    let usage = match metadata.get("usage") {
        Some(v) if !v.is_null() => v,
        // usage key absent or null — provider did not report usage.
        _ => return LlmUsage::Unknown,
    };

    // Accept both naming styles:
    // - prompt/completion (OpenAI-style)
    // - input/output (BAML collector Usage style)
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(parse_u64_value)
        .or_else(|| usage.get("input_tokens").and_then(parse_u64_value));
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(parse_u64_value)
        .or_else(|| usage.get("output_tokens").and_then(parse_u64_value));
    let total_tokens = usage.get("total_tokens").and_then(parse_u64_value);
    let cached_input_tokens = usage
        .get("cached_input_tokens")
        .and_then(parse_u64_value)
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(Value::as_object)
                .and_then(|details| details.get("cached_tokens"))
                .and_then(parse_u64_value)
        })
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(Value::as_object)
                .and_then(|details| details.get("cached_tokens"))
                .and_then(parse_u64_value)
        });

    match (prompt_tokens, completion_tokens, total_tokens) {
        (Some(prompt), Some(completion), Some(total)) => LlmUsage::Known {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cached_input_tokens,
        },
        (Some(prompt), Some(completion), None) => LlmUsage::Known {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt.saturating_add(completion),
            cached_input_tokens,
        },
        _ => {
            tracing::debug!(
                usage = ?usage,
                "LLM usage metadata present but missing expected token fields"
            );
            LlmUsage::Unknown
        }
    }
}

fn parse_u64_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
}

#[cfg(test)]
mod tests {
    use baml_rt_core::{
        context::RuntimeScope,
        ids::{AgentId, ContextId, ExternalId, MessageId, UuidId},
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn extracts_known_usage_from_valid_metadata() {
        let metadata = json!({
            "usage": {
                "prompt_tokens": 1500,
                "completion_tokens": 200,
                "total_tokens": 1700
            },
            "agent_id": "a1",
            "message_id": "m1"
        });

        let usage = extract_usage_from_metadata(&metadata);
        assert_eq!(
            usage,
            LlmUsage::Known {
                prompt_tokens: 1500,
                completion_tokens: 200,
                total_tokens: 1700,
                cached_input_tokens: None,
            }
        );
    }

    #[test]
    fn computes_total_when_missing() {
        let metadata = json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50
            }
        });

        let usage = extract_usage_from_metadata(&metadata);
        assert_eq!(
            usage,
            LlmUsage::Known {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                cached_input_tokens: None,
            }
        );
    }

    #[test]
    fn extracts_known_usage_from_input_output_shape() {
        let metadata = json!({
            "usage": {
                "input_tokens": 1200,
                "output_tokens": 240,
                "total_tokens": 1440
            }
        });

        let usage = extract_usage_from_metadata(&metadata);
        assert_eq!(
            usage,
            LlmUsage::Known {
                prompt_tokens: 1200,
                completion_tokens: 240,
                total_tokens: 1440,
                cached_input_tokens: None,
            }
        );
    }

    #[test]
    fn computes_total_from_input_output_when_total_missing() {
        let metadata = json!({
            "usage": {
                "input_tokens": 80,
                "output_tokens": 20
            }
        });

        let usage = extract_usage_from_metadata(&metadata);
        assert_eq!(
            usage,
            LlmUsage::Known {
                prompt_tokens: 80,
                completion_tokens: 20,
                total_tokens: 100,
                cached_input_tokens: None,
            }
        );
    }

    #[test]
    fn extracts_cached_input_tokens_from_nested_usage_details() {
        let metadata = json!({
            "usage": {
                "input_tokens": 1200,
                "output_tokens": 240,
                "total_tokens": 1440,
                "input_tokens_details": {
                    "cached_tokens": 1190
                }
            }
        });

        let usage = extract_usage_from_metadata(&metadata);
        assert_eq!(
            usage,
            LlmUsage::Known {
                prompt_tokens: 1200,
                completion_tokens: 240,
                total_tokens: 1440,
                cached_input_tokens: Some(1190),
            }
        );
    }

    #[test]
    fn returns_unknown_when_usage_absent() {
        let metadata = json!({ "agent_id": "a1" });
        assert_eq!(extract_usage_from_metadata(&metadata), LlmUsage::Unknown);
    }

    #[test]
    fn llm_completion_metadata_includes_result_payload() {
        let scope = RuntimeScope::message_scope(
            ContextId::new(1, 1),
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000123").unwrap()),
            MessageId::from_external(ExternalId::new("msg-1".to_string())),
        );
        let metadata = json!({ "usage": { "prompt_tokens": 1, "completion_tokens": 1 } });
        let result = Ok(json!({ "steps": [{ "op": "Open" }] }));
        let merged = metadata_with_llm_result(&metadata, &scope, &result);
        let obj = merged.as_object().expect("metadata must be object");
        assert!(obj.get("result").is_some(), "result must be present");
        assert_eq!(
            obj.get("message_id").and_then(Value::as_str),
            Some("msg-1"),
            "runtime scope fields must be preserved"
        );
    }
}

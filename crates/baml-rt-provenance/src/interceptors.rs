// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::Result;
#[cfg(test)]
use baml_rt_core::context::RuntimeScope;
use baml_rt_interceptor::{
    InterceptorDecision, LLMCallContext, LLMInterceptor, ToolCallContext, ToolInterceptor,
};
use serde_json::Value;

#[cfg(test)]
use crate::events::LlmUsage;
use crate::store::ProvenanceWriter;

pub struct ProvenanceInterceptor;

impl ProvenanceInterceptor {
    pub fn new(_writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self
    }
}

// NOTE: `ProvenanceInterceptor` still implements `LLMInterceptor` for compatibility,
// but A2A wiring intentionally does NOT register it for LLM provenance writes.
// LLM start/completion provenance is sourced from `EffectEvent` via
// `ProvenanceEffectSubscriber` to prevent duplicate `LlmCallCompleted` events.
#[async_trait]
impl LLMInterceptor for ProvenanceInterceptor {
    async fn intercept_llm_call(&self, _context: &LLMCallContext) -> Result<InterceptorDecision> {
        Ok(InterceptorDecision::Allow)
    }

    async fn on_llm_call_complete(
        &self,
        _context: &LLMCallContext,
        _result: &Result<Value>,
        _duration_ms: u64,
    ) {
    }
}

#[async_trait]
impl ToolInterceptor for ProvenanceInterceptor {
    async fn intercept_tool_call(&self, _context: &ToolCallContext) -> Result<InterceptorDecision> {
        Ok(InterceptorDecision::Allow)
    }

    async fn on_tool_call_complete(
        &self,
        _context: &ToolCallContext,
        _result: &Result<Value>,
        _duration_ms: u64,
    ) {
    }
}

#[cfg(test)]
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

#[cfg(test)]
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

/// Extract LLM token usage from interceptor call metadata.
///
/// `BamlLLMCollector::extract_context_from_llm_call` places the BAML trace
/// `call.usage` value under `metadata["usage"]`. This function attempts to
/// parse that JSON into `LlmUsage::Known`; if the field is absent or
/// malformed, it falls back to `LlmUsage::Unknown`.
#[cfg(test)]
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

#[cfg(test)]
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

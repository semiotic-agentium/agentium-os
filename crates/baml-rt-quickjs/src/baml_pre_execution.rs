//! Pre-execution LLM interception
//!
//! This module implements pre-execution interception by using BAML's build_request
//! to intercept LLM calls before the HTTP request is sent.

use std::{collections::HashMap, sync::Arc};

use baml_rt_core::{
    BamlRtError, InvocationKind, Result,
    bus::{EffectEmitter, LlmEffectMetadata},
    context,
};
use baml_rt_interceptor::{InterceptorDecision, InterceptorRegistry, LLMCallContext};
use baml_runtime::RuntimeContextManager;
use baml_types::{BamlMap, BamlValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;

// Helper function for ergonomic metadata construction
fn llm_effect_metadata_from_context(ctx: &LLMCallContext) -> LlmEffectMetadata {
    LlmEffectMetadata {
        client: ctx.client.clone(),
        model: ctx.model.clone(),
        function_name: ctx.function_name.clone(),
        prompt: ctx.prompt.clone(),
        metadata: ctx.metadata.clone(),
    }
}

fn is_unknown_placeholder(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unknown")
}

fn find_non_unknown_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !is_unknown_placeholder(s))
                {
                    return Some(found.to_string());
                }
            }
            for nested in map.values() {
                if let Some(found) = find_non_unknown_string(nested, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|nested| find_non_unknown_string(nested, keys)),
        _ => None,
    }
}

fn infer_provider_from_prompt_shape(prompt: &Value) -> Option<String> {
    let Value::Object(map) = prompt else {
        return None;
    };
    if map.contains_key("anthropic_version") {
        return Some("anthropic".to_string());
    }
    if map.contains_key("contents") {
        return Some("google".to_string());
    }
    if map.contains_key("messages") || map.contains_key("input") {
        // OpenAI-compatible request envelopes (including OpenRouter's compatibility surface).
        return Some("openai".to_string());
    }
    None
}

fn normalized_prompt_payload(prompt: &Value) -> Value {
    match prompt {
        Value::String(raw) => serde_json::from_str::<Value>(raw).unwrap_or_else(|_| prompt.clone()),
        _ => prompt.clone(),
    }
}

/// Extract LLM call context from BAML's HTTPRequest
///
/// This extracts the client, model, and prompt information from the HTTPRequest
/// that BAML builds before sending to the LLM. Requires an invocation scope (e.g. run inside `context::with_scope`).
pub fn extract_context_from_http_request(
    scope: &context::RuntimeScope,
    http_request: &baml_types::tracing::events::HTTPRequest,
    function_name: &str,
) -> Result<LLMCallContext> {
    // Extract client and model from client_details
    // HTTPRequest has fields: id, url, method, body, client_details (Arc<ClientDetails>)
    // ClientDetails has fields: name, provider, options
    let (raw_client, raw_model) = {
        let client_details = &http_request.client_details;
        (client_details.name.clone(), client_details.provider.clone())
    };

    // Extract prompt/messages from the request body
    // body is directly an HTTPBody, not an Option
    let prompt = {
        // Try to serialize the body to JSON
        // HTTPBody should implement Serialize
        match serde_json::to_value(&http_request.body) {
            Ok(json_body) => json_body,
            Err(_) => {
                // Fallback: try to convert to string representation
                json!({"body": format!("{:?}", http_request.body)})
            }
        }
    };
    let prompt_payload = normalized_prompt_payload(&prompt);
    let client_details_json =
        serde_json::to_value(http_request.client_details.as_ref()).unwrap_or_else(|_| json!({}));

    let client = [raw_client.as_str(), raw_model.as_str()]
        .into_iter()
        .map(str::trim)
        .find(|v| !is_unknown_placeholder(v))
        .map(ToString::to_string)
        .or_else(|| {
            find_non_unknown_string(
                &client_details_json,
                &["provider_type", "provider", "client", "name"],
            )
        })
        .or_else(|| infer_provider_from_prompt_shape(&prompt_payload))
        .ok_or_else(|| {
            tracing::error!(
                raw_client = %raw_client,
                raw_model = %raw_model,
                client_details = %client_details_json,
                prompt_payload = %prompt_payload,
                "unable to resolve canonical BAML provider type from HTTPRequest client_details"
            );
            BamlRtError::InvalidArgument(
                "LLM call missing canonical BAML provider type (client)".to_string(),
            )
        })?;

    let model = find_non_unknown_string(&prompt_payload, &["model"])
        .or_else(|| find_non_unknown_string(&client_details_json, &["model", "model_name"]))
        .or_else(|| {
            Some(str::trim(raw_model.as_str()))
                .filter(|v| !is_unknown_placeholder(v))
                .map(ToString::to_string)
        })
        .ok_or_else(|| BamlRtError::InvalidArgument("LLM call missing model".to_string()))?;

    let mut metadata_map = serde_json::Map::new();
    metadata_map.insert("url".to_string(), Value::String(http_request.url.clone()));
    metadata_map.insert(
        "method".to_string(),
        Value::String(http_request.method.clone()),
    );
    metadata_map.insert("id".to_string(), Value::String(http_request.id.to_string()));
    metadata_map.insert(
        "message_id".to_string(),
        Value::String(scope.message_id().as_str().to_string()),
    );
    metadata_map.insert(
        "agent_id".to_string(),
        Value::String(scope.agent_id().as_str().to_string()),
    );
    metadata_map.insert("client".to_string(), Value::String(client.clone()));
    metadata_map.insert("model".to_string(), Value::String(model.clone()));
    if let Some(task_id) = scope.task_id_opt() {
        metadata_map.insert(
            "task_id".to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }

    Ok(LLMCallContext {
        client,
        model,
        function_name: function_name.to_string(),
        runtime_scope: scope.clone(),
        prompt,
        metadata: Value::Object(metadata_map),
    })
}

/// Intercept an LLM call before execution using build_request
///
/// This builds the HTTP request, extracts context, runs interceptors,
/// and returns the decision. If blocked, returns an error.
///
/// If a collector is provided, stores the effect token for later completion.
#[allow(clippy::too_many_arguments)]
pub async fn intercept_llm_call_pre_execution(
    runtime: &baml_runtime::BamlRuntime,
    scope: &context::RuntimeScope,
    function_name: &str,
    params: &BamlMap<String, BamlValue>,
    ctx_manager: &RuntimeContextManager,
    interceptor_registry: &Arc<Mutex<InterceptorRegistry>>,
    env_vars: HashMap<String, String>,
    invocation: InvocationKind,
    effect_emitter: Option<&Arc<dyn EffectEmitter>>,
    collector: Option<&crate::baml_collector::BamlLLMCollector>,
) -> Result<InterceptorDecision> {
    // Build the HTTP request to get LLM call details
    // This doesn't actually send the request, just builds it
    let http_request_result = runtime
        .build_request(
            function_name.to_string(),
            params,
            ctx_manager,
            None, // type_builder
            None, // client_registry
            env_vars,
            invocation.is_stream(),
        )
        .await;

    let http_request =
        http_request_result.map_err(|e| BamlRtError::RequestBuildFailed(e.to_string()))?;

    // Extract LLM call context from the HTTP request
    let context = extract_context_from_http_request(scope, &http_request, function_name)?;

    // Start effect and get token (type-safe start/complete pairing)
    let effect_metadata = llm_effect_metadata_from_context(&context);
    let context_id = context.runtime_scope.context_id().clone();

    if let Some(emitter) = effect_emitter {
        tracing::trace!(context_id = %context_id, "LlmStarted emitting (effect_emitter set)");
        match emitter.start_llm(context_id.clone(), effect_metadata).await {
            Ok(token) => {
                // Store token in collector for later completion
                if let Some(coll) = collector {
                    coll.store_effect_token(context_id, token).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to start LLM effect");
            }
        }
    } else {
        tracing::trace!(context_id = %context_id, "effect_emitter is None, skipping LlmStarted");
    }

    tracing::debug!(
        client = context.client,
        model = context.model,
        function = function_name,
        "Pre-execution interception: extracted LLM call context"
    );

    // Run interceptors
    let registry = interceptor_registry.lock().await;
    let decision = registry.intercept_llm_call(&context).await?;
    drop(registry);

    // Return the decision
    Ok(decision)
}

//! Pre-execution LLM interception
//!
//! This module implements pre-execution interception by using BAML's build_request
//! to intercept LLM calls before the HTTP request is sent.

use baml_rt_core::{BamlRtError, Result};
use baml_rt_core::context;
use baml_rt_core::effects::{EffectEmitter, LlmEffectMetadata};
use baml_rt_interceptor::{InterceptorDecision, InterceptorRegistry, LLMCallContext};
use baml_runtime::RuntimeContextManager;
use baml_types::{BamlMap, BamlValue};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
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


/// Extract LLM call context from BAML's HTTPRequest
///
/// This extracts the client, model, and prompt information from the HTTPRequest
/// that BAML builds before sending to the LLM.
pub fn extract_context_from_http_request(
    http_request: &baml_types::tracing::events::HTTPRequest,
    function_name: &str,
) -> LLMCallContext {
    // Extract client and model from client_details
    // HTTPRequest has fields: id, url, method, body, client_details (Arc<ClientDetails>)
    // ClientDetails has fields: name, provider, options
    let (client, model) = {
        let client_details = &http_request.client_details;
        (
            client_details.name.clone(),
            client_details.provider.clone(),
        )
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

    let mut metadata_map = serde_json::Map::new();
    metadata_map.insert("url".to_string(), Value::String(http_request.url.clone()));
    metadata_map.insert("method".to_string(), Value::String(http_request.method.clone()));
    metadata_map.insert("id".to_string(), Value::String(http_request.id.to_string()));
    if let Some(message_id) = context::current_message_id() {
        metadata_map.insert("message_id".to_string(), Value::String(message_id.as_str().to_string()));
    }

    LLMCallContext {
        client,
        model,
        function_name: function_name.to_string(),
        context_id: context::current_or_new(),
        prompt,
        metadata: Value::Object(metadata_map),
    }
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
    function_name: &str,
    params: &BamlMap<String, BamlValue>,
    ctx_manager: &RuntimeContextManager,
    interceptor_registry: &Arc<Mutex<InterceptorRegistry>>,
    env_vars: HashMap<String, String>,
    stream: bool,
    effect_emitter: Option<&Arc<dyn EffectEmitter>>,
    collector: Option<&crate::baml_collector::BamlLLMCollector>,
) -> Result<InterceptorDecision> {
    // Build the HTTP request to get LLM call details
    // This doesn't actually send the request, just builds it
    let http_request_result = runtime.build_request(
        function_name.to_string(),
        params,
        ctx_manager,
        None, // type_builder
        None, // client_registry
        env_vars,
        stream,
    ).await;

    let http_request = http_request_result
        .map_err(|e| BamlRtError::RequestBuildFailed(e.to_string()))?;

    // Extract LLM call context from the HTTP request
    let context = extract_context_from_http_request(&http_request, function_name);

    // Start effect and get token (type-safe start/complete pairing)
    let effect_metadata = llm_effect_metadata_from_context(&context);
    let context_id = context.context_id.clone();
    
    if let Some(emitter) = effect_emitter {
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

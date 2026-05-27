//! Pre-execution LLM interception
//!
//! This module implements pre-execution interception by using BAML's build_request
//! to intercept LLM calls before the HTTP request is sent.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, OnceLock},
};

use baml_rt_core::{
    BamlFunctionId, BamlRtError, InvocationKind, Result,
    bus::{EffectEmitter, LlmEffectMetadata, ToolNameResolution},
    context,
};
use baml_rt_interceptor::{InterceptorDecision, InterceptorRegistry, LLMCallContext};
use baml_runtime::{RuntimeContextManager, client_registry::ClientRegistry};
use baml_types::{BamlMap, BamlValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::baml::FunctionToolManifest;

// Helper function for ergonomic metadata construction
fn llm_effect_metadata_from_context(
    ctx: &LLMCallContext,
    resolved_tool_name: ToolNameResolution,
) -> LlmEffectMetadata {
    LlmEffectMetadata {
        client: ctx.client.clone(),
        model: ctx.model.clone(),
        function_name: ctx.function_id.full_name(),
        prompt: ctx.prompt.clone(),
        metadata: ctx.metadata.clone(),
        tool_name: resolved_tool_name,
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

fn prompt_payload_bytes(prompt_payload: &Value) -> usize {
    serde_json::to_vec(prompt_payload).map_or(0, |v| v.len())
}

static PREVIOUS_PROMPT_PAYLOADS: OnceLock<StdMutex<HashMap<String, Vec<u8>>>> = OnceLock::new();

fn shared_prefix_len_bytes(a: &[u8], b: &[u8]) -> usize {
    let max = a.len().min(b.len());
    let mut i = 0usize;
    while i < max && a[i] == b[i] {
        i += 1;
    }
    i
}

fn compute_and_store_prefix_cacheability(
    key: String,
    current_payload: Vec<u8>,
) -> (usize, usize, f64) {
    let store = PREVIOUS_PROMPT_PAYLOADS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut guard = store.lock().expect("previous prompt payload lock poisoned");
    let previous = guard.get(&key).cloned();
    let current_bytes = current_payload.len();
    let shared_prefix_bytes = previous.as_ref().map_or(0usize, |prev| {
        shared_prefix_len_bytes(prev, &current_payload)
    });
    guard.insert(key, current_payload);
    let cacheable_pct = if current_bytes == 0 {
        0.0
    } else {
        (shared_prefix_bytes as f64 / current_bytes as f64) * 100.0
    };
    (
        shared_prefix_bytes,
        previous.map_or(0usize, |prev| prev.len()),
        cacheable_pct,
    )
}

fn prompt_message_count(prompt_payload: &Value) -> usize {
    match prompt_payload {
        Value::Object(map) => {
            if let Some(Value::Array(messages)) = map.get("messages") {
                return messages.len();
            }
            if let Some(Value::Array(contents)) = map.get("contents") {
                return contents.len();
            }
            0
        }
        _ => 0,
    }
}

/// Build a minimal LLM call context from scope and function name only.
/// Used when request building fails (e.g. missing secrets) so interceptors can still
/// return Substitute or Block without needing the full HTTP request.
fn minimal_llm_context(scope: &context::RuntimeScope, function_name: &str) -> LLMCallContext {
    let mut metadata_map = serde_json::Map::new();
    metadata_map.insert(
        "message_id".to_string(),
        Value::String(scope.message_id().as_str().to_string()),
    );
    metadata_map.insert(
        "agent_id".to_string(),
        Value::String(scope.agent_id().as_str().to_string()),
    );
    if let Some(task_id) = scope.task_id_opt() {
        metadata_map.insert(
            "task_id".to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
    LLMCallContext {
        client: String::new(),
        model: String::new(),
        function_id: BamlFunctionId::parse(function_name),
        runtime_scope: scope.clone(),
        prompt: Value::Null,
        metadata: Value::Object(metadata_map),
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
    planning_step: Option<(&str, &str)>,
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

    let payload_bytes = prompt_payload_bytes(&prompt_payload);
    let message_count = prompt_message_count(&prompt_payload);
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
    if !is_unknown_placeholder(&raw_client) {
        metadata_map.insert(
            "client_alias".to_string(),
            Value::String(raw_client.clone()),
        );
    }
    if !is_unknown_placeholder(&raw_model) {
        metadata_map.insert("model_alias".to_string(), Value::String(raw_model.clone()));
    }
    metadata_map.insert(
        "prompt_payload_bytes".to_string(),
        Value::from(payload_bytes as u64),
    );
    metadata_map.insert(
        "prompt_message_count".to_string(),
        Value::from(message_count as u64),
    );
    if let Some(task_id) = scope.task_id_opt() {
        metadata_map.insert(
            "task_id".to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
    if let Some((plan_id, step_id)) = planning_step {
        metadata_map.insert("plan_id".to_string(), Value::String(plan_id.to_string()));
        metadata_map.insert("step_id".to_string(), Value::String(step_id.to_string()));
    }

    Ok(LLMCallContext {
        client,
        model,
        function_id: BamlFunctionId::parse(function_name),
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
#[expect(
    clippy::too_many_arguments,
    reason = "pre-execution hook threads all interceptor inputs; grouping would obscure the call site"
)]
pub async fn intercept_llm_call_pre_execution(
    runtime: &baml_runtime::BamlRuntime,
    scope: &context::RuntimeScope,
    function_name: &str,
    params: &BamlMap<String, BamlValue>,
    ctx_manager: &RuntimeContextManager,
    interceptor_registry: &Arc<Mutex<InterceptorRegistry>>,
    env_vars: HashMap<String, String>,
    client_registry: Option<&ClientRegistry>,
    secret_keys_accessed: &[String],
    invocation: InvocationKind,
    effect_emitter: Option<&Arc<dyn EffectEmitter>>,
    collector: Option<&crate::baml_collector::BamlLLMCollector>,
    planning_step: Option<(&str, &str)>,
    function_tool_manifest: &FunctionToolManifest,
) -> Result<InterceptorDecision> {
    // Build the HTTP request to get LLM call details
    // This doesn't actually send the request, just builds it
    let http_request_result = runtime
        .build_request(
            function_name.to_string(),
            params,
            ctx_manager,
            None, // type_builder
            client_registry,
            env_vars,
            invocation.is_stream(),
        )
        .await;

    let http_request = match http_request_result {
        Ok(req) => req,
        Err(build_err) => {
            tracing::debug!(
                function = function_name,
                error = %build_err,
                "build_request failed; giving interceptors a chance to Substitute/Block"
            );
            // Request build failed (e.g. runtime-injected args like `session_context`
            // that are not in the declared BAML function signature, or missing env
            // secrets in the probe env map).  Give interceptors a chance to Substitute
            // or Block; if they Allow, the probe failure is non-fatal and the actual
            // call_function will proceed — it handles extra params differently.
            let mut minimal = minimal_llm_context(scope, function_name);
            let registry = interceptor_registry.lock().await;
            let decision = registry.intercept_llm_call(&minimal).await?;
            drop(registry);
            return match decision {
                InterceptorDecision::Substitute(value) => {
                    // When `build_request` fails we never reach the normal `start_llm` path, but
                    // `execute_function` still completes pending effects after Substitute. Without a
                    // stored token that completion is a no-op — provenance never sees the call.
                    // Emit start + pair with completion using stub client/model (minimal context has
                    // empty provider fields; normalizer requires non-unknown client/model).
                    if let (Some(emitter), Some(coll)) = (effect_emitter, collector) {
                        minimal.client = "openai-generic".to_string();
                        minimal.model = "stub".to_string();
                        if let Value::Object(ref mut m) = minimal.metadata {
                            m.insert("client".to_string(), json!("openai-generic"));
                            m.insert("model".to_string(), json!("stub"));
                        }
                        let tool_name_resolution =
                            match function_tool_manifest.tool_name_for_function(function_name) {
                                Some(name) => ToolNameResolution::FromManifest(name.to_string()),
                                None => ToolNameResolution::NotApplicable,
                            };
                        let effect_metadata =
                            llm_effect_metadata_from_context(&minimal, tool_name_resolution);
                        let context_id = minimal.runtime_scope.context_id().clone();
                        match emitter.start_llm(context_id.clone(), effect_metadata).await {
                            Ok(token) => coll.store_effect_token(context_id, token).await,
                            Err(e) => {
                                tracing::warn!(
                                    error = ?e,
                                    function = function_name,
                                    "build_request-fallback Substitute: failed to start LLM effect"
                                );
                            }
                        }
                    }
                    Ok(InterceptorDecision::Substitute(value))
                }
                InterceptorDecision::Block(msg) => Err(BamlRtError::BamlRuntime(format!(
                    "LLM call blocked by interceptor: {msg}"
                ))),
                // Allow: interceptors are happy to let the call proceed — the probe
                // failure is irrelevant.  call_function will make the actual LLM call.
                InterceptorDecision::Allow => Ok(InterceptorDecision::Allow),
            };
        }
    };

    // Extract LLM call context from the HTTP request
    let context =
        extract_context_from_http_request(scope, &http_request, function_name, planning_step)?;

    // Resolve tool name from the manifest (set at schema load time, no heuristics).
    let tool_name_resolution = match function_tool_manifest.tool_name_for_function(function_name) {
        Some(name) => ToolNameResolution::FromManifest(name.to_string()),
        None => ToolNameResolution::NotApplicable,
    };

    // Start effect and get token (type-safe start/complete pairing)
    let mut effect_metadata = llm_effect_metadata_from_context(&context, tool_name_resolution);
    if !secret_keys_accessed.is_empty() {
        let mut obj = effect_metadata
            .metadata
            .as_object()
            .cloned()
            .unwrap_or_default();
        let value = serde_json::to_value(secret_keys_accessed).unwrap_or_else(|e| {
            tracing::warn!(error = ?e, "Failed to serialize secret_keys_accessed for metadata");
            json!([])
        });
        obj.insert("secret_keys_accessed".to_string(), value);
        effect_metadata.metadata = Value::Object(obj);
    }
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
    let prompt_payload = normalized_prompt_payload(&context.prompt);
    let prompt_payload_bytes_vec = serde_json::to_vec(&prompt_payload).unwrap_or_default();
    let prompt_payload_bytes = if prompt_payload_bytes_vec.is_empty() {
        prompt_payload_bytes(&prompt_payload)
    } else {
        prompt_payload_bytes_vec.len()
    };
    let prompt_message_count = prompt_message_count(&prompt_payload);
    let (shared_prefix_bytes, previous_payload_bytes, cacheable_prefix_pct) =
        compute_and_store_prefix_cacheability(
            format!(
                "{}::{function_name}",
                context.runtime_scope.context_id().as_str()
            ),
            prompt_payload_bytes_vec,
        );

    tracing::debug!(
        client = context.client,
        model = context.model,
        function = function_name,
        prompt_payload_bytes,
        prompt_message_count,
        "LLM pre-execution telemetry"
    );
    tracing::debug!(
        function = function_name,
        context_id = %context.runtime_scope.context_id(),
        previous_payload_bytes,
        prompt_payload_bytes,
        shared_prefix_bytes,
        cacheable_prefix_pct = cacheable_prefix_pct,
        "LLM pre-execution prompt shared-prefix cacheability"
    );

    // Run interceptors
    let registry = interceptor_registry.lock().await;
    let decision = registry.intercept_llm_call(&context).await?;
    drop(registry);

    // Return the decision
    Ok(decision)
}

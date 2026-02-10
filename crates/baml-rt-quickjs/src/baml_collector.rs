//! BAML Collector implementation for LLM call interception
//!
//! This module implements a collector that hooks into BAML's execution lifecycle
//! to intercept LLM calls and route them through our interceptor system.
//!
//! BAML Collector implementation for LLM call interception
//!
//! This module implements a collector that hooks into BAML's execution lifecycle
//! to intercept LLM calls and route them through our interceptor system.

use baml_rt_core::Result;
use baml_rt_core::context;
use baml_rt_core::effects::{EffectEmitter, EffectStartToken, LlmKind};
use baml_rt_core::ids::ContextId;
use baml_rt_interceptor::{InterceptorRegistry, LLMCallContext};
use baml_runtime::tracingv2::storage::storage::Collector;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// BAML collector wrapper that tracks LLM calls via trace events
///
/// This wraps BAML's Collector to track function execution and extract
/// LLM call information from trace events for interceptor notifications.
pub struct BamlLLMCollector {
    inner: Arc<Collector>,
    interceptor_registry: Arc<Mutex<InterceptorRegistry>>,
    function_name: String,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    /// Store effect tokens keyed by context_id for type-safe completion
    effect_tokens: Arc<Mutex<HashMap<ContextId, EffectStartToken<LlmKind>>>>,
}

impl BamlLLMCollector {
    /// Create a new BAML LLM collector
    pub fn new(
        interceptor_registry: Arc<Mutex<InterceptorRegistry>>,
        function_name: String,
    ) -> Self {
        let inner = Arc::new(Collector::new(Some(format!(
            "llm_interceptor_{}",
            function_name
        ))));
        Self {
            inner,
            interceptor_registry,
            function_name,
            effect_emitter: None,
            effect_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Set the effect emitter (for effects-first liveness)
    pub fn set_effect_emitter(&mut self, emitter: Arc<dyn EffectEmitter>) {
        self.effect_emitter = Some(emitter);
    }

    /// Store an effect token for later completion (type-safe start/complete pairing).
    ///
    /// This allows the token to be passed from pre-execution interception to
    /// post-execution completion. Completion is never tied to BAML trace.
    pub async fn store_effect_token(
        &self,
        context_id: ContextId,
        token: EffectStartToken<LlmKind>,
    ) {
        let mut tokens = self.effect_tokens.lock().await;
        tokens.insert(context_id, token);
    }

    /// Complete all pending LLM effects using our token(s), clock, and outcome.
    /// Does not read BAML trace. Call this after call_function returns (success or failure).
    /// Trace is still used separately for provenance and interceptor notification.
    pub async fn complete_pending_effects(&self, success: bool, duration_ms: u64) {
        let emitter = match self.effect_emitter.as_ref() {
            Some(e) => e.clone(),
            None => return,
        };
        let tokens: Vec<EffectStartToken<LlmKind>> = {
            let mut guard = self.effect_tokens.lock().await;
            guard.drain().map(|(_, t)| t).collect()
        };
        for token in tokens {
            if let Err(e) = token
                .complete(emitter.as_ref(), None, duration_ms, success)
                .await
            {
                tracing::warn!(error = ?e, "Failed to complete LLM effect");
            }
        }
    }

    /// Get a reference to the inner BAML Collector
    pub fn as_collector(&self) -> Arc<Collector> {
        self.inner.clone()
    }

    /// Process trace events to extract LLM call information and notify interceptors
    ///
    /// This should be called after function execution to process collected trace events.
    /// Scope is explicit and must match the invocation scope for the function call.
    ///
    /// Note: This uses the last function log tracked by the collector.
    pub async fn process_trace_events(&self, scope: &context::RuntimeScope) -> Result<()> {
        // Get the last function log tracked by this collector
        // The collector tracks function IDs as they're executed when passed to call_function
        let mut function_log = match self.inner.last_function_log() {
            Some(log) => log,
            None => {
                // No function log found - this is fine, just means no LLM calls were made
                // or the function didn't trigger any LLM calls
                return Ok(());
            }
        };

        // Extract LLM calls from the function log
        let llm_calls = function_log.calls();

        // Process each LLM call for provenance and interceptor notification only.
        // Effect completion is done by complete_pending_effects(); we do not touch effect_tokens here.
        for call_kind in llm_calls {
            if let Some(llm_call) = call_kind.as_request() {
                let context = self.extract_context_from_llm_call(llm_call, scope);
                let duration_ms = llm_call.timing.duration_ms.unwrap_or(0) as u64;
                let result: Result<serde_json::Value> =
                    Ok(serde_json::to_value(llm_call).unwrap_or_else(|_| json!({})));
                let registry = self.interceptor_registry.lock().await;
                registry
                    .notify_llm_call_complete(&context, &result, duration_ms)
                    .await;
            }
            // TODO: Handle stream calls (call_kind.as_stream())
        }

        Ok(())
    }

    /// Extract LLM call context from an LLMCall
    fn extract_context_from_llm_call(
        &self,
        call: &baml_runtime::tracingv2::storage::storage::LLMCall,
        scope: &context::RuntimeScope,
    ) -> LLMCallContext {
        // Extract client/provider from the call
        let client = call.client_name.clone();
        let model = call.provider.clone(); // provider is the model/provider name

        // Extract prompt/messages from the request if available
        let prompt = if let Some(ref http_request) = call.request {
            serde_json::to_value(http_request.as_ref()).unwrap_or_else(|_| json!({}))
        } else {
            json!({})
        };

        LLMCallContext {
            client,
            model,
            function_name: self.function_name.clone(),
            runtime_scope: scope.clone(),
            prompt,
            metadata: json!({
                "usage": call.usage,
                "selected": call.selected,
                "agent_id": scope.agent_id().as_str(),
                "message_id": scope.message_id().as_str(),
                "task_id": scope.task_id_opt().map(|id| id.as_str()),
            }),
        }
    }
}

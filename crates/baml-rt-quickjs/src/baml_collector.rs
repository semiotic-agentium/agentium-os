//! BAML Collector implementation for LLM call interception
//!
//! This module implements a collector that hooks into BAML's execution lifecycle
//! to intercept LLM calls and route them through our interceptor system.
//!
//! BAML Collector implementation for LLM call interception
//!
//! This module implements a collector that hooks into BAML's execution lifecycle
//! to intercept LLM calls and route them through our interceptor system.

use std::{sync::Arc, time::Instant};

use baml_rt_core::{
    Outcome, Result,
    bus::{EffectEmitter, EffectStartToken, LlmKind, LlmUsage},
    context,
    ids::ContextId,
};
use baml_rt_interceptor::{InterceptorRegistry, LLMCallContext};
use baml_runtime::tracingv2::storage::storage::Collector;
use serde_json::json;
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
    /// Pending effect tokens to complete after function execution.
    effect_tokens: Arc<Mutex<Vec<EffectStartToken<LlmKind>>>>,
}

/// Handle to complete the LLM effect after tool/plan execution (e.g. so plan extraction failure emits PromptRejected).
///
/// Also fires `process_trace_events` so registered interceptors receive the
/// `on_llm_call_complete` notification. The deferred path in `execute_function`
/// skips `process_trace_events` to avoid premature notification; by the time the
/// manager calls `complete()` the outcome is known and it is safe to notify.
pub struct LLMCompletionHandle {
    collector: Arc<BamlLLMCollector>,
    start: Instant,
    scope: context::RuntimeScope,
    llm_result_payload: serde_json::Value,
}

impl LLMCompletionHandle {
    /// Complete the LLM effect with the given outcome. Call once after execute_tool_from_baml_result_or_value.
    /// When outcome is Failure, pass rejection_reason (e.g. plan extraction error) to emit PromptRejected in provenance.
    pub async fn complete(self, outcome: Outcome, rejection_reason: Option<String>) {
        // Notify interceptors (reads BAML trace; independent of the effect system).
        if let Err(e) = self.collector.process_trace_events(&self.scope).await {
            tracing::warn!(
                error = ?e,
                "LLMCompletionHandle: failed to process trace events for interceptor notification"
            );
        }
        let elapsed_ms = self.start.elapsed().as_millis() as u64;
        self.collector
            .complete_pending_effects(
                outcome,
                elapsed_ms,
                rejection_reason,
                Some(self.llm_result_payload),
            )
            .await;
    }
}

impl Drop for BamlLLMCollector {
    fn drop(&mut self) {
        let Some(emitter) = self.effect_emitter.clone() else {
            return;
        };
        let mut guard = match self.effect_tokens.try_lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if guard.is_empty() {
            return;
        }
        let leaked_tokens = std::mem::take(&mut *guard);
        drop(guard);
        tracing::warn!(
            leaked = leaked_tokens.len(),
            function = %self.function_name,
            "BamlLLMCollector dropped with pending LLM effect tokens; completing as cancellation"
        );
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    for token in leaked_tokens {
                        if let Err(e) = token
                            .complete(
                                emitter.as_ref(),
                                None,
                                Some(json!({ "error": "invocation_cancelled" })),
                                0,
                                Outcome::Failure,
                                Some("invocation_cancelled".to_string()),
                            )
                            .await
                        {
                            tracing::warn!(error = ?e, "Failed to complete leaked cancelled LLM effect");
                        }
                    }
                });
            }
            Err(_) => {
                tracing::error!(
                    leaked = leaked_tokens.len(),
                    "No Tokio runtime while dropping collector with pending tokens; cannot emit cancellation completions"
                );
            }
        }
    }
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
            effect_tokens: Arc::new(Mutex::new(Vec::new())),
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
        _context_id: ContextId,
        token: EffectStartToken<LlmKind>,
    ) {
        let mut tokens = self.effect_tokens.lock().await;
        tokens.push(token);
    }

    /// Complete all pending LLM effects using our token(s), clock, and outcome.
    /// Does not read BAML trace. Call this after call_function returns (success or failure).
    /// Trace is still used separately for provenance and interceptor notification.
    /// When outcome is Failure, pass `rejection_reason` (e.g. plan extraction error) to emit PromptRejected.
    pub async fn complete_pending_effects(
        &self,
        outcome: Outcome,
        duration_ms: u64,
        rejection_reason: Option<String>,
        llm_result_payload: Option<serde_json::Value>,
    ) {
        let emitter = match self.effect_emitter.as_ref() {
            Some(e) => e.clone(),
            None => return,
        };
        let usage = self.extract_usage_from_trace();
        let tokens: Vec<EffectStartToken<LlmKind>> = {
            let mut guard = self.effect_tokens.lock().await;
            std::mem::take(&mut *guard)
        };
        for token in tokens {
            if let Err(e) = token
                .complete(
                    emitter.as_ref(),
                    usage.clone(),
                    llm_result_payload.clone(),
                    duration_ms,
                    outcome,
                    rejection_reason.clone(),
                )
                .await
            {
                tracing::warn!(error = ?e, "Failed to complete LLM effect");
            }
        }
    }

    /// Create a completion handle so the caller can complete the LLM effect after tool/plan execution.
    /// Use when the executor returns successfully but tool/plan execution is done by the manager;
    /// the manager calls `handle.complete(Success, None)` or `handle.complete(Failure, Some(reason))`.
    pub fn completion_handle(
        collector: Arc<BamlLLMCollector>,
        start: Instant,
        scope: context::RuntimeScope,
        llm_result_payload: serde_json::Value,
    ) -> LLMCompletionHandle {
        LLMCompletionHandle {
            collector,
            start,
            scope,
            llm_result_payload,
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

    fn extract_usage_from_trace(&self) -> Option<LlmUsage> {
        let mut function_log = self.inner.last_function_log()?;
        let mut latest_known: Option<LlmUsage> = None;
        for call_kind in function_log.calls() {
            if let Some(llm_call) = call_kind.as_request() {
                let usage_value = serde_json::to_value(&llm_call.usage).ok()?;
                if let Some(parsed) = parse_llm_usage(&usage_value) {
                    latest_known = Some(parsed);
                }
            }
        }
        latest_known
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

fn parse_llm_usage(usage: &serde_json::Value) -> Option<LlmUsage> {
    if usage.is_null() {
        return None;
    }
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
                .and_then(serde_json::Value::as_object)
                .and_then(|details| details.get("cached_tokens"))
                .and_then(parse_u64_value)
        })
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(serde_json::Value::as_object)
                .and_then(|details| details.get("cached_tokens"))
                .and_then(parse_u64_value)
        });
    match (prompt_tokens, completion_tokens, total_tokens) {
        (Some(prompt), Some(completion), Some(total)) => Some(LlmUsage::Known {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cached_input_tokens,
        }),
        (Some(prompt), Some(completion), None) => Some(LlmUsage::Known {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt.saturating_add(completion),
            cached_input_tokens,
        }),
        _ => None,
    }
}

fn parse_u64_value(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
}

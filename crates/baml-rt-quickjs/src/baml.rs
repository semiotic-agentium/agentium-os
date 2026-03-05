//! BAML runtime wrapper and function execution.
//!
//! [`BamlRuntimeManager`] owns the function registry, tool registry, and session
//! state. Tool call and plan extraction live in [`tool_extraction`]; session
//! open/send/next/finish/abort and plan execution remain here and use
//! scope-from-token for attribution.

mod tool_extraction;
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fs,
    path::Path,
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Outcome, Result, SessionLifecycleError,
    bus::{EffectEmitter, EffectEvent, EffectStartToken, ToolEffectMetadata, ToolKind},
    context,
    correlation::current_correlation_id,
    types::FunctionSignature,
};
use baml_rt_interceptor::{InterceptorRegistry, ToolCallContext};
use baml_rt_observability::metrics;
use baml_rt_tools::{
    ToolFunctionMetadataExport, ToolRegistry as ConcreteToolRegistry, ToolSessionId, ToolStep,
};
use baml_types::BamlValue;
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;
pub(crate) use tool_extraction::{
    ToolSessionOp, ToolSessionPlan, extract_tool_call, extract_tool_session_plan,
    normalize_plan_input, resolve_tool_name_from_input_with_registry,
    resolve_tool_name_from_plan_type_with_registry,
};

use crate::{
    baml_execution::{BamlExecutor, ConversationContextProvider, ParseRetryPolicy},
    traits::{BamlFunctionExecutor, SchemaLoader},
};

// Helper function to build metadata map with correlation/message/task/agent ids.
fn build_metadata_map_with_phase(
    scope: &context::RuntimeScope,
    phase: Option<&'static str>,
) -> Value {
    let mut map = serde_json::Map::new();
    if let Some(correlation_id) = current_correlation_id() {
        map.insert(
            "correlation_id".to_string(),
            Value::String(correlation_id.to_string()),
        );
    }
    map.insert(
        "message_id".to_string(),
        Value::String(scope.message_id().as_str().to_owned()),
    );
    if let Some(task_id) = scope.task_id_opt() {
        map.insert(
            "task_id".to_string(),
            Value::String(task_id.as_str().to_owned()),
        );
    }
    map.insert(
        "agent_id".to_string(),
        Value::String(scope.agent_id().as_str().to_owned()),
    );
    if let Some(phase) = phase {
        map.insert("phase".to_string(), Value::String(phase.to_string()));
    }
    Value::Object(map)
}

// BAML executes in Rust. We will implement execution of BAML functions
// in Rust, then map those function calls to QuickJS so JavaScript can invoke them.
// use baml;

/// Helper function for creating an empty open_input value.
///
/// This centralizes the pattern of using an empty JSON object as the default
/// open_input when none is provided.
fn empty_open_input() -> Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn tool_session_trace_enabled() -> bool {
    std::env::var("BAML_TRACE_TOOL_SESSION").is_ok()
}

fn tool_session_trace(message: &str) {
    if tool_session_trace_enabled() {
        tracing::trace!(message = %message, "[tool-session-trace]");
    }
}

fn completion_error_from(err: &BamlRtError) -> BamlRtError {
    match err {
        BamlRtError::SessionLifecycle(lifecycle) => {
            BamlRtError::SessionLifecycle(lifecycle.clone())
        }
        _ => BamlRtError::InvalidArgument(err.to_string()),
    }
}

/// Extract target.agent_package from open_input for delegation tools.
/// Supports system/internal_a2a, system/a2a, and support/a2aRelay (same Open shape).
fn extract_delegation_target_from_open_input(
    tool_name: &str,
    open_input: &Value,
) -> Option<String> {
    const A2A_TOOLS: [&str; 3] = ["system/internal_a2a", "system/a2a", "support/a2aRelay"];
    if !A2A_TOOLS.contains(&tool_name) {
        return None;
    }
    let target = open_input
        .get("target")
        .and_then(|t| t.get("agent_package"))
        .and_then(Value::as_str)?;
    Some(target.to_string())
}

/// Map from BAML function name to session plan type name (from builder-generated session_plan_functions.json).
/// When set, the runtime resolves the tool from the invoking function name instead of requiring __type in the JSON.
pub type SessionPlanFunctionsMap = std::collections::HashMap<String, String>;

/// Manages the BAML runtime and function registry
pub struct BamlRuntimeManager {
    function_registry: HashMap<String, FunctionSignature>,
    pub(crate) executor: Option<BamlExecutor>,
    tool_registry: Arc<ConcreteToolRegistry>,
    /// Builder-generated map: function name → session plan type. Lets the runtime resolve tool from the call site.
    session_plan_functions: Option<SessionPlanFunctionsMap>,
    interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    tool_session_scopes: Arc<TokioMutex<HashMap<ToolSessionId, ToolSessionScope>>>,
    tool_session_states: Arc<TokioMutex<HashMap<ToolSessionId, ToolCallSessionState>>>,
    /// Tokens for ToolStarted emitted in tool_session_send; completed in tool_session_next/finish/abort. Shared across handle() calls.
    tool_session_effect_tokens: Arc<TokioMutex<HashMap<ToolSessionId, EffectStartToken<ToolKind>>>>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    conversation_context_provider: Option<Arc<dyn ConversationContextProvider>>,
    pending_parse_retry_policy: Option<ParseRetryPolicy>,
}

#[derive(Debug, Clone)]
struct ToolCallSessionState {
    context: ToolCallContext,
    start: Instant,
}

#[derive(Debug, Clone)]
struct ToolSessionScope {
    tool_name: String,
    scope: context::RuntimeScope,
    /// Open-phase input; used to extract delegation_target for system/internal_a2a.
    open_input: serde_json::Value,
}

#[derive(Clone)]
pub(crate) struct ToolExecutionHandle {
    tool_registry: Arc<ConcreteToolRegistry>,
    interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
}

impl ToolExecutionHandle {
    pub(crate) async fn execute_tool(
        &self,
        scope: &context::RuntimeScope,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        self.execute_tool_inner(scope.clone(), name, args).await
    }

    pub(crate) async fn execute_tool_from_baml_result(
        &self,
        scope: &context::RuntimeScope,
        baml_result: Value,
    ) -> Result<Value> {
        let call = extract_tool_call(&baml_result)?.ok_or_else(|| {
            BamlRtError::InvalidArgument("No tool call found in result".to_string())
        })?;
        let tool_name =
            resolve_tool_name_from_input_with_registry(&self.tool_registry, &call.args)?;
        self.execute_tool(scope, &tool_name, call.args).await
    }

    async fn execute_tool_inner(
        &self,
        scope: context::RuntimeScope,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        use std::time::Instant;

        use baml_rt_interceptor::ToolCallContext;

        let start = Instant::now();
        let context_id = scope.context_id().clone();
        let agent_id = scope.agent_id().clone();
        let metadata = build_metadata_map_with_phase(&scope, Some("execute"));

        // Build context for interceptors
        let context = ToolCallContext {
            tool_name: name.to_string(),
            function_name: None, // Could be enhanced to track which function called this tool
            args: args.clone(),
            metadata: metadata.clone(),
            runtime_scope: scope.clone(),
            delegation_target: None,
        };

        // Start effect and get token (type-safe start/complete pairing)
        let effect_metadata = ToolEffectMetadata {
            tool_name: name.to_string(),
            function_name: None,
            args: args.clone(),
            metadata: metadata.clone(),
            delegation_target: None,
        };
        let effect_token = if let Some(emitter) = self.effect_emitter.as_ref() {
            match emitter
                .start_tool(context_id.clone(), effect_metadata)
                .await
            {
                Ok(token) => Some(token),
                Err(e) => {
                    tracing::warn!(error = ?e, "Failed to start tool effect");
                    None
                }
            }
        } else {
            None
        };

        // Run interceptors before execution
        let interceptor_registry = self.interceptor_registry.lock().await;
        let interceptor_result = interceptor_registry.intercept_tool_call(&context).await;
        drop(interceptor_registry);
        if let Err(e) = interceptor_result {
            if let Some(token) = effect_token
                && let Some(emitter) = self.effect_emitter.as_ref()
            {
                let duration_ms = start.elapsed().as_millis() as u64;
                if let Err(complete_err) = token
                    .complete(emitter.as_ref(), duration_ms, Outcome::Failure)
                    .await
                {
                    tracing::warn!(
                        error = ?complete_err,
                        "Failed to complete tool effect after interceptor denied"
                    );
                }
            }
            return Err(e);
        }
        let _decision = match interceptor_result {
            Ok(d) => d,
            Err(_) => unreachable!("Err branch returned above"),
        };

        // Handle interceptor decision
        // If we get here, the decision is Allow (blocking would have returned Err)
        let final_args = args;

        // Execute the tool (context_id and agent_id from scope)
        let result = self
            .tool_registry
            .execute(name, final_args, &context_id, &agent_id)
            .await;

        // Calculate duration
        let duration = start.elapsed();
        let duration_ms = duration.as_millis() as u64;
        let outcome = Outcome::from(result.is_ok());

        // Complete effect using token (type-safe: token consumed, cannot double-complete)
        if let Some(token) = effect_token
            && let Some(emitter) = self.effect_emitter.as_ref()
            && let Err(e) = token.complete(emitter.as_ref(), duration_ms, outcome).await
        {
            tracing::warn!(error = ?e, "Failed to complete tool effect");
        }

        // Notify interceptors of completion
        let interceptor_registry = self.interceptor_registry.lock().await;
        interceptor_registry
            .notify_tool_call_complete(&context, &result, duration_ms)
            .await;
        drop(interceptor_registry);

        let metric_result = if result.is_ok() { "success" } else { "error" };
        metrics::record_tool_invocation(name, metric_result, duration);

        result
    }
}

/// Handle for tool session operations without holding the full runtime lock.
/// Use this when session operations may await and another task needs the runtime (e.g. A2A dispatcher).
#[derive(Clone)]
pub struct ToolSessionExecutionHandle {
    tool_registry: Arc<ConcreteToolRegistry>,
    interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    tool_session_scopes: Arc<TokioMutex<HashMap<ToolSessionId, ToolSessionScope>>>,
    tool_session_states: Arc<TokioMutex<HashMap<ToolSessionId, ToolCallSessionState>>>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    /// Token for ToolStarted emitted in tool_session_send; completed in tool_session_next (or on error in send).
    tool_session_effect_tokens: Arc<TokioMutex<HashMap<ToolSessionId, EffectStartToken<ToolKind>>>>,
}

impl ToolSessionExecutionHandle {
    /// Open a tool session with explicit runtime scope.
    pub async fn open_tool_session(
        &self,
        scope: &context::RuntimeScope,
        tool_name: &str,
        open_input: serde_json::Value,
    ) -> Result<ToolSessionId> {
        let scope = scope.clone();
        let context_id = scope.context_id().clone();
        let agent_id = scope.agent_id().clone();

        let start = Instant::now();
        let metadata = build_metadata_map_with_phase(&scope, Some("open"));
        let delegation_target = extract_delegation_target_from_open_input(tool_name, &open_input);
        let context = ToolCallContext {
            tool_name: tool_name.to_string(),
            function_name: None,
            args: open_input.clone(),
            metadata,
            runtime_scope: scope.clone(),
            delegation_target,
        };

        tracing::info!(
            tool_name = tool_name,
            context_id = %context_id,
            "Tool session open: start"
        );

        // Record tool call start for "open" (session-based: open + execute = 2 invocations per request)
        let interceptor_registry = self.interceptor_registry.lock().await;
        let _ = interceptor_registry.intercept_tool_call(&context).await?;
        drop(interceptor_registry);

        let result = self
            .tool_registry
            .open_session(tool_name, open_input.clone(), &context_id, &agent_id)
            .await;
        let duration_ms = start.elapsed().as_millis() as u64;
        let completion_result: Result<Value> = match &result {
            Ok(_) => Ok(Value::Null),
            Err(e) => Err(BamlRtError::InvalidArgument(e.to_string())),
        };
        let interceptor_registry = self.interceptor_registry.lock().await;
        interceptor_registry
            .notify_tool_call_complete(&context, &completion_result, duration_ms)
            .await;
        drop(interceptor_registry);

        if let Err(ref e) = result {
            tracing::warn!(
                tool_name = tool_name,
                context_id = %context_id,
                error = ?e,
                "Tool session open: error"
            );
        }

        let session_id = result?;
        if tool_session_trace_enabled() {
            let scope_len = self.tool_session_scopes.lock().await.len();
            tool_session_trace(&format!(
                "open ok: session_id={}, context_id={}, scopes={}",
                session_id, context_id, scope_len
            ));
        }
        tracing::info!(
            tool_name = tool_name,
            context_id = %context_id,
            session_id = %session_id,
            "Tool session open: ok"
        );
        let mut scopes = self.tool_session_scopes.lock().await;
        scopes.insert(
            session_id.clone(),
            ToolSessionScope {
                tool_name: tool_name.to_string(),
                scope,
                open_input: open_input.clone(),
            },
        );
        if tool_session_trace_enabled() {
            tool_session_trace(&format!(
                "open inserted: session_id={}, scopes={}",
                session_id,
                scopes.len()
            ));
        }
        Ok(session_id)
    }

    /// Find an existing open session for this context and tool, if any.
    /// Used when the coordinator returns a continuation plan (e.g. [Send, Next]) so we reuse
    /// the same session instead of auto-inserting Open and creating a new one.
    ///
    /// **Task-aware:** When the scope is `TaskScope`, the match also requires the same `task_id`
    /// to prevent parallel child branches from hijacking each other's sessions under the same
    /// `context_id`. For `MessageScope`, existing `(context_id, tool_name)` behavior is preserved.
    async fn find_existing_session_for_scope_and_tool(
        &self,
        scope: &context::RuntimeScope,
        tool_name: &str,
    ) -> Option<ToolSessionId> {
        let context_id = scope.context_id();
        let task_id = scope.task_id_opt();
        let scopes = self.tool_session_scopes.lock().await;
        for (sid, session_scope) in scopes.iter() {
            if session_scope.tool_name != tool_name {
                continue;
            }
            if session_scope.scope.context_id() != context_id {
                continue;
            }
            match task_id {
                // TaskScope: must also match task_id to prevent cross-branch reuse
                Some(tid) => {
                    if session_scope.scope.task_id_opt() == Some(tid) {
                        return Some(sid.clone());
                    }
                }
                // MessageScope: existing behavior — match by context_id only
                None => return Some(sid.clone()),
            }
        }
        None
    }

    /// Collect all session IDs whose scope matches this context_id (for teardown).
    pub async fn collect_session_ids_for_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
    ) -> Vec<ToolSessionId> {
        let scopes = self.tool_session_scopes.lock().await;
        scopes
            .iter()
            .filter(|(_, s)| s.scope.context_id() == context_id)
            .map(|(sid, _)| sid.clone())
            .collect()
    }

    /// Collect session IDs whose scope matches both `context_id` AND `task_id`.
    /// For task-scoped teardown: only targets sessions belonging to a specific child branch,
    /// leaving sibling branches' sessions untouched.
    pub async fn collect_session_ids_for_task_scope(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        task_id: &baml_rt_core::ids::TaskId,
    ) -> Vec<ToolSessionId> {
        let scopes = self.tool_session_scopes.lock().await;
        scopes
            .iter()
            .filter(|(_, s)| {
                s.scope.context_id() == context_id && s.scope.task_id_opt() == Some(task_id)
            })
            .map(|(sid, _)| sid.clone())
            .collect()
    }

    pub async fn tool_session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()> {
        use baml_rt_interceptor::InterceptorDecision;

        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            if tool_session_trace_enabled() {
                tool_session_trace(&format!(
                    "send lookup: session_id={}, scopes={}",
                    session_id,
                    scopes.len()
                ));
            }
            scopes.get(session_id).cloned()
        };
        let session_scope = session_scope.ok_or_else(|| {
            if tool_session_trace_enabled() {
                if let Ok(scopes) = self.tool_session_scopes.try_lock() {
                    let known_ids: Vec<String> = scopes.keys().map(|id| id.to_string()).collect();
                    tool_session_trace(&format!(
                        "send missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                        session_id,
                        scopes.len(),
                        known_ids
                    ));
                } else {
                    tool_session_trace(&format!(
                        "send missing scope: session_id={}, scopes=locked",
                        session_id
                    ));
                }
            }
            BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                session_id: session_id.to_string(),
            })
        })?;

        tracing::info!(
            session_id = %session_id,
            tool_name = %session_scope.tool_name,
            context_id = %session_scope.scope.context_id(),
            "Tool session send: start"
        );

        let run = || async {
            let start = Instant::now();
            let metadata = build_metadata_map_with_phase(&session_scope.scope, Some("send"));

            let delegation_target = extract_delegation_target_from_open_input(
                &session_scope.tool_name,
                &session_scope.open_input,
            );

            let context = ToolCallContext {
                tool_name: session_scope.tool_name.clone(),
                function_name: None,
                args: input.clone(),
                metadata,
                runtime_scope: session_scope.scope.clone(),
                delegation_target,
            };

            let interceptor_registry = self.interceptor_registry.lock().await;
            let _decision: InterceptorDecision =
                interceptor_registry.intercept_tool_call(&context).await?;
            drop(interceptor_registry);

            // Keep a single in-flight state/token per session. This avoids replacing an
            // existing EffectStartToken on duplicate send() calls (which would leak/panic).
            let is_new_send = {
                let mut states = self.tool_session_states.lock().await;
                match states.entry(session_id.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(ToolCallSessionState {
                            context: context.clone(),
                            start,
                        });
                        true
                    }
                    Entry::Occupied(_) => false,
                }
            };

            // Emit ToolStarted so relay can send TASK_STATE_WORKING (session path does not use execute_tool).
            if is_new_send {
                if let Some(emitter) = self.effect_emitter.as_ref() {
                    let mut effect_metadata = ToolEffectMetadata {
                        tool_name: session_scope.tool_name.clone(),
                        function_name: None,
                        args: input.clone(),
                        metadata: context.metadata.clone(),
                        delegation_target: None,
                    };
                    if let Some(ref target) = context.delegation_target {
                        effect_metadata.delegation_target = Some(target.clone());
                    }
                    if let Ok(token) = emitter
                        .start_tool(session_scope.scope.context_id().clone(), effect_metadata)
                        .await
                    {
                        let mut tokens = self.tool_session_effect_tokens.lock().await;
                        tokens.insert(session_id.clone(), token);
                    }
                }
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    "Tool session send: reusing in-flight state/token"
                );
            }

            let result = self.tool_registry.session_send(session_id, input).await;

            if result.is_err() {
                let state = {
                    let mut states = self.tool_session_states.lock().await;
                    states.remove(session_id)
                };
                let duration_ms = state
                    .as_ref()
                    .map(|state| state.start.elapsed().as_millis() as u64)
                    .unwrap_or_else(|| start.elapsed().as_millis() as u64);
                if let Some(token) = self
                    .tool_session_effect_tokens
                    .lock()
                    .await
                    .remove(session_id)
                    && let Some(emitter) = self.effect_emitter.as_ref()
                {
                    let _ = token
                        .complete(emitter.as_ref(), duration_ms, Outcome::Failure)
                        .await;
                }
                let completion_result: Result<Value> = match &result {
                    Ok(_) => Ok(Value::Null),
                    Err(err) => Err(completion_error_from(err)),
                };
                let interceptor_registry = self.interceptor_registry.lock().await;
                if let Some(state) = state.as_ref() {
                    interceptor_registry
                        .notify_tool_call_complete(&state.context, &completion_result, duration_ms)
                        .await;
                } else {
                    interceptor_registry
                        .notify_tool_call_complete(&context, &completion_result, duration_ms)
                        .await;
                }
                drop(interceptor_registry);
                let mut scopes = self.tool_session_scopes.lock().await;
                scopes.remove(session_id);
            }

            result
        };

        let result = run().await;
        if let Err(ref e) = result {
            tracing::warn!(
                session_id = %session_id,
                tool_name = %session_scope.tool_name,
                context_id = %session_scope.scope.context_id(),
                error = ?e,
                "Tool session send: error"
            );
        } else {
            tracing::info!(
                session_id = %session_id,
                tool_name = %session_scope.tool_name,
                context_id = %session_scope.scope.context_id(),
                "Tool session send: ok"
            );
        }
        result
    }

    pub async fn tool_session_next(&self, session_id: &ToolSessionId) -> Result<ToolStep> {
        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            scopes.get(session_id).cloned()
        };

        let run = || async {
            tracing::info!(session_id = %session_id, "Tool session next: start");
            let result = self.tool_registry.session_next(session_id).await;

            let completion = match &result {
                Ok(ToolStep::Done { output }) => Some(Ok(output.clone().unwrap_or(Value::Null))),
                Ok(ToolStep::Error { error }) => Some(Err(BamlRtError::InvalidArgument(format!(
                    "Tool failure ({:?}): {}",
                    error.kind, error.message
                )))),
                Err(err) => Some(Err(completion_error_from(err))),
                _ => None,
            };

            if let Some(completion_result) = completion
                && let Some(state) = {
                    let mut states = self.tool_session_states.lock().await;
                    states.remove(session_id)
                }
            {
                // Do not remove scopes here; Finish/Abort is responsible for cleanup.
                let duration_ms = state.start.elapsed().as_millis() as u64;
                if let Some(token) = self
                    .tool_session_effect_tokens
                    .lock()
                    .await
                    .remove(session_id)
                    && let Some(emitter) = self.effect_emitter.as_ref()
                {
                    let outcome = if completion_result.is_ok() {
                        Outcome::Success
                    } else {
                        Outcome::Failure
                    };
                    let _ = token.complete(emitter.as_ref(), duration_ms, outcome).await;
                }
                let interceptor_registry = self.interceptor_registry.lock().await;
                interceptor_registry
                    .notify_tool_call_complete(&state.context, &completion_result, duration_ms)
                    .await;
                drop(interceptor_registry);
                let metric_result = if completion_result.is_ok() {
                    "success"
                } else {
                    "error"
                };
                metrics::record_tool_invocation(
                    &state.context.tool_name,
                    metric_result,
                    state.start.elapsed(),
                );
            }

            result
        };

        let _scope = session_scope
            .ok_or_else(|| {
                if tool_session_trace_enabled() {
                    if let Ok(scopes) = self.tool_session_scopes.try_lock() {
                        let known_ids: Vec<String> =
                            scopes.keys().map(|id| id.to_string()).collect();
                        tool_session_trace(&format!(
                            "next missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                            session_id,
                            scopes.len(),
                            known_ids
                        ));
                    } else {
                        tool_session_trace(&format!(
                            "next missing scope: session_id={}, scopes=locked",
                            session_id
                        ));
                    }
                }
                BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                    session_id: session_id.to_string(),
                })
            })?
            .scope;
        let result = run().await;
        if let Ok(ref step) = result {
            tracing::info!(
                session_id = %session_id,
                step = ?step,
                "Tool session next: ok"
            );
        } else if let Err(ref e) = result {
            tracing::warn!(session_id = %session_id, error = ?e, "Tool session next: error");
        }
        result
    }

    pub async fn tool_session_finish(&self, session_id: &ToolSessionId) -> Result<()> {
        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            scopes.get(session_id).cloned()
        };

        let run = || async {
            tracing::info!(session_id = %session_id, "Tool session finish: start");
            let result = self.tool_registry.session_finish(session_id).await;

            if let Some(state) = {
                let mut states = self.tool_session_states.lock().await;
                states.remove(session_id)
            } {
                let duration_ms = state.start.elapsed().as_millis() as u64;
                if let Some(token) = self
                    .tool_session_effect_tokens
                    .lock()
                    .await
                    .remove(session_id)
                    && let Some(emitter) = self.effect_emitter.as_ref()
                {
                    let outcome = if result.is_ok() {
                        Outcome::Success
                    } else {
                        Outcome::Failure
                    };
                    let _ = token.complete(emitter.as_ref(), duration_ms, outcome).await;
                }
                let completion_result: Result<Value> = match &result {
                    Ok(_) => Ok(Value::Null),
                    Err(err) => Err(completion_error_from(err)),
                };
                let interceptor_registry = self.interceptor_registry.lock().await;
                interceptor_registry
                    .notify_tool_call_complete(&state.context, &completion_result, duration_ms)
                    .await;
                drop(interceptor_registry);
                let metric_result = if completion_result.is_ok() {
                    "success"
                } else {
                    "error"
                };
                metrics::record_tool_invocation(
                    &state.context.tool_name,
                    metric_result,
                    state.start.elapsed(),
                );
            }

            // Always remove from scopes so teardown (close_sessions_for_context) does not leak
            // sessions that were only opened and never had send/next (no state).
            let mut scopes = self.tool_session_scopes.lock().await;
            scopes.remove(session_id);

            result
        };

        let _scope = session_scope
            .ok_or_else(|| {
                if tool_session_trace_enabled() {
                    if let Ok(scopes) = self.tool_session_scopes.try_lock() {
                        let known_ids: Vec<String> =
                            scopes.keys().map(|id| id.to_string()).collect();
                        tool_session_trace(&format!(
                            "finish missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                            session_id,
                            scopes.len(),
                            known_ids
                        ));
                    } else {
                        tool_session_trace(&format!(
                            "finish missing scope: session_id={}, scopes=locked",
                            session_id
                        ));
                    }
                }
                BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                    session_id: session_id.to_string(),
                })
            })?
            .scope;
        let result = run().await;
        if let Err(ref e) = result {
            tracing::warn!(session_id = %session_id, error = ?e, "Tool session finish: error");
        } else {
            tracing::info!(session_id = %session_id, "Tool session finish: ok");
        }
        result
    }

    pub async fn tool_session_abort(
        &self,
        session_id: &ToolSessionId,
        reason: Option<String>,
    ) -> Result<()> {
        let session_scope = {
            let scopes = self.tool_session_scopes.lock().await;
            scopes.get(session_id).cloned()
        };

        let run = || async {
            tracing::info!(session_id = %session_id, reason = ?reason, "Tool session abort: start");
            let result = self
                .tool_registry
                .session_abort(session_id, reason.clone())
                .await;

            if let Some(state) = {
                let mut states = self.tool_session_states.lock().await;
                states.remove(session_id)
            } {
                let duration_ms = state.start.elapsed().as_millis() as u64;
                if let Some(token) = self
                    .tool_session_effect_tokens
                    .lock()
                    .await
                    .remove(session_id)
                    && let Some(emitter) = self.effect_emitter.as_ref()
                {
                    let _ = token
                        .complete(emitter.as_ref(), duration_ms, Outcome::Failure)
                        .await;
                }
                let mut scopes = self.tool_session_scopes.lock().await;
                scopes.remove(session_id);
                let completion_result = Err(BamlRtError::InvalidArgument(
                    reason.unwrap_or_else(|| "Tool session aborted".to_string()),
                ));
                let interceptor_registry = self.interceptor_registry.lock().await;
                interceptor_registry
                    .notify_tool_call_complete(&state.context, &completion_result, duration_ms)
                    .await;
                drop(interceptor_registry);
                metrics::record_tool_invocation(
                    &state.context.tool_name,
                    "error",
                    state.start.elapsed(),
                );
            }

            result
        };

        let _scope = session_scope
            .ok_or_else(|| {
                if tool_session_trace_enabled() {
                    if let Ok(scopes) = self.tool_session_scopes.try_lock() {
                        let known_ids: Vec<String> =
                            scopes.keys().map(|id| id.to_string()).collect();
                        tool_session_trace(&format!(
                            "abort missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                            session_id,
                            scopes.len(),
                            known_ids
                        ));
                    } else {
                        tool_session_trace(&format!(
                            "abort missing scope: session_id={}, scopes=locked",
                            session_id
                        ));
                    }
                }
                BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                    session_id: session_id.to_string(),
                })
            })?
            .scope;
        let result = run().await;
        if let Err(ref e) = result {
            tracing::warn!(session_id = %session_id, error = ?e, "Tool session abort: error");
        } else {
            tracing::info!(session_id = %session_id, "Tool session abort: ok");
        }
        result
    }
}

impl BamlRuntimeManager {
    /// Create a new BAML runtime manager
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing BAML runtime manager");

        Ok(Self {
            function_registry: HashMap::new(),
            executor: None,
            tool_registry: Arc::new(ConcreteToolRegistry::new()),
            session_plan_functions: None,
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(TokioMutex::new(HashMap::new())),
            tool_session_states: Arc::new(TokioMutex::new(HashMap::new())),
            tool_session_effect_tokens: Arc::new(TokioMutex::new(HashMap::new())),
            effect_emitter: None,
            conversation_context_provider: None,
            pending_parse_retry_policy: None,
        })
    }

    /// Set the effect emitter (for effects-first liveness).
    /// If the executor is already loaded, forwards the emitter to it so LLM/tool effects
    /// are emitted and the promise-polling loop can use effect-gated timeouts.
    pub fn set_effect_emitter(&mut self, emitter: Arc<dyn EffectEmitter>) {
        self.effect_emitter = Some(emitter.clone());
        if let Some(executor) = self.executor.as_mut() {
            executor.set_effect_emitter(emitter);
        }
    }

    pub fn set_conversation_context_provider(
        &mut self,
        provider: Arc<dyn ConversationContextProvider>,
    ) {
        self.conversation_context_provider = Some(provider.clone());
        if let Some(executor) = self.executor.as_mut() {
            executor.set_conversation_context_provider(provider);
        }
    }

    /// Set the policy for retrying BAML calls on parse failure. May be called before or after [`load_schema`](Self::load_schema).
    /// Use `ParseRetryPolicy { max_attempts: 1, .. }` in tests to avoid retry delay.
    pub fn set_parse_retry_policy(&mut self, policy: ParseRetryPolicy) {
        self.pending_parse_retry_policy = Some(policy.clone());
        if let Some(executor) = self.executor.as_mut() {
            executor.set_parse_retry_policy(policy);
        }
    }

    /// Set the session plan function map (for tests). Normally loaded from `session_plan_functions.json` when loading BAML.
    pub fn set_session_plan_functions(&mut self, map: Option<SessionPlanFunctionsMap>) {
        self.session_plan_functions = map;
    }

    pub(crate) fn tool_execution_handle(&self) -> ToolExecutionHandle {
        ToolExecutionHandle {
            tool_registry: self.tool_registry.clone(),
            interceptor_registry: self.interceptor_registry.clone(),
            effect_emitter: self.effect_emitter.clone(),
        }
    }

    /// Returns a handle for session operations. Use this to avoid holding the runtime lock across awaits.
    pub fn tool_session_handle(&self) -> ToolSessionExecutionHandle {
        ToolSessionExecutionHandle {
            tool_registry: self.tool_registry.clone(),
            interceptor_registry: self.interceptor_registry.clone(),
            tool_session_scopes: self.tool_session_scopes.clone(),
            tool_session_states: self.tool_session_states.clone(),
            effect_emitter: self.effect_emitter.clone(),
            tool_session_effect_tokens: self.tool_session_effect_tokens.clone(),
        }
    }

    /// Check if a schema is loaded
    pub fn is_schema_loaded(&self) -> bool {
        self.executor.is_some()
    }

    /// Load a compiled BAML schema/configuration
    ///
    /// This loads the BAML IL (Intermediate Language) from the baml_src directory
    /// and registers all available functions.
    ///
    /// The schema_path should point to the baml_src directory.
    pub fn load_schema(&mut self, schema_path: &str) -> Result<()> {
        tracing::info!(schema_path = schema_path, "Loading BAML IL");

        use std::path::Path;

        // Find project root
        let schema_path_obj = Path::new(schema_path);
        let project_root = if schema_path_obj.is_file() {
            schema_path_obj.parent().and_then(|p| p.parent())
        } else if schema_path_obj.file_name() == Some(std::ffi::OsStr::new("baml_src")) {
            schema_path_obj.parent()
        } else {
            Some(schema_path_obj)
        }
        .ok_or_else(|| BamlRtError::InvalidArgument("Invalid schema path".to_string()))?;

        let baml_src_dir = project_root.join("baml_src");
        if !baml_src_dir.exists() {
            return Err(BamlRtError::BamlRuntime(
                "baml_src directory not found".to_string(),
            ));
        }

        // Load BAML IL into executor (pass tool registry)
        let tool_registry_clone = self.tool_registry.clone();
        let mut executor = BamlExecutor::load_il(&baml_src_dir, tool_registry_clone)?;

        // Set effect emitter if available
        if let Some(ref emitter) = self.effect_emitter {
            executor.set_effect_emitter(emitter.clone());
        }
        if let Some(ref provider) = self.conversation_context_provider {
            executor.set_conversation_context_provider(provider.clone());
        }
        if let Some(policy) = self.pending_parse_retry_policy.take() {
            executor.set_parse_retry_policy(policy);
        }

        // Discover functions from the BAML runtime
        let function_names = executor.list_functions();
        for func_name in function_names {
            // Register function signature
            self.function_registry.insert(
                func_name.clone(),
                FunctionSignature {
                    name: func_name.clone(),
                    input_types: vec![],
                    output_type: baml_rt_core::types::BamlType::String,
                },
            );
        }

        self.executor = Some(executor);

        // Load builder-generated session plan function map so we can resolve tool from the invoking function name (no __type in prompt required).
        let manifest_path = project_root.join("session_plan_functions.json");
        self.session_plan_functions = if manifest_path.exists() {
            match std::fs::read_to_string(&manifest_path) {
                Ok(s) => serde_json::from_str(&s).ok(),
                Err(e) => {
                    tracing::warn!(path = %manifest_path.display(), error = %e, "Could not read session_plan_functions.json");
                    None
                }
            }
        } else {
            None
        };

        tracing::info!(
            function_count = self.function_registry.len(),
            session_plan_manifest = self
                .session_plan_functions
                .as_ref()
                .map(|m| m.len())
                .unwrap_or(0),
            "Loaded BAML IL"
        );

        Ok(())
    }

    /// Get the signature of a function by name
    pub fn get_function_signature(&self, name: &str) -> Option<&FunctionSignature> {
        self.function_registry.get(name)
    }

    /// Execute a BAML function with the given arguments
    ///
    /// This is the main entry point for executing BAML functions.
    /// It validates the function exists and delegates to the executor.
    pub async fn invoke_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let correlation_id = current_correlation_id();
        if let Some(correlation_id) = correlation_id.as_ref().map(|id| id.as_str()) {
            tracing::debug!(
                function = function_name,
                args = ?args,
                correlation_id = correlation_id,
                "Invoking BAML function"
            );
        } else {
            tracing::debug!(
                function = function_name,
                args = ?args,
                "Invoking BAML function"
            );
        }

        // Verify function exists
        let _signature = self
            .function_registry
            .get(function_name)
            .ok_or_else(|| BamlRtError::FunctionNotFound(function_name.to_string()))?;

        // Execute the BAML function using the executor
        let executor = self
            .executor
            .as_ref()
            .ok_or_else(|| BamlRtError::BamlRuntime("BAML runtime not loaded".to_string()))?;

        // Pass tool registry and interceptor registry to executor
        let interceptor_registry = Some(self.interceptor_registry.clone());
        let (result, completion) = executor
            .execute_function(scope, function_name, args, interceptor_registry)
            .await?;
        // If the BAML function returned a session plan (e.g. GetDiscoverAgentsPlan) or tool call, execute it and return the tool output so JS gets e.g. { agents, done } not the raw plan.
        match self
            .execute_tool_from_baml_result_or_value(scope, result, Some(function_name))
            .await
        {
            Ok(v) => {
                if let Some(h) = completion {
                    h.complete(Outcome::Success, None).await;
                }
                Ok(v)
            }
            Err(e) => {
                if let Some(h) = completion {
                    h.complete(Outcome::Failure, Some(e.to_string())).await;
                }
                Err(e)
            }
        }
    }

    /// Invoke a BAML function with streaming support
    ///
    /// Returns a stream and the context manager. Caller must pass the same ctx_manager to
    /// `stream.run(..., ctx_manager, ...)`. Pass `context_tags` (e.g. from
    /// `build_conversation_context_tags`) for resume so BAML sees prior turns.
    pub fn invoke_function_stream(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
        context_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<(
        baml_runtime::FunctionResultStream,
        baml_runtime::RuntimeContextManager,
    )> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            has_context_tags = context_tags.as_ref().map_or(0, |m| m.len()) > 0,
            "Invoking BAML function with streaming"
        );

        // Verify function exists
        let _signature = self
            .function_registry
            .get(function_name)
            .ok_or_else(|| BamlRtError::FunctionNotFound(function_name.to_string()))?;

        // Execute the BAML function using the executor
        let executor = self
            .executor
            .as_ref()
            .ok_or_else(|| BamlRtError::BamlRuntime("BAML runtime not loaded".to_string()))?;

        executor.execute_function_stream(scope, function_name, args, context_tags)
    }

    /// Build conversation context tags for the given scope (for stream path resume).
    /// Call from async context (e.g. bridge start_stream_session) then pass result to invoke_function_stream.
    pub async fn build_conversation_context_tags(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        match &self.executor {
            Some(exec) => exec.build_conversation_context_tags(scope).await,
            None => Ok(None),
        }
    }

    /// List all available BAML functions
    pub fn list_functions(&self) -> Vec<String> {
        self.function_registry.keys().cloned().collect()
    }

    /// Get the tool registry (for tool registration)
    pub fn tool_registry(&self) -> Arc<ConcreteToolRegistry> {
        self.tool_registry.clone()
    }

    /// Get the interceptor registry (for registering interceptors)
    pub fn interceptor_registry(&self) -> Arc<TokioMutex<InterceptorRegistry>> {
        self.interceptor_registry.clone()
    }

    /// Register an LLM interceptor
    pub async fn register_llm_interceptor<I: baml_rt_interceptor::LLMInterceptor>(
        &self,
        interceptor: I,
    ) {
        let mut registry = self.interceptor_registry.lock().await;
        registry.register_llm_interceptor(interceptor);
    }

    /// Register a tool interceptor
    pub async fn register_tool_interceptor<I: baml_rt_interceptor::ToolInterceptor>(
        &self,
        interceptor: I,
    ) {
        let mut registry = self.interceptor_registry.lock().await;
        registry.register_tool_interceptor(interceptor);
    }

    /// Register a tool that implements the BamlTool trait
    ///
    /// Tools can be called by LLMs during BAML function execution
    /// or directly from JavaScript via the QuickJS bridge.
    ///
    /// # Example
    /// ```rust,no_run
    /// use baml_rt::baml::BamlRuntimeManager;
    /// use baml_rt::tools::BamlTool;
    /// use baml_rt_tools::bundles::Support;
    /// use async_trait::async_trait;
    /// use schemars::JsonSchema;
    /// use serde::{Deserialize, Serialize};
    /// use ts_rs::TS;
    ///
    /// struct MyTool;
    ///
    /// #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// #[ts(export)]
    /// struct MyInput {}
    ///
    /// #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// #[ts(export)]
    /// struct MyOutput {
    ///     result: String,
    /// }
    ///
    /// #[async_trait]
    /// impl BamlTool for MyTool {
    ///     type Bundle = Support;
    ///     const LOCAL_NAME: &'static str = "my_tool";
    ///     type OpenInput = ();
    ///     type Input = MyInput;
    ///     type Output = MyOutput;
    ///     fn description(&self) -> &'static str { "Does something" }
    ///     async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
    ///         Ok(MyOutput { result: "success".to_string() })
    ///     }
    /// }
    ///
    /// # tokio_test::block_on(async {
    /// let mut manager = BamlRuntimeManager::new()?;
    /// manager.register_tool(MyTool).await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    pub async fn register_tool<T: baml_rt_tools::BamlTool>(&mut self, tool: T) -> Result<()> {
        self.tool_registry.register(tool)
    }

    /// Execute a tool function by name with an explicit scope.
    ///
    /// Use this when you have a [`RuntimeScope`](context::RuntimeScope) in hand (e.g. in tests or
    /// at runtime boundaries). Runs the tool inside `context::with_scope(scope, ...)` so
    /// nested calls see the scope.
    pub async fn execute_tool(
        &self,
        scope: &context::RuntimeScope,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        self.tool_execution_handle()
            .execute_tool(scope, name, args)
            .await
    }

    /// Backward-compatible alias for explicit-scope tool execution.
    pub async fn execute_tool_with_scope(
        &self,
        scope: &context::RuntimeScope,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        self.execute_tool(scope, name, args).await
    }

    /// List all registered tools
    pub async fn list_tools(&self) -> Vec<String> {
        self.tool_registry.list_tools()
    }

    pub async fn set_tool_allowlist(&self, allowlist: HashSet<String>) -> Result<()> {
        self.tool_registry.set_allowlist_from_strings(allowlist)?;
        Ok(())
    }

    pub async fn open_tool_session(
        &self,
        scope: &context::RuntimeScope,
        tool_name: &str,
        open_input: serde_json::Value,
    ) -> Result<ToolSessionId> {
        self.tool_session_handle()
            .open_tool_session(scope, tool_name, open_input)
            .await
    }

    pub async fn tool_session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()> {
        self.tool_session_handle()
            .tool_session_send(session_id, input)
            .await
    }

    pub async fn tool_session_next(&self, session_id: &ToolSessionId) -> Result<ToolStep> {
        self.tool_session_handle()
            .tool_session_next(session_id)
            .await
    }

    pub async fn tool_session_finish(&self, session_id: &ToolSessionId) -> Result<()> {
        self.tool_session_handle()
            .tool_session_finish(session_id)
            .await
    }

    pub async fn tool_session_abort(
        &self,
        session_id: &ToolSessionId,
        reason: Option<String>,
    ) -> Result<()> {
        self.tool_session_handle()
            .tool_session_abort(session_id, reason)
            .await
    }

    /// Number of open tool sessions for this context. Used by tests to assert no leak after teardown.
    pub async fn open_session_count_for_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
    ) -> usize {
        self.tool_session_handle()
            .collect_session_ids_for_context(context_id)
            .await
            .len()
    }

    /// Close all tool sessions for this context (teardown). Call when an invocation ends
    /// so sessions are not leaked. Best-effort: logs but does not fail on individual finish errors.
    pub async fn close_sessions_for_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
    ) -> Result<()> {
        self.close_sessions_for_scope(context_id, None).await
    }

    /// Close tool sessions scoped to a specific task branch, or all sessions for the context
    /// when `task_id` is `None` (legacy/message-scope).
    ///
    /// **Task-scoped teardown:** When `task_id` is `Some`, only sessions whose scope matches
    /// *both* `context_id` and `task_id` are closed. This prevents parallel sibling branches
    /// from having their sessions torn down when one branch finalizes.
    pub async fn close_sessions_for_scope(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        task_id: Option<&baml_rt_core::ids::TaskId>,
    ) -> Result<()> {
        let handle = self.tool_session_handle();
        let to_close = match task_id {
            Some(tid) => {
                handle
                    .collect_session_ids_for_task_scope(context_id, tid)
                    .await
            }
            None => handle.collect_session_ids_for_context(context_id).await,
        };
        for id in &to_close {
            if let Err(e) = self.tool_session_finish(id).await {
                tracing::warn!(
                    session_id = %id,
                    context_id = %context_id,
                    task_id = ?task_id,
                    error = %e,
                    "Teardown: tool session finish failed",
                );
            }
        }
        if !to_close.is_empty() {
            tracing::debug!(
                context_id = %context_id,
                task_id = ?task_id,
                count = to_close.len(),
                "Teardown: closed tool sessions for scope",
            );
        }
        Ok(())
    }

    /// Get tool metadata (export-safe shape)
    pub async fn get_tool_metadata(&self, name: &str) -> Option<ToolFunctionMetadataExport> {
        self.tool_registry
            .get_metadata(name)
            .map(|metadata| ToolFunctionMetadataExport::from(&metadata))
    }

    pub async fn export_tool_metadata(&self) -> Vec<ToolFunctionMetadataExport> {
        self.tool_registry.export_metadata_records()
    }

    pub async fn write_tool_metadata(&self, path: &Path) -> Result<()> {
        let metadata = self.export_tool_metadata().await;
        let payload = serde_json::json!({ "tools": metadata });
        let content = serde_json::to_string_pretty(&payload).map_err(BamlRtError::Json)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        fs::write(path, content).map_err(BamlRtError::Io)?;
        Ok(())
    }

    pub async fn write_tool_typescript(&self, path: &Path) -> Result<()> {
        self.tool_registry.write_typescript_declarations(path)
    }

    pub async fn validate_tool_allowlist_registered(&self) -> Result<()> {
        self.tool_registry.validate_allowlist_registered()
    }

    /// Execute a tool from a BAML result
    ///
    /// BAML returns either:
    /// - A `ToolSessionPlan` describing FSM steps, or
    /// - A `tool_name` payload for a one-shot session.
    ///
    /// The runtime executes host tools via the session FSM in Rust.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use baml_rt::baml::BamlRuntimeManager;
    /// # use baml_rt::tools::BamlTool;
    /// # use baml_rt_tools::bundles::Support;
    /// # use async_trait::async_trait;
    /// # use schemars::JsonSchema;
    /// # use serde::{Deserialize, Serialize};
    /// # use ts_rs::TS;
    /// # struct WeatherTool;
    /// # #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// # #[ts(export)]
    /// # struct WeatherInput { location: String }
    /// # #[derive(Serialize, Deserialize, JsonSchema, TS)]
    /// # #[ts(export)]
    /// # struct WeatherOutput { temperature: String }
    /// # #[async_trait]
    /// # impl BamlTool for WeatherTool {
    /// #     type Bundle = Support;
    /// #     const LOCAL_NAME: &'static str = "get_weather";
    /// #     type OpenInput = ();
    /// #     type Input = WeatherInput;
    /// #     type Output = WeatherOutput;
    /// #     fn description(&self) -> &'static str { "" }
    /// #     async fn execute(&self, _args: Self::Input) -> baml_rt::Result<Self::Output> {
    /// #         Ok(WeatherOutput { temperature: "22°C".to_string() })
    /// #     }
    /// # }
    /// # tokio_test::block_on(async {
    /// # let mut manager = BamlRuntimeManager::new()?;
    /// manager.register_tool(WeatherTool).await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    /// Execute a tool from a BAML union type result
    ///
    /// Takes a BAML result (typed class or single-key object),
    /// derives the tool from the type name, and executes it.
    ///
    /// # Arguments
    /// * `baml_result` - The JSON result from BAML function (union variant)
    ///
    /// # Returns
    /// The result of executing the tool function
    pub async fn execute_tool_from_baml_result(
        &self,
        scope: &context::RuntimeScope,
        baml_result: Value,
    ) -> Result<Value> {
        let call = extract_tool_call(&baml_result)?.ok_or_else(|| {
            BamlRtError::InvalidArgument("No tool call found in result".to_string())
        })?;
        let tool_name = self.resolve_tool_name_from_input(&call.args).await?;
        self.execute_tool(scope, &tool_name, call.args).await
    }

    /// Execute a tool from a BAML result: session plan (requires source_baml_function) or single tool call (resolved by input schema).
    ///
    /// Session plans are bound to a tool by manifest mapping (function name -> plan type).
    /// Runtime requires the invoking function to be present in the builder-generated
    /// `session_plan_functions.json` so tool resolution does not rely on prompt-emitted `__type`.
    pub async fn execute_tool_from_baml_result_or_value(
        &self,
        scope: &context::RuntimeScope,
        baml_result: Value,
        source_baml_function: Option<&str>,
    ) -> Result<Value> {
        let plan_result = extract_tool_session_plan(&baml_result).map_err(|e| {
            tracing::warn!(
                error = %e,
                source_function = ?source_baml_function,
                "Tool session plan extraction failed; LLM effect completed with rejection_reason and PromptRejected emitted in provenance"
            );
            e
        })?;
        if let Some(plan) = plan_result {
            let tool_name = if let (Some(func_name), Some(map)) =
                (source_baml_function, &self.session_plan_functions)
            {
                if let Some(plan_type) = map.get(func_name) {
                    resolve_tool_name_from_plan_type_with_registry(
                        &self.tool_registry,
                        plan_type.as_str(),
                    )
                    .ok()
                } else {
                    None
                }
            } else {
                None
            };
            let tool_name = tool_name.ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "Session plan tool could not be resolved: no manifest entry for the invoking function. Build the agent with the builder so session_plan_functions.json is present and up to date.".to_string(),
                )
            })?;
            return self.execute_tool_session_plan(scope, tool_name, plan).await;
        }
        if let Some(call) = extract_tool_call(&baml_result)? {
            let tool_name = self.resolve_tool_name_from_input(&call.args).await?;
            return self.execute_tool(scope, &tool_name, call.args).await;
        }
        Ok(baml_result)
    }

    async fn resolve_tool_name_from_input(&self, input: &Value) -> Result<String> {
        resolve_tool_name_from_input_with_registry(&self.tool_registry, input)
    }

    /// Execute a typed tool session plan.
    ///
    /// The plan is a sequence of typed `ToolSessionOp` operations that must follow FSM rules:
    /// - First operation must be Open
    /// - Subsequent operations must be Send/Next/Finish/Abort (after Open)
    /// - After Finish/Abort, session is closed
    async fn execute_tool_session_plan(
        &self,
        scope: &context::RuntimeScope,
        tool_name: String,
        plan: ToolSessionPlan,
    ) -> Result<Value> {
        let plan_scope = scope.clone();
        let mut steps = plan.steps;
        // When the coordinator returns a continuation plan ([Send, Next] without Open), reuse the
        // existing session for this context/tool so conversation history is preserved. Only
        // insert Open when no such session exists (e.g. first plan was malformed or edge case).
        let mut session_id: Option<ToolSessionId> = None;
        if let Some(first) = steps.first()
            && !matches!(first, ToolSessionOp::Open { .. })
        {
            if let Some(existing) = self
                .tool_session_handle()
                .find_existing_session_for_scope_and_tool(&plan_scope, &tool_name)
                .await
            {
                tracing::debug!(
                    tool_name = %tool_name,
                    session_id = %existing,
                    "Reusing existing session for continuation plan (no Open step)",
                );
                session_id = Some(existing);
            } else {
                steps.insert(
                    0,
                    ToolSessionOp::Open {
                        initial_input: None,
                        reason: Some("auto-open for plan missing explicit Open".to_string()),
                    },
                );
            }
        }
        // Validate FSM: no Send before first Open
        let first_open = steps
            .iter()
            .position(|s| matches!(s, ToolSessionOp::Open { .. }));
        let first_send = steps
            .iter()
            .position(|s| matches!(s, ToolSessionOp::Send { .. }));
        if let (Some(open_pos), Some(send_pos)) = (first_open, first_send)
            && send_pos < open_pos
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "FSM violation: plan has 'send' step at position {} before 'open' at {}. FSM requires Open before Send.",
                send_pos, open_pos
            )));
        }
        if steps.is_empty() {
            if let Some(ref reason) = plan.reason {
                tracing::warn!(
                    tool_name = %tool_name,
                    plan_reason = %reason,
                    "ToolSessionPlan rejected: empty steps. Plan-level reason from coordinator logged for debugging.",
                );
            }
            return Err(BamlRtError::InvalidArgument(
                "ToolSessionPlan must have at least one step. The coordinator may have returned prose before the JSON or an empty plan; ask it to respond with only a single JSON object.".to_string(),
            ));
        }

        let mut last_output: Option<Value> = None;
        let mut streaming_outputs: Vec<Value> = Vec::new();
        let mut suspended = false;
        // True after an explicit Next step returned Done; prevents the trailing implicit-Next block from running.
        let mut next_returned_done = false;

        let total_steps = steps.len();
        for (index, step) in steps.into_iter().enumerate() {
            let _has_remaining_steps = index + 1 < total_steps;
            match step {
                ToolSessionOp::Open {
                    initial_input,
                    reason,
                } => {
                    tracing::debug!(
                        tool = %tool_name,
                        reason = ?reason,
                        "FSM step: Open"
                    );
                    if session_id.is_some() {
                        return Err(BamlRtError::InvalidArgument(
                            "Tool session already open".to_string(),
                        ));
                    }
                    // For Open step, use initial_input if provided and non-null, otherwise empty object
                    let open_input = initial_input
                        .clone()
                        .and_then(|v| if v.is_null() { None } else { Some(v) })
                        .unwrap_or_else(empty_open_input);
                    let session = self
                        .open_tool_session(&plan_scope, &tool_name, open_input)
                        .await?;
                    session_id = Some(session.clone());
                }
                ToolSessionOp::Send { input, reason } => {
                    tracing::debug!(
                        tool = %tool_name,
                        reason = ?reason,
                        "FSM step: Send"
                    );
                    let session = session_id.as_ref().ok_or_else(|| {
                        BamlRtError::InvalidArgument(
                            "send step before open: FSM requires Open before Send".to_string(),
                        )
                    })?;
                    let normalized = normalize_plan_input(input)?;
                    self.tool_session_send(session, normalized).await?;
                }
                ToolSessionOp::Next { reason } => {
                    tracing::debug!(
                        tool = %tool_name,
                        reason = ?reason,
                        "FSM step: Next"
                    );
                    let session = session_id.as_ref().ok_or_else(|| {
                        BamlRtError::InvalidArgument("next step before open".to_string())
                    })?;
                    loop {
                        match self.tool_session_next(session).await? {
                            ToolStep::Streaming { output } => {
                                let decorated =
                                    crate::quickjs_bridge::stream_yield::decorate_tool_chunk(
                                        &tool_name, &output,
                                    );
                                crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(
                                    &decorated,
                                );
                                if let Some(emitter) = self.effect_emitter.as_ref() {
                                    let _ = emitter
                                        .emit(EffectEvent::ToolStreamChunk {
                                            context_id: plan_scope.context_id().clone(),
                                            chunk: decorated,
                                        })
                                        .await;
                                }
                                streaming_outputs.push(output);
                            }
                            ToolStep::Suspended { output } => {
                                let decorated =
                                    crate::quickjs_bridge::stream_yield::decorate_tool_chunk(
                                        &tool_name, &output,
                                    );
                                crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(
                                    &decorated,
                                );
                                if let Some(emitter) = self.effect_emitter.as_ref() {
                                    let _ = emitter
                                        .emit(EffectEvent::ToolStreamChunk {
                                            context_id: plan_scope.context_id().clone(),
                                            chunk: decorated,
                                        })
                                        .await;
                                }
                                streaming_outputs.push(output);
                                suspended = true;
                                tracing::debug!(
                                    tool = %tool_name,
                                    "FSM Next: breaking on Suspended (session left open for resume)"
                                );
                                break;
                            }
                            ToolStep::Done { output } => {
                                last_output = output;
                                next_returned_done = true;
                                // Do not clear session_id here: a subsequent step in this plan may be
                                // Finish or Abort, which must call tool_session_finish/abort. Only
                                // those steps clear session_id. Continuation plans ([Send, Next]
                                // without Open) reuse the session via find_existing_session_for_scope_and_tool.
                                break;
                            }
                            ToolStep::Error { error } => {
                                self.tool_session_abort(session, Some(error.message.clone()))
                                    .await?;
                                return Err(BamlRtError::InvalidArgument(format!(
                                    "Tool failure ({:?}): {}",
                                    error.kind, error.message
                                )));
                            }
                        }
                    }
                }
                ToolSessionOp::Finish { reason } => {
                    if suspended {
                        tracing::debug!(
                            tool = %tool_name,
                            "FSM step: Finish skipped (session suspended, left open)"
                        );
                        continue;
                    }
                    tracing::debug!(
                        tool = %tool_name,
                        reason = ?reason,
                        "FSM step: Finish"
                    );
                    if let Some(session) = session_id.as_ref() {
                        self.tool_session_finish(session).await?;
                        session_id = None;
                    }
                }
                ToolSessionOp::Abort { reason, .. } => {
                    tracing::debug!(
                        tool = %tool_name,
                        reason = ?reason,
                        "FSM step: Abort"
                    );
                    if let Some(session) = session_id.as_ref() {
                        self.tool_session_abort(session, reason).await?;
                        session_id = None;
                    }
                }
            }
        }

        // If session is still open and no explicit Next was called, call Next to get result.
        // Skip when we broke on Suspended (session left open for resume) or when an explicit Next already returned Done.
        if suspended {
            // Leave session open; streaming_outputs already has the suspended output.
        } else if !next_returned_done && let Some(session) = session_id.as_ref() {
            loop {
                match self.tool_session_next(session).await? {
                    ToolStep::Streaming { output } => {
                        let decorated = crate::quickjs_bridge::stream_yield::decorate_tool_chunk(
                            &tool_name, &output,
                        );
                        crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(&decorated);
                        if let Some(emitter) = self.effect_emitter.as_ref() {
                            let _ = emitter
                                .emit(EffectEvent::ToolStreamChunk {
                                    context_id: plan_scope.context_id().clone(),
                                    chunk: decorated,
                                })
                                .await;
                        }
                        streaming_outputs.push(output);
                    }
                    ToolStep::Suspended { output } => {
                        let decorated = crate::quickjs_bridge::stream_yield::decorate_tool_chunk(
                            &tool_name, &output,
                        );
                        crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(&decorated);
                        if let Some(emitter) = self.effect_emitter.as_ref() {
                            let _ = emitter
                                .emit(EffectEvent::ToolStreamChunk {
                                    context_id: plan_scope.context_id().clone(),
                                    chunk: decorated,
                                })
                                .await;
                        }
                        streaming_outputs.push(output);
                        tracing::debug!(
                            tool = %tool_name,
                            "FSM fallback Next: breaking on Suspended (session left open)"
                        );
                        break;
                    }
                    ToolStep::Done { output } => {
                        last_output = output;
                        // Leave session open for coordinator continuation; only Finish step closes.
                        break;
                    }
                    ToolStep::Error { error } => {
                        self.tool_session_abort(session, Some(error.message.clone()))
                            .await?;
                        return Err(BamlRtError::InvalidArgument(format!(
                            "Tool failure ({:?}): {}",
                            error.kind, error.message
                        )));
                    }
                }
            }
        }

        if !streaming_outputs.is_empty() {
            if let Some(ref done) = last_output {
                let decorated =
                    crate::quickjs_bridge::stream_yield::decorate_tool_chunk(&tool_name, done);
                crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(&decorated);
                if let Some(emitter) = self.effect_emitter.as_ref() {
                    let _ = emitter
                        .emit(EffectEvent::ToolStreamChunk {
                            context_id: plan_scope.context_id().clone(),
                            chunk: decorated,
                        })
                        .await;
                }
                streaming_outputs.push(done.clone());
            }
            return Ok(Value::Array(streaming_outputs));
        }

        Ok(last_output.unwrap_or(Value::Null))
    }
}

// Implement traits for better abstraction
#[async_trait]
impl BamlFunctionExecutor for BamlRuntimeManager {
    async fn execute_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: Value,
    ) -> Result<Value> {
        self.invoke_function(scope, function_name, args).await
    }

    fn list_functions(&self) -> Vec<String> {
        self.function_registry.keys().cloned().collect()
    }
}

impl SchemaLoader for BamlRuntimeManager {
    fn load_schema(&mut self, schema_path: &str) -> Result<()> {
        self.load_schema(schema_path)
    }

    fn is_schema_loaded(&self) -> bool {
        self.is_schema_loaded()
    }
}

impl Default for BamlRuntimeManager {
    fn default() -> Self {
        Self {
            function_registry: HashMap::new(),
            executor: None,
            tool_registry: Arc::new(ConcreteToolRegistry::new()),
            session_plan_functions: None,
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(TokioMutex::new(HashMap::new())),
            tool_session_states: Arc::new(TokioMutex::new(HashMap::new())),
            tool_session_effect_tokens: Arc::new(TokioMutex::new(HashMap::new())),
            effect_emitter: None,
            conversation_context_provider: None,
            pending_parse_retry_policy: None,
        }
    }
}

//! Tool session execution handle.
//!
//! `ToolSessionExecutionHandle` wraps `ToolExecutionContext` with session
//! lifecycle state (scopes, FSM tokens) and provides `open_session`, `send`,
//! `read`, `finish`, `abort` methods. Extracted from `baml.rs` for modularity.

use std::{sync::Arc, time::Instant};

use baml_rt_core::{
    BamlRtError, Outcome, Result, SessionLifecycleError,
    bus::{EffectStartToken, ToolEffectMetadata, ToolKind},
    context,
};
use baml_rt_interceptor::ToolCallContext;
use baml_rt_observability::metrics;
use baml_rt_tools::{ToolName, ToolSessionId, ToolStep};
use dashmap::DashMap;
use serde_json::Value;

use crate::{
    baml::{
        completion_error_from, extract_delegation_target_from_open_input, tool_session_trace,
        tool_session_trace_enabled,
    },
    quickjs_bridge::stream_yield::emit_stream_chunk_static,
    tool_execution::{ToolExecutionContext, build_metadata_map_with_phase, resolve_planning_step},
};

#[derive(Debug, Clone)]
pub(crate) struct ToolCallSessionState {
    pub(crate) context: ToolCallContext,
    pub(crate) start: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolSessionScope {
    pub(crate) tool_name: String,
    pub(crate) scope: context::RuntimeScope,
    pub(crate) open_input: serde_json::Value,
}

/// Handle for tool session operations without holding the full runtime lock.
/// Use this when session operations may await and another task needs the runtime (e.g. A2A dispatcher).
#[derive(Clone)]
pub struct ToolSessionExecutionHandle {
    pub(crate) ctx: ToolExecutionContext,
    pub(crate) tool_session_scopes: Arc<DashMap<ToolSessionId, ToolSessionScope>>,
    pub(crate) tool_session_states: Arc<DashMap<ToolSessionId, ToolCallSessionState>>,
    pub(crate) tool_session_effect_tokens: Arc<DashMap<ToolSessionId, EffectStartToken<ToolKind>>>,
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
        let mut metadata = build_metadata_map_with_phase(&scope, Some("open"));
        if let Some((plan_id, step_id)) =
            resolve_planning_step(&self.ctx.execution_sessions, &scope)
            && let Some(obj) = metadata.as_object_mut()
        {
            obj.insert("plan_id".to_string(), Value::String(plan_id));
            obj.insert("step_id".to_string(), Value::String(step_id));
        }
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
        let interceptor_registry = self.ctx.interceptor_registry.lock().await;
        let _ = interceptor_registry.intercept_tool_call(&context).await?;
        drop(interceptor_registry);

        let result = self
            .ctx
            .tool_registry
            .open_session_scoped(
                tool_name,
                open_input.clone(),
                &context_id,
                &agent_id,
                scope.task_id_opt(),
            )
            .await;
        let duration_ms = start.elapsed().as_millis() as u64;
        let completion_result: Result<Value> = match &result {
            Ok(_) => Ok(Value::Null),
            Err(e) => Err(BamlRtError::InvalidArgument(e.to_string())),
        };
        let interceptor_registry = self.ctx.interceptor_registry.lock().await;
        interceptor_registry
            .notify_tool_call_complete(&context, &completion_result, duration_ms)
            .await;
        drop(interceptor_registry);

        if let Err(ref e) = result {
            tracing::error!(
                tool_name = tool_name,
                context_id = %context_id,
                error = %e,
                "Tool session open: error"
            );
        }

        let session_id = result?;
        if tool_session_trace_enabled() {
            let scope_len = self.tool_session_scopes.len();
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
        self.tool_session_scopes.insert(
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
                self.tool_session_scopes.len()
            ));
        }
        Ok(session_id)
    }

    /// Build a SessionLifecycle not-found error with optional trace diagnostics.
    #[allow(dead_code)]
    fn scope_not_found_error(&self, session_id: &ToolSessionId, phase: &str) -> BamlRtError {
        if tool_session_trace_enabled() {
            let known_ids: Vec<String> = self
                .tool_session_scopes
                .iter()
                .map(|entry| entry.key().to_string())
                .collect();
            tool_session_trace(&format!(
                "{} missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                phase,
                session_id,
                self.tool_session_scopes.len(),
                known_ids
            ));
        }
        BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
            session_id: session_id.to_string(),
        })
    }

    /// Complete the lifecycle for a session: effect token, interceptor notification, metric.
    #[allow(dead_code)]
    async fn complete_session_lifecycle(
        &self,
        session_id: &ToolSessionId,
        result: &Result<Value>,
        remove_scope: bool,
    ) {
        if let Some((_, state)) = self.tool_session_states.remove(session_id) {
            let duration_ms = state.start.elapsed().as_millis() as u64;
            if let Some((_, token)) = self.tool_session_effect_tokens.remove(session_id)
                && let Some(emitter) = self.ctx.effect_emitter.as_ref()
            {
                let outcome = if result.is_ok() {
                    Outcome::Success
                } else {
                    Outcome::Failure
                };
                if let Err(e) = token
                    .complete(emitter.as_ref(), duration_ms, outcome, None)
                    .await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        error = ?e,
                        "effect token completion failed; liveness record may be stale"
                    );
                }
            }
            let completion_result: Result<Value> = match result {
                Ok(_) => Ok(Value::Null),
                Err(err) => Err(completion_error_from(err)),
            };
            let interceptor_registry = self.ctx.interceptor_registry.lock().await;
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
        if remove_scope {
            self.tool_session_scopes.remove(session_id);
        }
    }

    /// Find an existing open session for this context and tool, if any.
    /// Used when the coordinator returns a continuation plan (e.g. [Send, Read]) so we reuse
    /// the same session instead of auto-inserting Open and creating a new one.
    ///
    /// **Task-aware:** When the scope is `TaskScope`, the match also requires the same `task_id`
    /// to prevent parallel child branches from hijacking each other's sessions under the same
    /// `context_id`. For `MessageScope`, existing `(context_id, tool_name)` behavior is preserved.
    pub async fn find_existing_session_for_scope_and_tool(
        &self,
        scope: &context::RuntimeScope,
        tool_name: &str,
    ) -> Option<ToolSessionId> {
        let context_id = scope.context_id();
        let task_id = scope.task_id_opt();
        for entry in self.tool_session_scopes.iter() {
            let sid = entry.key();
            let session_scope = entry.value();
            if session_scope.tool_name != tool_name {
                continue;
            }
            if session_scope.scope.context_id() != context_id {
                continue;
            }
            match task_id {
                Some(tid) => {
                    if session_scope.scope.task_id_opt() == Some(tid) {
                        return Some(sid.clone());
                    }
                }
                None => return Some(sid.clone()),
            }
        }
        None
    }

    /// Tool name from an already-open session for this exact [`context::RuntimeScope`], if any.
    ///
    /// Polymorphic BAML session plans omit `selected_tool` on Send/Read after Open; the runtime
    /// recovers the bound tool from the live session row (first scope match).
    pub fn tool_name_for_scope(&self, scope: &context::RuntimeScope) -> Option<ToolName> {
        self.tool_session_scopes
            .iter()
            .find(|entry| entry.value().scope == *scope)
            .and_then(|entry| ToolName::parse(&entry.value().tool_name).ok())
    }

    /// Collect all session IDs whose scope matches this context_id (for teardown).
    pub async fn collect_session_ids_for_context(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
    ) -> Vec<ToolSessionId> {
        self.tool_session_scopes
            .iter()
            .filter(|entry| entry.value().scope.context_id() == context_id)
            .map(|entry| entry.key().clone())
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
        self.tool_session_scopes
            .iter()
            .filter(|entry| {
                let s = entry.value();
                s.scope.context_id() == context_id && s.scope.task_id_opt() == Some(task_id)
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    pub async fn tool_session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()> {
        use baml_rt_interceptor::InterceptorDecision;

        let session_scope = {
            if tool_session_trace_enabled() {
                tool_session_trace(&format!(
                    "send lookup: session_id={}, scopes={}",
                    session_id,
                    self.tool_session_scopes.len()
                ));
            }
            self.tool_session_scopes.get(session_id).map(|r| r.clone())
        };
        let session_scope = session_scope.ok_or_else(|| {
            if tool_session_trace_enabled() {
                let known_ids: Vec<String> = self
                    .tool_session_scopes
                    .iter()
                    .map(|entry| entry.key().to_string())
                    .collect();
                tool_session_trace(&format!(
                    "send missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                    session_id,
                    self.tool_session_scopes.len(),
                    known_ids
                ));
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
            let mut metadata = build_metadata_map_with_phase(&session_scope.scope, Some("send"));
            if let Some((plan_id, step_id)) =
                resolve_planning_step(&self.ctx.execution_sessions, &session_scope.scope)
                && let Some(obj) = metadata.as_object_mut()
            {
                obj.insert("plan_id".to_string(), Value::String(plan_id));
                obj.insert("step_id".to_string(), Value::String(step_id));
            }

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

            let interceptor_registry = self.ctx.interceptor_registry.lock().await;
            let _decision: InterceptorDecision =
                interceptor_registry.intercept_tool_call(&context).await?;
            drop(interceptor_registry);

            // Keep a single in-flight state/token per session. This avoids replacing an
            // existing EffectStartToken on duplicate send() calls (which would leak/panic).
            let is_new_send = {
                match self.tool_session_states.entry(session_id.clone()) {
                    dashmap::mapref::entry::Entry::Vacant(entry) => {
                        entry.insert(ToolCallSessionState {
                            context: context.clone(),
                            start,
                        });
                        true
                    }
                    dashmap::mapref::entry::Entry::Occupied(_) => false,
                }
            };

            // Emit ToolStarted so relay can send TASK_STATE_WORKING (session path does not use execute_tool).
            if is_new_send {
                if let Some(emitter) = self.ctx.effect_emitter.as_ref() {
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
                        self.tool_session_effect_tokens
                            .insert(session_id.clone(), token);
                    }
                }
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    "Tool session send: reusing in-flight state/token"
                );
            }

            let result = self.ctx.tool_registry.session_send(session_id, input).await;

            // Complete the effect token on Send success so provenance records a
            // non-null tool_result. Without this, a Send-only plan fragment (no
            // subsequent Read in this invocation) leaves the ToolStarted token
            // pending forever, producing null in provenance.
            if result.is_ok() {
                let duration_ms = start.elapsed().as_millis() as u64;
                if let Some((_, token)) = self.tool_session_effect_tokens.remove(session_id)
                    && let Some(emitter) = self.ctx.effect_emitter.as_ref()
                {
                    let sent_result = serde_json::json!({ "status": "sent" });
                    if let Err(e) = token
                        .complete(
                            emitter.as_ref(),
                            duration_ms,
                            Outcome::Success,
                            Some(sent_result),
                        )
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = ?e,
                            "effect token completion failed on send success"
                        );
                    }
                }
            }

            if result.is_err() {
                let state = self.tool_session_states.remove(session_id).map(|(_, v)| v);
                let duration_ms = state
                    .as_ref()
                    .map(|state| state.start.elapsed().as_millis() as u64)
                    .unwrap_or_else(|| start.elapsed().as_millis() as u64);
                if let Some((_, token)) = self.tool_session_effect_tokens.remove(session_id)
                    && let Some(emitter) = self.ctx.effect_emitter.as_ref()
                    && let Err(e) = token
                        .complete(emitter.as_ref(), duration_ms, Outcome::Failure, None)
                        .await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        error = ?e,
                        "effect token completion failed on send error; liveness record may be stale"
                    );
                }
                let completion_result: Result<Value> = match &result {
                    Ok(_) => Ok(Value::Null),
                    Err(err) => Err(completion_error_from(err)),
                };
                let interceptor_registry = self.ctx.interceptor_registry.lock().await;
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
                // Do NOT remove tool_session_scopes here. A send failure (e.g.
                // "already has input") is recoverable — the session plan executor's
                // auto-drain path reads the pending output then retries the send.
                // Scope cleanup is the responsibility of finish/abort.
            }

            result
        };

        let result = run().await;
        if let Err(ref e) = result {
            tracing::error!(
                session_id = %session_id,
                tool_name = %session_scope.tool_name,
                context_id = %session_scope.scope.context_id(),
                error = %e,
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

    pub async fn tool_session_read(
        &self,
        session_id: &ToolSessionId,
        input: Value,
    ) -> Result<ToolStep> {
        let session_scope = self.tool_session_scopes.get(session_id).map(|r| r.clone());

        let run = || async {
            tracing::info!(session_id = %session_id, "Tool session read: start");

            // Ensure an effect token exists for this Read so provenance captures
            // the response side of the conversation. Send completes its own token
            // immediately, so Read must create a fresh one when none is present.
            if !self.tool_session_effect_tokens.contains_key(session_id) {
                if let Some(scope_entry) = self.tool_session_scopes.get(session_id)
                    && let Some(emitter) = self.ctx.effect_emitter.as_ref()
                {
                    let mut read_metadata = ToolEffectMetadata {
                        tool_name: scope_entry.tool_name.clone(),
                        function_name: None,
                        args: input.clone(),
                        metadata: build_metadata_map_with_phase(&scope_entry.scope, Some("read")),
                        delegation_target: None,
                    };
                    if let Some(target) = extract_delegation_target_from_open_input(
                        &scope_entry.tool_name,
                        &scope_entry.open_input,
                    ) {
                        read_metadata.delegation_target = Some(target);
                    }
                    let ctx_id = scope_entry.scope.context_id().clone();
                    drop(scope_entry);
                    if let Ok(token) = emitter.start_tool(ctx_id, read_metadata).await {
                        self.tool_session_effect_tokens
                            .insert(session_id.clone(), token);
                    }
                }
                // Also ensure a state entry exists for the interceptor completion path.
                if !self.tool_session_states.contains_key(session_id)
                    && let Some(scope_entry) = self.tool_session_scopes.get(session_id)
                {
                    let read_context = ToolCallContext {
                        tool_name: scope_entry.tool_name.clone(),
                        function_name: None,
                        args: input.clone(),
                        metadata: build_metadata_map_with_phase(&scope_entry.scope, Some("read")),
                        runtime_scope: scope_entry.scope.clone(),
                        delegation_target: extract_delegation_target_from_open_input(
                            &scope_entry.tool_name,
                            &scope_entry.open_input,
                        ),
                    };
                    drop(scope_entry);
                    self.tool_session_states.insert(
                        session_id.clone(),
                        ToolCallSessionState {
                            context: read_context,
                            start: Instant::now(),
                        },
                    );
                }
            }

            let result = self.ctx.tool_registry.session_read(session_id, input).await;

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
                && let Some((_, state)) = self.tool_session_states.remove(session_id)
            {
                // Do not remove scopes here; Finish/Abort is responsible for cleanup.
                let duration_ms = state.start.elapsed().as_millis() as u64;
                if let Some((_, token)) = self.tool_session_effect_tokens.remove(session_id)
                    && let Some(emitter) = self.ctx.effect_emitter.as_ref()
                {
                    let outcome = if completion_result.is_ok() {
                        Outcome::Success
                    } else {
                        Outcome::Failure
                    };
                    let result_for_prov = completion_result.as_ref().ok().cloned();
                    if let Err(e) = token
                        .complete(emitter.as_ref(), duration_ms, outcome, result_for_prov)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = ?e,
                            "effect token completion failed on read; liveness record may be stale"
                        );
                    }
                }
                let interceptor_registry = self.ctx.interceptor_registry.lock().await;
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
                    let known_ids: Vec<String> = self
                        .tool_session_scopes
                        .iter()
                        .map(|entry| entry.key().to_string())
                        .collect();
                    tool_session_trace(&format!(
                        "read missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                        session_id,
                        self.tool_session_scopes.len(),
                        known_ids
                    ));
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
                "Tool session read: ok"
            );
        } else if let Err(ref e) = result {
            tracing::error!(session_id = %session_id, error = %e, "Tool session read: error");
        }
        result
    }

    pub async fn tool_session_finish(&self, session_id: &ToolSessionId) -> Result<()> {
        let session_scope = self.tool_session_scopes.get(session_id).map(|r| r.clone());

        let run = || async {
            tracing::info!(session_id = %session_id, "Tool session finish: start");
            let result = self.ctx.tool_registry.session_finish(session_id).await;

            if let Some((_, state)) = self.tool_session_states.remove(session_id) {
                let duration_ms = state.start.elapsed().as_millis() as u64;
                if let Some((_, token)) = self.tool_session_effect_tokens.remove(session_id)
                    && let Some(emitter) = self.ctx.effect_emitter.as_ref()
                {
                    let outcome = if result.is_ok() {
                        Outcome::Success
                    } else {
                        Outcome::Failure
                    };
                    if let Err(e) = token
                        .complete(emitter.as_ref(), duration_ms, outcome, None)
                        .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = ?e,
                            "effect token completion failed on finish; liveness record may be stale"
                        );
                    }
                }
                let completion_result: Result<Value> = match &result {
                    Ok(_) => Ok(Value::Null),
                    Err(err) => Err(completion_error_from(err)),
                };
                let interceptor_registry = self.ctx.interceptor_registry.lock().await;
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
            // sessions that were only opened and never had send/read (no state).
            self.tool_session_scopes.remove(session_id);

            result
        };

        let _scope = session_scope
            .ok_or_else(|| {
                if tool_session_trace_enabled() {
                    let known_ids: Vec<String> = self
                        .tool_session_scopes
                        .iter()
                        .map(|entry| entry.key().to_string())
                        .collect();
                    tool_session_trace(&format!(
                        "finish missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                        session_id,
                        self.tool_session_scopes.len(),
                        known_ids
                    ));
                }
                BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                    session_id: session_id.to_string(),
                })
            })?
            .scope;
        let result = run().await;
        if let Err(ref e) = result {
            tracing::error!(session_id = %session_id, error = %e, "Tool session finish: error");
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
        let session_scope = self.tool_session_scopes.get(session_id).map(|r| r.clone());

        let run = || async {
            tracing::info!(session_id = %session_id, reason = ?reason, "Tool session abort: start");
            let result = self
                .ctx
                .tool_registry
                .session_abort(session_id, reason.clone())
                .await;

            if let Some((_, state)) = self.tool_session_states.remove(session_id) {
                let duration_ms = state.start.elapsed().as_millis() as u64;
                if let Some((_, token)) = self.tool_session_effect_tokens.remove(session_id)
                    && let Some(emitter) = self.ctx.effect_emitter.as_ref()
                    && let Err(e) = token
                        .complete(emitter.as_ref(), duration_ms, Outcome::Failure, None)
                        .await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        error = ?e,
                        "effect token completion failed on abort; liveness record may be stale"
                    );
                }
                self.tool_session_scopes.remove(session_id);
                let completion_result = Err(BamlRtError::InvalidArgument(
                    reason.unwrap_or_else(|| "Tool session aborted".to_string()),
                ));
                let interceptor_registry = self.ctx.interceptor_registry.lock().await;
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
                    let known_ids: Vec<String> = self
                        .tool_session_scopes
                        .iter()
                        .map(|entry| entry.key().to_string())
                        .collect();
                    tool_session_trace(&format!(
                        "abort missing scope: session_id={}, known_scopes={}, known_ids={:?}",
                        session_id,
                        self.tool_session_scopes.len(),
                        known_ids
                    ));
                }
                BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                    session_id: session_id.to_string(),
                })
            })?
            .scope;
        let result = run().await;
        if let Err(ref e) = result {
            tracing::error!(session_id = %session_id, error = %e, "Tool session abort: error");
        } else {
            tracing::info!(session_id = %session_id, "Tool session abort: ok");
        }
        result
    }

    /// Send input to a session and block until `Done`, archiving the result.
    ///
    /// Handles all internal `Streaming`/`Suspended` polling transparently.
    /// `chunk_timeout` is the edge timeout: max duration between successive
    /// Streaming/Suspended chunks before the session is aborted as hung.
    pub async fn tool_session_send_blocking(
        &self,
        session_id: &ToolSessionId,
        input: Value,
        plan_scope: &context::RuntimeScope,
        archive_ref_tables: &baml_rt_tools::archive_refs::ContextRefTables,
        chunk_timeout: std::time::Duration,
    ) -> Result<crate::tool_session_handle::SendResult> {
        use baml_rt_tools::{archive_read, archive_refs};

        // Fire the send — this enqueues input but does not block.
        self.tool_session_send(session_id, input.clone()).await?;

        // Capture send args for the archive summary before the read loop removes the state.
        let send_args_for_summary = Some(input);

        // Poll until Done, emitting streaming chunks with an edge timeout between each.
        let mut streaming_outputs: Vec<Value> = Vec::new();
        loop {
            let read_input = Value::Object(serde_json::Map::new());
            let step_future = self.tool_session_read(session_id, read_input);
            let step_result = tokio::time::timeout(chunk_timeout, step_future)
                .await
                .map_err(|_| {
                    BamlRtError::InvalidArgument(format!(
                        "tool session send_blocking: no chunk received within {:?}; aborting",
                        chunk_timeout
                    ))
                })??;

            match step_result {
                ToolStep::Streaming { output } => {
                    emit_stream_chunk_static(
                        self.ctx.effect_emitter.as_ref(),
                        plan_scope.context_id(),
                        &output,
                        &mut streaming_outputs,
                    )
                    .await;
                }
                ToolStep::Suspended { output } => {
                    // A2A task paused (input required or still running). Continue polling.
                    emit_stream_chunk_static(
                        self.ctx.effect_emitter.as_ref(),
                        plan_scope.context_id(),
                        &output,
                        &mut streaming_outputs,
                    )
                    .await;
                    continue;
                }
                ToolStep::Done { output } => {
                    let output_value = output.unwrap_or(Value::Null);
                    let rendered = archive_read::render_to_lines(&output_value);
                    let tool_name = self
                        .tool_session_scopes
                        .get(session_id)
                        .map(|s| s.tool_name.clone())
                        .unwrap_or_default();
                    // Prefer the semantic Send-input description (what was queried) as the
                    // archive summary — e.g. "discovering agents matching 'X'" is more
                    // meaningful than "tool result". Fall back to describe_result, then
                    // the generic label. send_args_for_summary captured before the read
                    // loop removes the session state.
                    let summary = send_args_for_summary
                        .as_ref()
                        .map(|args| {
                            // Wrap in the canonical step shape expected by describe_invocation.
                            let wrapped = serde_json::json!({ "op": "Send", "input": args });
                            self.ctx
                                .tool_registry
                                .describe_invocation_with_hint(Some(tool_name.as_str()), &wrapped)
                        })
                        .or_else(|| {
                            self.ctx
                                .tool_registry
                                .describe_result_for(&tool_name, &output_value)
                        })
                        .unwrap_or_else(|| "tool result".to_string());
                    let entry = archive_refs::ArchiveEntry::new(
                        rendered,
                        tool_name,
                        summary,
                        String::new(), // activity_anchor: wired when tool result is tied to graph a2a_activity_anchor
                        "tool_result".to_string(),
                    );
                    let context_id = plan_scope.context_id().as_str().to_string();
                    let ref_table =
                        archive_refs::get_or_create_ref_table(archive_ref_tables, &context_id);
                    let archive_ref = ref_table.insert(entry.clone());
                    let header = entry.display_header(archive_ref);
                    return Ok(crate::tool_session_handle::SendResult {
                        archive_ref,
                        header,
                        output: output_value,
                    });
                }
                ToolStep::Error { error } => {
                    self.tool_session_abort(session_id, Some(error.message.clone()))
                        .await
                        .ok();
                    return Err(BamlRtError::InvalidArgument(format!(
                        "Tool failure ({:?}): {}",
                        error.kind, error.message
                    )));
                }
            }
        }
    }
}

/// Result of a blocking Send: archived output with header line and raw value.
#[derive(Debug, Clone)]
pub struct SendResult {
    pub archive_ref: baml_rt_tools::archive_read::ShortRef,
    /// Display header: `@1 support/crm "found 5 accounts" [47 lines, 3.2KB]`
    pub header: String,
    /// Raw Done output (for drift scoring).
    pub output: serde_json::Value,
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tool execution context and helpers.
//!
//! `ToolExecutionContext` bundles the `Arc`-wrapped registries needed for tool
//! execution without holding the full `BamlRuntimeManager` lock. Used by both
//! single-shot tool execution and the session execution handle.

use std::{sync::Arc, time::Instant};

use baml_rt_core::{
    BamlRtError, Outcome, Result,
    bus::{EffectEmitter, ToolEffectMetadata},
    context,
    correlation::current_correlation_id,
};
use baml_rt_interceptor::{InterceptorDecision, InterceptorRegistry, ToolCallContext};
use baml_rt_observability::metrics;
use baml_rt_tools::{ToolRegistry as ConcreteToolRegistry, archive_refs::ContextRefTables};
use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;

use crate::{
    baml::tool_extraction::{extract_tool_call, resolve_tool_name_from_input_with_registry},
    tool_effect_metadata::{
        stamp_agent_package, stamp_tool_effect_metadata_scope, stamp_tool_registry_metadata,
    },
};

/// Overwrite `message_id`, `agent_id`, and `task_id` (when task-scoped) on tool-effect metadata
/// using the authoritative [`context::RuntimeScope`].
///
/// Downstream code may enrich the JSON map after [`build_metadata_map_with_phase`]; this keeps
/// provenance effect metadata aligned with the scope actually executing the tool so
/// `ProvenanceEffectSubscriber` always records task-scoped tool calls under the correct task.
pub(crate) fn build_metadata_map_with_phase(
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

/// Resolve (plan_id, step_id) for the current in-progress step from the shared
/// execution session state. Returns `None` when no plan is active or no step is
/// in progress.
pub(crate) fn resolve_planning_step(
    execution_sessions: &DashMap<String, crate::quickjs_bridge::ExecutionSession>,
    scope: &context::RuntimeScope,
) -> Option<(String, String)> {
    use crate::quickjs_bridge::ExecutionSession;
    let task_id = scope.task_id_opt()?;
    execution_sessions
        .iter()
        .filter_map(|entry| {
            if let ExecutionSession::Executable {
                base,
                plan_id,
                current_step_id,
                ..
            } = entry.value()
            {
                if base.owner_task_id == task_id.as_str() {
                    current_step_id
                        .as_ref()
                        .map(|sid| (plan_id.clone(), sid.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .next()
}

/// Shared state bundle for tool execution. Replaces the former `ToolExecutionHandle`
/// and is reused by `ToolSessionExecutionHandle` and `BamlRuntimeManager`.
#[derive(Clone)]
pub(crate) struct ToolExecutionContext {
    pub tool_registry: Arc<ConcreteToolRegistry>,
    pub interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    pub effect_emitter: Option<Arc<dyn EffectEmitter>>,
    pub execution_sessions: Arc<DashMap<String, crate::quickjs_bridge::ExecutionSession>>,
    #[expect(
        dead_code,
        reason = "carried for archive-ref wiring; readers live on the session-handle path"
    )]
    pub archive_ref_tables: Arc<ContextRefTables>,
    /// When set (Surreal-backed agent), new archives allocate `@prefix/local` in the store.
    pub archive_ref_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
    /// Deployed agent package stamped on tool-call metadata for per-agent policy resolution.
    pub agent_package: Option<Arc<str>>,
}

impl ToolExecutionContext {
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
        self.execute_tool(scope, &tool_name.to_string(), call.args)
            .await
    }

    async fn execute_tool_inner(
        &self,
        scope: context::RuntimeScope,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        let start = Instant::now();
        let context_id = scope.context_id().clone();
        let agent_id = scope.agent_id().clone();
        let mut metadata = build_metadata_map_with_phase(&scope, Some("execute"));
        stamp_tool_effect_metadata_scope(&scope, &mut metadata);
        stamp_agent_package(self.agent_package.as_deref(), &mut metadata);
        stamp_tool_registry_metadata(&self.tool_registry, name, &mut metadata);
        if let Some((plan_id, step_id)) = resolve_planning_step(&self.execution_sessions, &scope)
            && let Some(obj) = metadata.as_object_mut()
        {
            obj.insert("plan_id".to_string(), Value::String(plan_id));
            obj.insert("step_id".to_string(), Value::String(step_id));
        }

        let context = ToolCallContext {
            tool_name: name.to_string(),
            function_name: None,
            args: args.clone(),
            metadata: metadata.clone(),
            runtime_scope: scope.clone(),
            agent_package: self.agent_package.as_ref().map(|s| s.to_string()),
            delegation_target: None,
        };

        let (tool_backend, tool_digest) = self
            .tool_registry
            .get_metadata(name)
            .map(|m| (format!("{:?}", m.backend), m.digest))
            .map(|(backend, digest)| (Some(backend), digest))
            .unwrap_or((None, None));

        let interceptor_registry = self.interceptor_registry.lock().await;
        let intercept_decision = interceptor_registry.intercept_tool_call(&context).await;
        interceptor_registry
            .stamp_tool_metadata(&context, &mut metadata)
            .await;
        drop(interceptor_registry);

        match intercept_decision {
            Ok(InterceptorDecision::RequireAuthorization(prompt)) => {
                self.emit_gate_blocked_effect(
                    &context_id,
                    name,
                    &args,
                    &metadata,
                    tool_backend.clone(),
                    tool_digest.clone(),
                )
                .await;
                return Err(BamlRtError::GateAuthorizationRequired { prompt });
            }
            Ok(InterceptorDecision::Allow) | Ok(InterceptorDecision::Substitute(_)) => {}
            Ok(InterceptorDecision::Block(msg)) => {
                self.emit_gate_blocked_effect(
                    &context_id,
                    name,
                    &args,
                    &metadata,
                    tool_backend.clone(),
                    tool_digest.clone(),
                )
                .await;
                return Err(BamlRtError::ToolExecution(format!(
                    "Tool call blocked by interceptor: {msg}"
                )));
            }
            Err(e) => return Err(e),
        }

        let effect_metadata = ToolEffectMetadata {
            tool_name: name.to_string(),
            function_name: None,
            args: args.clone(),
            metadata: metadata.clone(),
            delegation_target: None,
            tool_backend,
            tool_digest,
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

        let final_args = args;
        let result = self
            .tool_registry
            .execute(name, final_args, &context_id, &agent_id)
            .await;

        let duration = start.elapsed();
        let duration_ms = duration.as_millis() as u64;
        let outcome = Outcome::from(result.is_ok());

        let result_for_prov = result.as_ref().ok().cloned();
        if let Some(token) = effect_token
            && let Some(emitter) = self.effect_emitter.as_ref()
            && let Err(e) = token
                .complete(emitter.as_ref(), duration_ms, outcome, result_for_prov)
                .await
        {
            tracing::warn!(error = ?e, "Failed to complete tool effect");
        }

        let interceptor_registry = self.interceptor_registry.lock().await;
        interceptor_registry
            .notify_tool_call_complete(&context, &result, duration_ms)
            .await;
        drop(interceptor_registry);

        let metric_result = if result.is_ok() { "success" } else { "error" };
        metrics::record_tool_invocation(name, metric_result, duration);

        result
    }

    pub(crate) async fn emit_gate_blocked_effect(
        &self,
        context_id: &baml_rt_core::ids::ContextId,
        name: &str,
        args: &Value,
        metadata: &Value,
        tool_backend: Option<String>,
        tool_digest: Option<String>,
    ) {
        tracing::info!(
            tool = name,
            context_id = %context_id,
            has_emitter = self.effect_emitter.is_some(),
            has_gate_meta = metadata
                .get("semiotic_gate")
                .is_some(),
            "emit_gate_blocked_effect"
        );
        let Some(emitter) = self.effect_emitter.as_ref() else {
            tracing::warn!(
                tool = name,
                context_id = %context_id,
                "gate blocked effect skipped: effect_emitter not wired"
            );
            return;
        };
        let effect_metadata = ToolEffectMetadata {
            tool_name: name.to_string(),
            function_name: None,
            args: args.clone(),
            metadata: metadata.clone(),
            delegation_target: None,
            tool_backend,
            tool_digest,
        };
        let token = match emitter
            .start_tool(context_id.clone(), effect_metadata)
            .await
        {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!(
                    tool = name,
                    context_id = %context_id,
                    error = ?e,
                    "gate blocked effect: start_tool failed"
                );
                return;
            }
        };
        if let Err(e) = token
            .complete(emitter.as_ref(), 0, Outcome::Failure, None)
            .await
        {
            tracing::warn!(
                tool = name,
                context_id = %context_id,
                error = ?e,
                "gate blocked effect: tool complete failed"
            );
        }
    }
}

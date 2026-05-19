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
use baml_rt_interceptor::{InterceptorRegistry, ToolCallContext};
use baml_rt_observability::metrics;
use baml_rt_tools::{ToolRegistry as ConcreteToolRegistry, archive_refs::ContextRefTables};
use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;

use crate::baml::tool_extraction::{extract_tool_call, resolve_tool_name_from_input_with_registry};

/// Overwrite `message_id`, `agent_id`, and `task_id` (when task-scoped) on tool-effect metadata
/// using the authoritative [`context::RuntimeScope`].
///
/// Downstream code may enrich the JSON map after [`build_metadata_map_with_phase`]; this keeps
/// provenance effect metadata aligned with the scope actually executing the tool so
/// `ProvenanceEffectSubscriber` always records task-scoped tool calls under the correct task.
pub(crate) fn stamp_tool_effect_metadata_scope(
    scope: &context::RuntimeScope,
    metadata: &mut Value,
) {
    let Value::Object(obj) = metadata else {
        return;
    };
    obj.insert(
        "message_id".to_string(),
        Value::String(scope.message_id().as_str().to_string()),
    );
    obj.insert(
        "agent_id".to_string(),
        Value::String(scope.agent_id().as_str().to_string()),
    );
    if let Some(task_id) = scope.task_id_opt() {
        obj.insert(
            "task_id".to_string(),
            Value::String(task_id.as_str().to_string()),
        );
    }
}

/// Build a metadata map for tool/session effects, including correlation, scope IDs, and optional FSM phase.
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
    #[allow(dead_code)] // Carried for archive ref wiring; readers live on the session handle path.
    pub archive_ref_tables: Arc<ContextRefTables>,
    /// When set (Surreal-backed agent), new archives allocate `@prefix/local` in the store.
    pub archive_ref_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
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
            delegation_target: None,
        };

        let (tool_backend, tool_digest) = self
            .tool_registry
            .get_metadata(name)
            .map(|m| (format!("{:?}", m.backend), m.digest))
            .map(|(backend, digest)| (Some(backend), digest))
            .unwrap_or((None, None));
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

        let interceptor_registry = self.interceptor_registry.lock().await;
        let interceptor_result = interceptor_registry.intercept_tool_call(&context).await;
        drop(interceptor_registry);
        if let Err(e) = interceptor_result {
            if let Some(token) = effect_token
                && let Some(emitter) = self.effect_emitter.as_ref()
            {
                let duration_ms = start.elapsed().as_millis() as u64;
                if let Err(complete_err) = token
                    .complete(emitter.as_ref(), duration_ms, Outcome::Failure, None)
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
}

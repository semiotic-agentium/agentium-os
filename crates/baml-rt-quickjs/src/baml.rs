//! BAML runtime wrapper and function execution.
//!
//! [`BamlRuntimeManager`] owns the function registry, tool registry, and session
//! state. Tool call and plan extraction live in [`tool_extraction`]; session
//! open/send/next/finish/abort and plan execution remain here and use
//! scope-from-token for attribution.

mod tool_extraction;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Outcome, Result, SessionLifecycleError,
    bus::{
        EffectEmitter, EffectEvent, EffectStartToken, PlanningSupersessionKind, ToolEffectMetadata,
        ToolKind,
    },
    context,
    correlation::current_correlation_id,
    ids::{ExternalId, IntentId, PlanId, PlanStepId, TaskId},
    types::FunctionSignature,
};
use baml_rt_interceptor::{InterceptorRegistry, ToolCallContext};
use baml_rt_llm_config::FnoxFileSecretResolver;
use baml_rt_observability::metrics;
use baml_rt_tools::{
    ToolFunctionMetadataExport, ToolRegistry as ConcreteToolRegistry, ToolSessionId, ToolStep,
};
use baml_types::BamlValue;
use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;
pub(crate) use tool_extraction::{
    ToolSessionOp, ToolSessionPlan, extract_tool_call, extract_tool_session_plan,
    normalize_plan_input, resolve_tool_name_from_input_with_registry,
    resolve_tool_name_from_plan_type_with_registry,
};

use crate::{
    baml_execution::{
        BamlExecutor, BamlStreamInvocation, ConversationContextProvider, ParseRetryPolicy,
    },
    llm_client_registry::LlmSecretResolver,
    llm_resolver_adapter::SecretResolverToLlmAdapter,
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

fn schema_allows_empty_open_input(schema: &Value) -> bool {
    match schema {
        Value::Null => true,
        Value::Object(map) => {
            if let Some(any_of) = map.get("anyOf").and_then(Value::as_array)
                && any_of.iter().any(schema_allows_empty_open_input)
            {
                return true;
            }
            if let Some(one_of) = map.get("oneOf").and_then(Value::as_array)
                && one_of.iter().any(schema_allows_empty_open_input)
            {
                return true;
            }
            if let Some(all_of) = map.get("allOf").and_then(Value::as_array)
                && !all_of.is_empty()
                && all_of.iter().all(schema_allows_empty_open_input)
            {
                return true;
            }

            let type_allows = match map.get("type") {
                Some(Value::String(t)) if t == "null" => return true,
                Some(Value::String(t)) if t == "object" => true,
                Some(Value::Array(types)) => types
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|t| t == "object" || t == "null"),
                Some(_) => false,
                None => map.contains_key("properties") || map.contains_key("required"),
            };
            if !type_allows {
                return false;
            }

            let has_required = map
                .get("required")
                .and_then(Value::as_array)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if has_required {
                return false;
            }
            let min_properties = map
                .get("minProperties")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            min_properties == 0
        }
        _ => false,
    }
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

#[derive(Debug, Clone)]
pub struct PlanningDynamicContext {
    pub scope: context::RuntimeScope,
    pub available_tools: Vec<String>,
    pub conversation_history: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct CanonicalIntentSubmission {
    pub intent_id: IntentId,
    pub description: String,
    pub derived_from_message_ids: Vec<String>,
    pub supersession: Option<PlanningSupersessionKind>,
}

#[derive(Debug, Clone)]
pub struct CanonicalPlanSubmission {
    pub intent_id: IntentId,
    pub plan_id: PlanId,
    pub steps: Value,
    pub supersession: Option<PlanningSupersessionKind>,
}

#[derive(Debug, Clone)]
pub struct CanonicalPlanStepStatusChange {
    pub intent_id: IntentId,
    pub plan_id: PlanId,
    pub step_id: PlanStepId,
    pub old_status: Option<String>,
    pub new_status: String,
    pub evidence_text: String,
}

#[async_trait]
pub trait PlanningCanonicalResolver: Send + Sync {
    async fn resolve_intent(
        &self,
        context: &PlanningDynamicContext,
        submission: CanonicalIntentSubmission,
    ) -> Result<CanonicalIntentSubmission>;
    async fn resolve_plan(
        &self,
        context: &PlanningDynamicContext,
        submission: CanonicalPlanSubmission,
    ) -> Result<CanonicalPlanSubmission>;
    async fn resolve_step_status(
        &self,
        context: &PlanningDynamicContext,
        submission: CanonicalPlanStepStatusChange,
    ) -> Result<CanonicalPlanStepStatusChange>;
}

struct DefaultPlanningCanonicalResolver;

#[async_trait]
impl PlanningCanonicalResolver for DefaultPlanningCanonicalResolver {
    async fn resolve_intent(
        &self,
        _context: &PlanningDynamicContext,
        submission: CanonicalIntentSubmission,
    ) -> Result<CanonicalIntentSubmission> {
        if submission.intent_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical intent_id must be non-empty".to_string(),
            ));
        }
        if submission.description.trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical intent description must be non-empty".to_string(),
            ));
        }
        if submission.derived_from_message_ids.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical intent must derive from at least one message".to_string(),
            ));
        }
        Ok(submission)
    }

    async fn resolve_plan(
        &self,
        _context: &PlanningDynamicContext,
        submission: CanonicalPlanSubmission,
    ) -> Result<CanonicalPlanSubmission> {
        if submission.intent_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan intent_id must be non-empty".to_string(),
            ));
        }
        if submission.plan_id.as_str().trim().is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan_id must be non-empty".to_string(),
            ));
        }
        let Some(steps) = submission.steps.as_array() else {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan steps must be an array".to_string(),
            ));
        };
        if steps.is_empty() {
            return Err(BamlRtError::InvalidArgument(
                "canonical plan steps must be non-empty".to_string(),
            ));
        }
        Ok(submission)
    }

    async fn resolve_step_status(
        &self,
        _context: &PlanningDynamicContext,
        submission: CanonicalPlanStepStatusChange,
    ) -> Result<CanonicalPlanStepStatusChange> {
        if submission.intent_id.as_str().trim().is_empty()
            || submission.plan_id.as_str().trim().is_empty()
            || submission.step_id.as_str().trim().is_empty()
            || submission.new_status.trim().is_empty()
            || submission.evidence_text.trim().is_empty()
        {
            return Err(BamlRtError::InvalidArgument(
                "canonical step status change fields must be non-empty".to_string(),
            ));
        }
        Ok(submission)
    }
}

/// Linearly-typed builder for [`BamlRuntimeManager`]. Inject optional dependencies (e.g. LLM
/// secret resolver from fnox) then call [`build`](BamlRuntimeManagerBuilder::build); then
/// [`load_schema`](BamlRuntimeManager::load_schema) and register tools as usual.
#[derive(Default)]
pub struct BamlRuntimeManagerBuilder {
    llm_secret_resolver: Option<Arc<dyn LlmSecretResolver>>,
}

impl BamlRuntimeManagerBuilder {
    pub fn with_llm_secret_resolver(self, resolver: Arc<dyn LlmSecretResolver>) -> Self {
        Self {
            llm_secret_resolver: Some(resolver),
        }
    }

    pub fn with_fnox_llm_resolver(self, path: impl AsRef<Path>) -> Self {
        let resolver = Arc::new(SecretResolverToLlmAdapter::new(Arc::new(
            FnoxFileSecretResolver::from_path(Some(path.as_ref())),
        )));
        self.with_llm_secret_resolver(resolver)
    }

    pub fn build(self) -> Result<BamlRuntimeManager> {
        let mut manager = BamlRuntimeManager::new()?;
        if let Some(resolver) = self.llm_secret_resolver {
            manager.set_llm_secret_resolver(resolver);
        }
        Ok(manager)
    }
}

/// Manages the BAML runtime and function registry
pub struct BamlRuntimeManager {
    function_registry: HashMap<String, FunctionSignature>,
    pub(crate) executor: Option<BamlExecutor>,
    tool_registry: Arc<ConcreteToolRegistry>,
    /// Builder-generated map: function name → session plan type. Lets the runtime resolve tool from the call site.
    session_plan_functions: Option<SessionPlanFunctionsMap>,
    interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    tool_session_scopes: Arc<DashMap<ToolSessionId, ToolSessionScope>>,
    tool_session_states: Arc<DashMap<ToolSessionId, ToolCallSessionState>>,
    /// Tokens for ToolStarted emitted in tool_session_send; completed in tool_session_read/finish/abort. Shared across handle() calls.
    tool_session_effect_tokens: Arc<DashMap<ToolSessionId, EffectStartToken<ToolKind>>>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    conversation_context_provider: Option<Arc<dyn ConversationContextProvider>>,
    pending_parse_retry_policy: Option<ParseRetryPolicy>,
    /// When set, LLM API keys are injected via ClientRegistry (not env vars).
    llm_secret_resolver: Option<Arc<dyn LlmSecretResolver>>,
    planning_resolver: Arc<dyn PlanningCanonicalResolver>,
    execution_sessions: Arc<DashMap<String, crate::quickjs_bridge::ExecutionSession>>,
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

/// Shared state bundle for tool execution. Replaces the former `ToolExecutionHandle`
/// and is reused by `ToolSessionExecutionHandle` and `BamlRuntimeManager`.
#[derive(Clone)]
pub(crate) struct ToolExecutionContext {
    pub tool_registry: Arc<ConcreteToolRegistry>,
    pub interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    pub effect_emitter: Option<Arc<dyn EffectEmitter>>,
    pub execution_sessions: Arc<DashMap<String, crate::quickjs_bridge::ExecutionSession>>,
}

/// Resolve (plan_id, step_id) for the current in-progress step from the shared
/// execution session state. Returns `None` when no plan is active or no step is
/// in progress (e.g. for BAML calls outside a plan context like InferNotionIntent).
///
/// INVARIANT 3 (step coordinate availability): Returns `Some((plan_id, step_id))`
/// when an `ExecutionSession::Executable` exists for this task with an active `current_step_id`.
///
/// Uses `StdMutex` (not tokio) so it is safe to call from both sync and async contexts.
/// The scan is O(sessions) which is tiny (typically 1-2 per task).
fn resolve_planning_step(
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
        let mut metadata = build_metadata_map_with_phase(&scope, Some("execute"));
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
        let result_for_prov = result.as_ref().ok().cloned();
        if let Some(token) = effect_token
            && let Some(emitter) = self.effect_emitter.as_ref()
            && let Err(e) = token
                .complete(emitter.as_ref(), duration_ms, outcome, result_for_prov)
                .await
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
    ctx: ToolExecutionContext,
    tool_session_scopes: Arc<DashMap<ToolSessionId, ToolSessionScope>>,
    tool_session_states: Arc<DashMap<ToolSessionId, ToolCallSessionState>>,
    /// Token for ToolStarted emitted in tool_session_send; completed in tool_session_read (or on error in send).
    tool_session_effect_tokens: Arc<DashMap<ToolSessionId, EffectStartToken<ToolKind>>>,
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
            tracing::warn!(
                tool_name = tool_name,
                context_id = %context_id,
                error = ?e,
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

    /// Find an existing open session for this context and tool, if any.
    /// Used when the coordinator returns a continuation plan (e.g. [Send, Read]) so we reuse
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
                self.tool_session_scopes.remove(session_id);
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
            tracing::warn!(session_id = %session_id, error = ?e, "Tool session read: error");
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
            tracing::warn!(session_id = %session_id, error = ?e, "Tool session abort: error");
        } else {
            tracing::info!(session_id = %session_id, "Tool session abort: ok");
        }
        result
    }
}

impl BamlRuntimeManager {
    pub fn builder() -> BamlRuntimeManagerBuilder {
        BamlRuntimeManagerBuilder::default()
    }

    /// Create a new BAML runtime manager
    pub fn new() -> Result<Self> {
        tracing::info!("Initializing BAML runtime manager");

        Ok(Self {
            function_registry: HashMap::new(),
            executor: None,
            tool_registry: Arc::new(ConcreteToolRegistry::new()),
            session_plan_functions: None,
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(DashMap::new()),
            tool_session_states: Arc::new(DashMap::new()),
            tool_session_effect_tokens: Arc::new(DashMap::new()),
            effect_emitter: None,
            conversation_context_provider: None,
            pending_parse_retry_policy: None,
            llm_secret_resolver: None,
            planning_resolver: Arc::new(DefaultPlanningCanonicalResolver),
            execution_sessions: Arc::new(DashMap::new()),
        })
    }

    /// Resolve all known LLM secret keys from the resolver as a HashMap for BAML's env_vars.
    /// BAML schemas reference secrets as `api_key env.X`; BAML resolves these from env_vars
    /// passed to `BamlRuntime::from_directory`. By resolving via fnox here, we avoid
    /// depending on std::env::var — the fnox resolver is the single source of truth.
    fn resolve_secrets_as_env_vars(&self) -> std::collections::HashMap<String, String> {
        let Some(resolver) = &self.llm_secret_resolver else {
            return std::collections::HashMap::new();
        };
        let mut env_vars = std::collections::HashMap::new();
        for key in crate::llm_client_registry::LLM_SECRET_KEYS {
            if let Some((value, _)) = resolver.resolve_llm_api_key("default", key) {
                env_vars.insert((*key).to_string(), value);
            }
        }
        env_vars
    }

    /// Set the LLM secret resolver for ClientRegistry-based API key injection.
    /// When set, API keys are resolved via the resolver (e.g. fnox + llm mapping) and
    /// passed to BAML as ClientRegistry, not env vars. May be called before or after load_schema.
    pub fn set_llm_secret_resolver(&mut self, resolver: Arc<dyn LlmSecretResolver>) {
        self.llm_secret_resolver = Some(resolver.clone());
        if let Some(executor) = self.executor.as_mut() {
            executor.set_llm_secret_resolver(resolver);
        }
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

    pub fn set_planning_resolver(&mut self, resolver: Arc<dyn PlanningCanonicalResolver>) {
        self.planning_resolver = resolver;
    }

    /// Replace the execution_sessions map with a shared Arc from the bridge.
    /// Called during bridge registration so tool handles read the same state
    /// that the JS execution session commands write to.
    pub(crate) fn set_execution_sessions(
        &mut self,
        sessions: Arc<DashMap<String, crate::quickjs_bridge::ExecutionSession>>,
    ) {
        self.execution_sessions = sessions;
    }

    async fn build_planning_dynamic_context(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<PlanningDynamicContext> {
        let mut available_tools = self
            .tool_registry
            .all_metadata()
            .iter()
            .map(|metadata| metadata.name.to_string())
            .collect::<Vec<_>>();
        available_tools.sort();
        available_tools.dedup();
        let conversation_history =
            if let Some(provider) = self.conversation_context_provider.as_ref() {
                provider.conversation_history_json(scope).await?
            } else {
                None
            };
        Ok(PlanningDynamicContext {
            scope: scope.clone(),
            available_tools,
            conversation_history,
        })
    }

    pub(crate) fn tool_execution_context(&self) -> ToolExecutionContext {
        ToolExecutionContext {
            tool_registry: self.tool_registry.clone(),
            interceptor_registry: self.interceptor_registry.clone(),
            effect_emitter: self.effect_emitter.clone(),
            execution_sessions: self.execution_sessions.clone(),
        }
    }

    /// Returns a handle for session operations. Use this to avoid holding the runtime lock across awaits.
    pub fn tool_session_handle(&self) -> ToolSessionExecutionHandle {
        ToolSessionExecutionHandle {
            ctx: self.tool_execution_context(),
            tool_session_scopes: self.tool_session_scopes.clone(),
            tool_session_states: self.tool_session_states.clone(),
            tool_session_effect_tokens: self.tool_session_effect_tokens.clone(),
        }
    }

    pub async fn emit_planning_intent_resolved(
        &self,
        scope: &context::RuntimeScope,
        intent_id: String,
        description: String,
        derived_from_message_ids: Vec<String>,
        supersession: Option<PlanningSupersessionKind>,
        epoch: Option<u64>,
    ) -> Result<()> {
        let Some(task_id) = scope.task_id_opt() else {
            return Err(BamlRtError::InvalidArgument(
                "planning intent requires task scope".to_string(),
            ));
        };
        let emitter = self
            .effect_emitter
            .as_ref()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("effect emitter not configured".to_string())
            })?
            .clone();
        let dynamic_context = self.build_planning_dynamic_context(scope).await?;
        let canonical = self
            .planning_resolver
            .resolve_intent(
                &dynamic_context,
                CanonicalIntentSubmission {
                    intent_id: intent_id.into(),
                    description,
                    derived_from_message_ids,
                    supersession,
                },
            )
            .await?;
        let event = EffectEvent::IntentResolved {
            context_id: scope.context_id().clone(),
            task_id: TaskId::from_external(ExternalId::new(task_id.as_str().to_string())),
            intent_id: canonical.intent_id,
            description: canonical.description,
            derived_from_message_ids: canonical.derived_from_message_ids,
            supersession: canonical.supersession,
            epoch,
        };
        emitter.emit(event).await
    }

    pub async fn emit_planning_plan_generated(
        &self,
        scope: &context::RuntimeScope,
        intent_id: String,
        plan_id: String,
        steps: Value,
        supersession: Option<PlanningSupersessionKind>,
        epoch: Option<u64>,
    ) -> Result<()> {
        let Some(task_id) = scope.task_id_opt() else {
            return Err(BamlRtError::InvalidArgument(
                "planning plan requires task scope".to_string(),
            ));
        };
        let emitter = self
            .effect_emitter
            .as_ref()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("effect emitter not configured".to_string())
            })?
            .clone();
        let dynamic_context = self.build_planning_dynamic_context(scope).await?;
        let canonical = self
            .planning_resolver
            .resolve_plan(
                &dynamic_context,
                CanonicalPlanSubmission {
                    intent_id: intent_id.into(),
                    plan_id: plan_id.into(),
                    steps,
                    supersession,
                },
            )
            .await?;
        let event = EffectEvent::PlanGenerated {
            context_id: scope.context_id().clone(),
            task_id: TaskId::from_external(ExternalId::new(task_id.as_str().to_string())),
            intent_id: canonical.intent_id,
            plan_id: canonical.plan_id,
            steps: canonical.steps,
            supersession: canonical.supersession,
            epoch,
        };
        emitter.emit(event).await
    }

    #[allow(clippy::too_many_arguments)] // 9 distinct planning fields with no natural grouping at this layer
    pub async fn emit_planning_step_status_changed(
        &self,
        scope: &context::RuntimeScope,
        intent_id: String,
        plan_id: String,
        step_id: String,
        old_status: Option<String>,
        new_status: String,
        evidence_text: String,
        epoch: Option<u64>,
    ) -> Result<()> {
        let Some(task_id) = scope.task_id_opt() else {
            return Err(BamlRtError::InvalidArgument(
                "planning step status requires task scope".to_string(),
            ));
        };
        let emitter = self
            .effect_emitter
            .as_ref()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument("effect emitter not configured".to_string())
            })?
            .clone();
        let dynamic_context = self.build_planning_dynamic_context(scope).await?;
        let canonical = self
            .planning_resolver
            .resolve_step_status(
                &dynamic_context,
                CanonicalPlanStepStatusChange {
                    intent_id: intent_id.into(),
                    plan_id: plan_id.into(),
                    step_id: step_id.into(),
                    old_status,
                    new_status,
                    evidence_text,
                },
            )
            .await?;
        let event = EffectEvent::PlanStepStatusChanged {
            context_id: scope.context_id().clone(),
            task_id: TaskId::from_external(ExternalId::new(task_id.as_str().to_string())),
            intent_id: canonical.intent_id,
            plan_id: canonical.plan_id,
            step_id: canonical.step_id,
            old_status: canonical.old_status,
            new_status: canonical.new_status,
            evidence_text: canonical.evidence_text,
            epoch,
        };
        emitter.emit(event).await
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

        // Resolve LLM secrets from fnox resolver and inject as env_vars so BAML schema's
        // `api_key env.X` references resolve without relying on std::env::var.
        let env_vars = self.resolve_secrets_as_env_vars();
        let tool_registry_clone = self.tool_registry.clone();
        let mut executor = BamlExecutor::load_il(&baml_src_dir, tool_registry_clone, env_vars)?;

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
        if let Some(ref resolver) = self.llm_secret_resolver {
            executor.set_llm_secret_resolver(resolver.clone());
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
        let planning_step = resolve_planning_step(&self.execution_sessions, scope);
        let force_session_fsm_client = self
            .session_plan_functions
            .as_ref()
            .map(|map| map.contains_key(function_name))
            .unwrap_or(false);
        let invocation_args = args.clone();
        let (result, completion) = executor
            .execute_function(
                scope,
                function_name,
                args,
                force_session_fsm_client,
                interceptor_registry,
                planning_step,
            )
            .await?;
        // If the BAML function returned a session plan (e.g. GetDiscoverAgentsPlan) or tool call, execute it and return the tool output so JS gets e.g. { agents, done } not the raw plan.
        match self
            .execute_tool_from_baml_result_or_value(
                scope,
                result,
                Some(function_name),
                Some(&invocation_args),
            )
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
    /// Returns a [`BamlStreamInvocation`] bundling stream, context manager, client registry, and env vars.
    /// Run with `inv.stream.run(..., &inv.ctx_manager, None, inv.client_registry_opt.as_ref(), inv.env_vars)`.
    pub fn invoke_function_stream(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: serde_json::Value,
        context_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<BamlStreamInvocation> {
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

    /// Get the session plan functions map (function name → plan type).
    pub fn session_plan_functions(&self) -> Option<SessionPlanFunctionsMap> {
        self.session_plan_functions.clone()
    }

    /// Resolve the `SessionPolicy` for a BAML step-executor function name.
    ///
    /// Looks up `func_name` in the session-plan-functions manifest, then finds
    /// the matching tool in the registry and returns its declared policy.
    /// Returns `Strict` (the safe default) when lookup fails at any step.
    pub fn resolve_session_policy_for_function(
        &self,
        func_name: &str,
    ) -> baml_rt_tools::SessionPolicy {
        let plan_type = match self
            .session_plan_functions
            .as_ref()
            .and_then(|m| m.get(func_name))
        {
            Some(pt) => pt.clone(),
            None => return baml_rt_tools::SessionPolicy::default(),
        };
        let tool_name = match tool_extraction::resolve_tool_name_from_plan_type_with_registry(
            &self.tool_registry,
            &plan_type,
        ) {
            Ok(name) => name,
            Err(_) => return baml_rt_tools::SessionPolicy::default(),
        };
        self.tool_registry
            .get_metadata(&tool_name)
            .map(|meta| meta.session_policy)
            .unwrap_or_default()
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
    /// let mut manager = BamlRuntimeManager::builder().build()?;
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
        self.tool_execution_context()
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

    pub async fn tool_session_read(
        &self,
        session_id: &ToolSessionId,
        input: Value,
    ) -> Result<ToolStep> {
        self.tool_session_handle()
            .tool_session_read(session_id, input)
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
    /// # let mut manager = BamlRuntimeManager::builder().build()?;
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
        invocation_args: Option<&Value>,
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
            return self
                .execute_tool_session_plan(
                    scope,
                    tool_name,
                    plan,
                    source_baml_function,
                    invocation_args,
                )
                .await;
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
    /// - Subsequent operations must be Send/Read/Finish/Abort (after Open)
    /// - After Finish/Abort, session is closed
    async fn execute_tool_session_plan(
        &self,
        scope: &context::RuntimeScope,
        tool_name: String,
        plan: ToolSessionPlan,
        source_baml_function: Option<&str>,
        invocation_args: Option<&Value>,
    ) -> Result<Value> {
        fn op_name(op: &ToolSessionOp) -> &'static str {
            match op {
                ToolSessionOp::Open { .. } => "Open",
                ToolSessionOp::Send { .. } => "Send",
                ToolSessionOp::Read { .. } => "Read",
                ToolSessionOp::Finish { .. } => "Finish",
                ToolSessionOp::Abort { .. } => "Abort",
            }
        }
        fn coerce_step_to_allowed(step: ToolSessionOp, allowed_op: &str) -> Option<ToolSessionOp> {
            match allowed_op {
                "Open" => Some(ToolSessionOp::Open {
                    initial_input: None,
                    reason: Some("runtime coerced step to allowed Open".to_string()),
                }),
                "Send" => match step {
                    ToolSessionOp::Send { input, .. } => Some(ToolSessionOp::Send {
                        input,
                        reason: None,
                    }),
                    // Read and Send share the input field — safe coercion.
                    ToolSessionOp::Read { input, .. } => Some(ToolSessionOp::Send {
                        input,
                        reason: None,
                    }),
                    // Open has initial_input not input — cannot coerce safely; return None to
                    // let BAML retry with a properly-formed Send step.
                    _ => None,
                },
                "Read" => match step {
                    ToolSessionOp::Read { input, .. } => Some(ToolSessionOp::Read {
                        input,
                        reason: None,
                    }),
                    ToolSessionOp::Send { input, .. } => Some(ToolSessionOp::Read {
                        input,
                        reason: None,
                    }),
                    _ => None,
                },
                "Finish" => Some(ToolSessionOp::Finish {
                    reason: Some("runtime coerced step to allowed Finish".to_string()),
                }),
                "Abort" => Some(ToolSessionOp::Abort {
                    reason: Some("runtime coerced step to allowed Abort".to_string()),
                }),
                _ => None,
            }
        }
        fn allowed_ops_from_args(args: Option<&Value>) -> Option<Vec<String>> {
            let args_obj = args?.as_object()?;
            let session_context = args_obj.get("session_context")?.as_object()?;
            let allowed = session_context.get("allowed_ops")?.as_array()?;
            let mut out = Vec::with_capacity(allowed.len());
            for value in allowed {
                if let Some(op) = value.as_str() {
                    out.push(op.to_string());
                }
            }
            Some(out)
        }

        let mut first_step = plan.step;
        if let Some(allowed_ops) = allowed_ops_from_args(invocation_args) {
            let emitted_op = op_name(&first_step).to_string();
            if !allowed_ops.is_empty() && !allowed_ops.iter().any(|op| op == &emitted_op) {
                if allowed_ops.len() == 1 {
                    if let Some(coerced_step) = coerce_step_to_allowed(first_step, &allowed_ops[0])
                    {
                        tracing::warn!(
                            function = source_baml_function.unwrap_or("unknown_step_executor"),
                            emitted_op = %emitted_op,
                            coerced_op = %allowed_ops[0],
                            "Runtime coerced invalid step-executor op to single allowed op"
                        );
                        first_step = coerced_step;
                    } else {
                        return Err(BamlRtError::InvalidArgument(format!(
                            "runtime step executor contract violation ({}): expected op in [{}], got '{}'",
                            source_baml_function.unwrap_or("unknown_step_executor"),
                            allowed_ops.join(","),
                            emitted_op
                        )));
                    }
                } else {
                    return Err(BamlRtError::InvalidArgument(format!(
                        "runtime step executor contract violation ({}): expected op in [{}], got '{}'",
                        source_baml_function.unwrap_or("unknown_step_executor"),
                        allowed_ops.join(","),
                        emitted_op
                    )));
                }
            }
        }

        let plan_scope = scope.clone();
        let mut steps = vec![first_step];
        // Strict linear mode: exactly one fragment per invocation.
        // If this fragment is not Open, try to reuse an existing session.
        let mut session_id: Option<ToolSessionId> = self
            .tool_session_handle()
            .find_existing_session_for_scope_and_tool(&plan_scope, &tool_name)
            .await;
        if let Some(existing) = &session_id {
            tracing::debug!(
                tool_name = %tool_name,
                session_id = %existing,
                "Reusing existing session for single-fragment continuation",
            );
        }
        if let Some(first) = steps.first()
            && matches!(
                first,
                ToolSessionOp::Send { .. } | ToolSessionOp::Read { .. }
            )
            && session_id.is_none()
        {
            let can_auto_open = self
                .tool_registry
                .get_metadata(&tool_name)
                .map(|m| schema_allows_empty_open_input(&m.open_input_schema))
                .unwrap_or(false);
            if !can_auto_open {
                return Err(BamlRtError::InvalidArgument(
                    "step rejected: no open session; strict auto-open is allowed only when tool open_input is empty/optional"
                        .to_string(),
                ));
            }
            steps.insert(
                0,
                ToolSessionOp::Open {
                    initial_input: None,
                    reason: Some("auto-open for send fragment with no open session".to_string()),
                },
            );
        } else if let Some(first) = steps.first()
            && !matches!(first, ToolSessionOp::Open { .. })
            && session_id.is_none()
        {
            return Err(BamlRtError::InvalidArgument(
                "session fragment rejected: no open session for non-Open step".to_string(),
            ));
        }

        let mut last_output: Option<Value> = None;
        let mut streaming_outputs: Vec<Value> = Vec::new();
        let mut suspended = false;

        for step in steps {
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
                    if let Some(existing) = session_id.as_ref() {
                        // Accept idempotent Open only for unit/null input. For non-empty input,
                        // treat Open as an explicit reopen request and rotate the session.
                        let unit_or_null_open = match initial_input.as_ref() {
                            None => true,
                            Some(v) if v.is_null() => true,
                            Some(Value::Object(map)) if map.is_empty() => true,
                            _ => false,
                        };
                        if unit_or_null_open {
                            tracing::warn!(
                                tool = %tool_name,
                                session_id = %existing,
                                reason = ?reason,
                                "FSM step Open while session already open with unit/null input; reusing existing session"
                            );
                            last_output = Some(serde_json::json!({
                                "status": "open",
                                "session_id": existing.to_string()
                            }));
                            continue;
                        }
                        let existing = existing.clone();
                        tracing::info!(
                            tool = %tool_name,
                            previous_session_id = %existing,
                            reason = ?reason,
                            "FSM step Open with non-empty reopen input; aborting previous session before reopen"
                        );
                        self.tool_session_abort(
                            &existing,
                            Some(
                                "reopen requested by planner open with non-empty input".to_string(),
                            ),
                        )
                        .await?;
                    }
                    // For Open step, use initial_input if provided and non-null, otherwise empty object
                    let open_input = initial_input
                        .clone()
                        .and_then(|v| if v.is_null() { None } else { Some(v) })
                        .unwrap_or_else(empty_open_input);
                    let session = self
                        .open_tool_session(&plan_scope, &tool_name, open_input)
                        .await?;
                    last_output = Some(serde_json::json!({
                        "status": "open",
                        "session_id": session.to_string()
                    }));
                    session_id = Some(session.clone());
                }
                ToolSessionOp::Send { input, reason } => {
                    tracing::debug!(
                        tool = %tool_name,
                        reason = ?reason,
                        "FSM step: Send"
                    );
                    let current_session = session_id.clone().ok_or_else(|| {
                        BamlRtError::InvalidArgument(
                            "send step before open: FSM requires Open before Send".to_string(),
                        )
                    })?;
                    let normalized = normalize_plan_input(input)?;
                    match self
                        .tool_session_send(&current_session, normalized.clone())
                        .await
                    {
                        Ok(_) => {}
                        Err(BamlRtError::SessionLifecycle(
                            SessionLifecycleError::ToolSessionNotFound { .. },
                        )) => {
                            let refreshed = self
                                .tool_session_handle()
                                .find_existing_session_for_scope_and_tool(&plan_scope, &tool_name)
                                .await;
                            if let Some(refreshed_session) = refreshed {
                                if refreshed_session != current_session {
                                    tracing::warn!(
                                        tool = %tool_name,
                                        stale_session_id = %current_session,
                                        refreshed_session_id = %refreshed_session,
                                        "Recovered stale session id for Send step via scope+tool lookup"
                                    );
                                    session_id = Some(refreshed_session.clone());
                                    self.tool_session_send(&refreshed_session, normalized)
                                        .await?;
                                } else {
                                    return Err(BamlRtError::SessionLifecycle(
                                        SessionLifecycleError::ToolSessionNotFound {
                                            session_id: current_session.to_string(),
                                        },
                                    ));
                                }
                            } else {
                                return Err(BamlRtError::SessionLifecycle(
                                    SessionLifecycleError::ToolSessionNotFound {
                                        session_id: current_session.to_string(),
                                    },
                                ));
                            }
                        }
                        Err(BamlRtError::InvalidArgument(ref msg))
                            if msg.contains("Tool session already has input") =>
                        {
                            // The LLM skipped a Read step (e.g. due to BAML misparse of
                            // `{ "step": { "op": "Read" } }` without an `input` field).
                            // The session has pending input from a previous Send that was
                            // never consumed. Auto-drain with a Read to clear the state,
                            // then retry this Send.
                            tracing::warn!(
                                tool = %tool_name,
                                session_id = %current_session,
                                "Send step: session has pending input (LLM likely skipped Read); auto-draining with Read before retry"
                            );
                            let _ = self.tool_session_read(&current_session, Value::Null).await;
                            self.tool_session_send(&current_session, normalized).await?;
                        }
                        Err(err) => return Err(err),
                    }
                    // Send is fire-and-forget at the session level; the real response
                    // arrives via Read. If this is the last step in the plan (no
                    // subsequent Read), automatically drain the response so the caller
                    // never gets a bare {"status":"sent"} / null as the tool_result.
                    last_output = Some(serde_json::json!({ "status": "sent" }));
                }
                ToolSessionOp::Read { input, reason } => {
                    tracing::debug!(
                        tool = %tool_name,
                        reason = ?reason,
                        "FSM step: Read"
                    );
                    let mut current_session = session_id.clone().ok_or_else(|| {
                        BamlRtError::InvalidArgument("read step before open".to_string())
                    })?;
                    loop {
                        let read_input = normalize_plan_input(input.clone())?;
                        let step_result = match self
                            .tool_session_read(&current_session, read_input)
                            .await
                        {
                            Ok(step) => step,
                            Err(BamlRtError::SessionLifecycle(
                                SessionLifecycleError::ToolSessionNotFound { .. },
                            )) => {
                                let refreshed = self
                                    .tool_session_handle()
                                    .find_existing_session_for_scope_and_tool(
                                        &plan_scope,
                                        &tool_name,
                                    )
                                    .await;
                                if let Some(refreshed_session) = refreshed
                                    && refreshed_session != current_session
                                {
                                    tracing::warn!(
                                        tool = %tool_name,
                                        stale_session_id = %current_session,
                                        refreshed_session_id = %refreshed_session,
                                        "Recovered stale session id for Read step via scope+tool lookup"
                                    );
                                    session_id = Some(refreshed_session.clone());
                                    current_session = refreshed_session;
                                    continue;
                                }
                                return Err(BamlRtError::SessionLifecycle(
                                    SessionLifecycleError::ToolSessionNotFound {
                                        session_id: current_session.to_string(),
                                    },
                                ));
                            }
                            Err(err) => return Err(err),
                        };
                        match step_result {
                            ToolStep::Streaming { output } => {
                                let decorated =
                                    crate::quickjs_bridge::stream_yield::decorate_tool_chunk(
                                        &tool_name, &output,
                                    );
                                crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(
                                    &decorated,
                                );
                                if let Some(emitter) = self.effect_emitter.as_ref()
                                    && let Err(e) = emitter
                                        .emit(EffectEvent::ToolStreamChunk {
                                            context_id: plan_scope.context_id().clone(),
                                            chunk: decorated,
                                        })
                                        .await
                                {
                                    tracing::warn!(
                                        context_id = %plan_scope.context_id(),
                                        error = ?e,
                                        "tool stream chunk emit failed; chunk lost from provenance"
                                    );
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
                                if let Some(emitter) = self.effect_emitter.as_ref()
                                    && let Err(e) = emitter
                                        .emit(EffectEvent::ToolStreamChunk {
                                            context_id: plan_scope.context_id().clone(),
                                            chunk: decorated,
                                        })
                                        .await
                                {
                                    tracing::warn!(
                                        context_id = %plan_scope.context_id(),
                                        error = ?e,
                                        "tool stream chunk emit failed; chunk lost from provenance"
                                    );
                                }
                                streaming_outputs.push(output);
                                suspended = true;
                                tracing::debug!(
                                    tool = %tool_name,
                                    "FSM Read: breaking on Suspended (session left open for resume)"
                                );
                                break;
                            }
                            ToolStep::Done { output } => {
                                last_output = Some(serde_json::json!({
                                    "status": "done",
                                    "output": output
                                }));
                                // Do not clear session_id here: a subsequent step in this plan may be
                                // Finish or Abort, which must call tool_session_finish/abort. Only
                                // those steps clear session_id. Continuation plans ([Send, Read]
                                // without Open) reuse the session via find_existing_session_for_scope_and_tool.
                                break;
                            }
                            ToolStep::Error { error } => {
                                self.tool_session_abort(
                                    &current_session,
                                    Some(error.message.clone()),
                                )
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
                    // Preserve any Done output from a preceding Read step — Finish is
                    // session teardown, not a result-bearing operation.  Only write
                    // "finished" when there is no prior Done payload to return to the caller.
                    if last_output
                        .as_ref()
                        .and_then(|o| o.get("status"))
                        .and_then(serde_json::Value::as_str)
                        != Some("done")
                    {
                        last_output = Some(serde_json::json!({ "status": "finished" }));
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
                    last_output = Some(serde_json::json!({ "status": "aborted" }));
                }
            }
        }

        if !streaming_outputs.is_empty() {
            if let Some(ref done) = last_output {
                let decorated =
                    crate::quickjs_bridge::stream_yield::decorate_tool_chunk(&tool_name, done);
                crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(&decorated);
                if let Some(emitter) = self.effect_emitter.as_ref()
                    && let Err(e) = emitter
                        .emit(EffectEvent::ToolStreamChunk {
                            context_id: plan_scope.context_id().clone(),
                            chunk: decorated,
                        })
                        .await
                {
                    tracing::warn!(
                        context_id = %plan_scope.context_id(),
                        error = ?e,
                        "tool stream chunk emit failed on done; chunk lost from provenance"
                    );
                }
                streaming_outputs.push(done.clone());
            }
            return Ok(Value::Array(streaming_outputs));
        }

        last_output.ok_or_else(|| {
            BamlRtError::InvalidArgument(
                "Tool session plan produced no output; expected at least one step to yield a result. \
                 This is a runtime invariant violation — every plan execution must produce a non-null tool_result."
                    .to_string(),
            )
        })
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
            tool_session_scopes: Arc::new(DashMap::new()),
            tool_session_states: Arc::new(DashMap::new()),
            tool_session_effect_tokens: Arc::new(DashMap::new()),
            effect_emitter: None,
            conversation_context_provider: None,
            pending_parse_retry_policy: None,
            llm_secret_resolver: None,
            planning_resolver: Arc::new(DefaultPlanningCanonicalResolver),
            execution_sessions: Arc::new(DashMap::new()),
        }
    }
}

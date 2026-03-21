//! BAML runtime wrapper and function execution.
//!
//! [`BamlRuntimeManager`] owns the function registry, tool registry, and session
//! state. Tool call and plan extraction live in [`tool_extraction`]; session
//! open/send/next/finish/abort and plan execution remain here and use
//! scope-from-token for attribution.

pub(crate) mod tool_extraction;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
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
    ids::{ExternalId, TaskId},
    types::FunctionSignature,
};
use baml_rt_interceptor::InterceptorRegistry;
use baml_rt_llm_config::FnoxFileSecretResolver;
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

// Helper function to build metadata map with correlation/message/task/agent ids.
pub(crate) use crate::tool_execution::{ToolExecutionContext, resolve_planning_step};
use crate::{
    baml_execution::{
        BamlExecutor, BamlStreamInvocation, ConversationContextProvider, ParseRetryPolicy,
    },
    llm_client_registry::LlmSecretResolver,
    llm_resolver_adapter::SecretResolverToLlmAdapter,
    traits::{BamlFunctionExecutor, SchemaLoader},
};

/// Helper function for creating an empty open_input value.
///
/// This centralizes the pattern of using an empty JSON object as the default
/// open_input when none is provided.
fn empty_open_input() -> Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Matches provenance store step completion statuses for terminal steps.
fn is_planning_step_terminal_completed_status(status: &str) -> bool {
    let normalized = status.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "completed" | "done" | "step_completed" | "finished"
    )
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

pub(crate) fn tool_session_trace_enabled() -> bool {
    std::env::var("BAML_TRACE_TOOL_SESSION").is_ok()
}

pub(crate) fn tool_session_trace(message: &str) {
    if tool_session_trace_enabled() {
        tracing::trace!(message = %message, "[tool-session-trace]");
    }
}

pub(crate) fn completion_error_from(err: &BamlRtError) -> BamlRtError {
    match err {
        BamlRtError::SessionLifecycle(lifecycle) => {
            BamlRtError::SessionLifecycle(lifecycle.clone())
        }
        _ => BamlRtError::InvalidArgument(err.to_string()),
    }
}

/// Load a builder-generated JSON manifest from the project build directory.
/// Returns `None` if the file does not exist; logs on parse/read errors.
fn load_build_manifest<T: serde::de::DeserializeOwned>(
    project_root: &std::path::Path,
    filename: &str,
) -> Option<T> {
    let path = project_root.join(filename);
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<T>(&s) {
            Ok(val) => Some(val),
            Err(e) => {
                tracing::error!(
                    path = %path.display(),
                    error = %e,
                    "{filename} has invalid format — rebuild the agent"
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Could not read {filename}");
            None
        }
    }
}

/// Extract target.agent_package from open_input for delegation tools.
/// Supports system/internal_a2a, system/a2a, and support/a2aRelay (same Open shape).
pub(crate) fn extract_delegation_target_from_open_input(
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

pub use baml_rt_tools::{SessionPlanFunctionsMap, SessionPlanTypeName};

use crate::planning::DefaultPlanningCanonicalResolver;
pub use crate::{
    function_tool_manifest::FunctionToolManifest,
    planning::{
        CanonicalIntentSubmission, CanonicalPlanStepStatusChange, CanonicalPlanSubmission,
        PlanningCanonicalResolver, PlanningDynamicContext,
    },
};

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
    /// Builder-generated map: tool_name → single-tool step executor function name.
    /// Used by the shim for polymorphic auto-narrowing after Open.
    tool_step_executors: Option<std::collections::HashMap<String, String>>,
    /// Eagerly resolved function→tool manifest for drift scoring routing.
    function_tool_manifest: Arc<FunctionToolManifest>,
    interceptor_registry: Arc<TokioMutex<InterceptorRegistry>>,
    tool_session_scopes: Arc<DashMap<ToolSessionId, ToolSessionScope>>,
    tool_session_states: Arc<DashMap<ToolSessionId, ToolCallSessionState>>,
    /// Tokens for ToolStarted emitted in tool_session_send; completed in tool_session_read/finish/abort. Shared across handle() calls.
    tool_session_effect_tokens: Arc<DashMap<ToolSessionId, EffectStartToken<ToolKind>>>,
    /// Per-context archive ref tables. Maps context_id → RefTable for Read deref.
    archive_ref_tables: Arc<baml_rt_tools::archive_refs::ContextRefTables>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    conversation_context_provider: Option<Arc<dyn ConversationContextProvider>>,
    pending_parse_retry_policy: Option<ParseRetryPolicy>,
    /// When set, LLM API keys are injected via ClientRegistry (not env vars).
    llm_secret_resolver: Option<Arc<dyn LlmSecretResolver>>,
    planning_resolver: Arc<dyn PlanningCanonicalResolver>,
    execution_sessions: Arc<DashMap<String, crate::quickjs_bridge::ExecutionSession>>,
}

pub use crate::tool_session_handle::ToolSessionExecutionHandle;
pub(crate) use crate::tool_session_handle::{ToolCallSessionState, ToolSessionScope};

/// Emit a streaming chunk when tool_name comes from the session scope.
pub(crate) async fn emit_stream_chunk_static(
    effect_emitter: Option<&std::sync::Arc<dyn baml_rt_core::bus::EffectEmitter>>,
    context_id: &baml_rt_core::ids::ContextId,
    output: &Value,
    streaming_outputs: &mut Vec<Value>,
) {
    // Extract tool_name from chunk if available, otherwise use empty string.
    let tool_name = output
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let decorated = crate::quickjs_bridge::stream_yield::decorate_tool_chunk(tool_name, output);
    crate::quickjs_bridge::stream_yield::send_tool_stream_chunk(&decorated);
    if let Some(emitter) = effect_emitter
        && let Err(e) = emitter
            .emit(baml_rt_core::bus::EffectEvent::ToolStreamChunk {
                context_id: context_id.clone(),
                chunk: decorated,
            })
            .await
    {
        tracing::warn!(
            context_id = %context_id,
            error = ?e,
            "tool stream chunk emit failed; chunk lost from provenance"
        );
    }
    streaming_outputs.push(output.clone());
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
            tool_step_executors: None,
            function_tool_manifest: Arc::new(FunctionToolManifest::default()),
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(DashMap::new()),
            tool_session_states: Arc::new(DashMap::new()),
            tool_session_effect_tokens: Arc::new(DashMap::new()),
            archive_ref_tables: Arc::new(baml_rt_tools::archive_refs::ContextRefTables::new()),
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
    /// Also eagerly rebuilds the function→tool manifest.
    pub fn set_session_plan_functions(&mut self, map: Option<SessionPlanFunctionsMap>) {
        self.function_tool_manifest = Arc::new(
            map.as_ref()
                .map(|raw| FunctionToolManifest::build(raw, &self.tool_registry))
                .unwrap_or_default(),
        );
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
            archive_ref_tables: self.archive_ref_tables.clone(),
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
        // Provenance requires a successful LlmCall or ToolCall attributed to this plan step
        // before `PlanStepStatusChanged` may enter a terminal completed state. Coordinator-only
        // steps (scope parsing, formatting, etc.) have no real tool/LLM hop — synthesize a
        // bounded internal tool effect so the step gate and conversation_context stay consistent.
        if is_planning_step_terminal_completed_status(&canonical.new_status) {
            let context_id = scope.context_id().clone();
            let mut metadata_map = serde_json::Map::new();
            if let Some(correlation_id) = current_correlation_id() {
                metadata_map.insert(
                    "correlation_id".to_string(),
                    Value::String(correlation_id.to_string()),
                );
            }
            metadata_map.insert(
                "message_id".to_string(),
                Value::String(scope.message_id().as_str().to_owned()),
            );
            metadata_map.insert(
                "task_id".to_string(),
                Value::String(task_id.as_str().to_owned()),
            );
            metadata_map.insert(
                "agent_id".to_string(),
                Value::String(scope.agent_id().as_str().to_owned()),
            );
            metadata_map.insert(
                "plan_id".to_string(),
                Value::String(canonical.plan_id.as_str().to_string()),
            );
            metadata_map.insert(
                "step_id".to_string(),
                Value::String(canonical.step_id.as_str().to_string()),
            );
            metadata_map.insert(
                "phase".to_string(),
                Value::String("execution_session_complete".to_string()),
            );
            let tool_meta = ToolEffectMetadata {
                tool_name: "a2a/execution_session_step".to_string(),
                function_name: None,
                args: serde_json::json!({
                    "plan_id": canonical.plan_id.as_str(),
                    "step_id": canonical.step_id.as_str(),
                }),
                metadata: Value::Object(metadata_map),
                delegation_target: None,
            };
            let token = emitter.start_tool(context_id, tool_meta).await?;
            token
                .complete(
                    emitter.as_ref(),
                    0,
                    Outcome::Success,
                    Some(serde_json::json!({
                        "evidence_text": canonical.evidence_text,
                    })),
                )
                .await?;
        }
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
        let mut executor = BamlExecutor::load_il(&baml_src_dir, env_vars)?;

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

        self.session_plan_functions = load_build_manifest::<SessionPlanFunctionsMap>(
            project_root,
            "session_plan_functions.json",
        );
        self.tool_step_executors = load_build_manifest::<std::collections::HashMap<String, String>>(
            project_root,
            "tool_step_executors.json",
        );

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

        // Pass tool registry and interceptor registry to executor.
        // Build merged context tags (persisted history + intra-turn buffer) so the LLM
        // sees prior hops from this turn even when async provenance writes haven't landed.
        let interceptor_registry = Some(self.interceptor_registry.clone());
        let planning_step = resolve_planning_step(&self.execution_sessions, scope);
        let invocation_args = args.clone();
        let merged_tags = self.build_conversation_context_tags(scope).await?;
        let (result, completion) = executor
            .execute_function(
                scope,
                function_name,
                args,
                interceptor_registry,
                planning_step,
                &self.function_tool_manifest,
                merged_tags,
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

    /// The archive ref tables for this runtime instance.
    pub fn archive_ref_tables(&self) -> Arc<baml_rt_tools::archive_refs::ContextRefTables> {
        self.archive_ref_tables.clone()
    }

    /// Build conversation context tags for the given scope.
    /// Provenance is the single source of truth — writes are synchronous so this always
    /// reflects the current state of the conversation.
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

    /// Get the eagerly resolved function→tool manifest.
    pub fn function_tool_manifest(&self) -> Arc<FunctionToolManifest> {
        self.function_tool_manifest.clone()
    }

    /// Rebuild the function→tool manifest from the current session_plan_functions
    /// and tool_registry. Must be called AFTER tools are registered.
    pub fn rebuild_function_tool_manifest(&mut self) {
        self.function_tool_manifest = Arc::new(
            self.session_plan_functions
                .as_ref()
                .map(|raw| FunctionToolManifest::build(raw, &self.tool_registry))
                .unwrap_or_default(),
        );
        tracing::info!(
            function_tool_bindings = self.function_tool_manifest.len(),
            "Function-tool manifest built"
        );
    }

    /// Resolve the `SessionPolicy` for a bound tool (after Open has selected it).
    ///
    /// Unified policy resolution path used by both single-tool and polymorphic
    /// functions. Returns `Strict` (the safe default) when the tool is not found.
    pub fn resolve_session_policy_for_tool(
        &self,
        tool_name: &baml_rt_tools::ToolName,
    ) -> baml_rt_tools::SessionPolicy {
        self.tool_registry
            .get_metadata_by_name(tool_name)
            .map(|meta| meta.session_policy)
            .unwrap_or_default()
    }

    /// Resolve the single-tool step executor function name for a tool.
    /// Used by the shim's `__resolve_tool_step_executor` host helper for
    /// polymorphic auto-narrowing after Open.
    pub fn resolve_tool_step_executor(&self, tool_name: &str) -> Option<String> {
        self.tool_step_executors
            .as_ref()
            .and_then(|map| map.get(tool_name).cloned())
    }

    /// Resolve the `SessionPolicy` for a BAML step-executor function name.
    ///
    /// For single-tool functions (one candidate), resolves that tool's policy.
    /// For polymorphic functions (multiple candidates), returns `Strict` as safe
    /// default — the shim should use `resolve_session_policy_for_tool` with the
    /// selected tool name after Open.
    pub fn resolve_session_policy_for_function(
        &self,
        func_name: &str,
    ) -> baml_rt_tools::SessionPolicy {
        let candidates = match self
            .session_plan_functions
            .as_ref()
            .and_then(|m| m.get(func_name))
        {
            Some(c) => c,
            None => {
                tracing::debug!(
                    func = func_name,
                    "session policy: no candidates in session_plan_functions"
                );
                return baml_rt_tools::SessionPolicy::default();
            }
        };
        if candidates.len() == 1 {
            let tool_name = match tool_extraction::resolve_tool_name_from_plan_type_with_registry(
                &self.tool_registry,
                candidates[0].as_str(),
            ) {
                Ok(name) => name,
                Err(e) => {
                    tracing::debug!(func = func_name, plan_type = candidates[0].as_str(), error = %e, "session policy: tool resolution failed");
                    return baml_rt_tools::SessionPolicy::default();
                }
            };
            let policy = self.resolve_session_policy_for_tool(&tool_name);
            tracing::debug!(func = func_name, tool = %tool_name, policy = ?policy, "session policy: resolved from tool");
            policy
        } else {
            tracing::debug!(
                func = func_name,
                count = candidates.len(),
                "session policy: polymorphic, defaulting to Strict"
            );
            baml_rt_tools::SessionPolicy::default()
        }
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
        self.execute_tool(scope, &tool_name.to_string(), call.args)
            .await
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
        tracing::info!(
            baml_result = %baml_result,
            source_function = ?source_baml_function,
            "execute_tool_from_baml_result_or_value: entry"
        );
        let plan_result = extract_tool_session_plan(&baml_result).map_err(|e| {
            tracing::warn!(
                error = %e,
                source_function = ?source_baml_function,
                "Tool session plan extraction failed; LLM effect completed with rejection_reason and PromptRejected emitted in provenance"
            );
            e
        })?;
        tracing::info!(
            plan_found = plan_result.is_some(),
            selected_tool = ?plan_result.as_ref().and_then(|p| p.selected_tool.as_ref().map(|t| t.to_string())),
            "execute_tool_from_baml_result_or_value: plan extraction result"
        );
        if let Some(plan) = plan_result {
            let tool_name = if let (Some(func_name), Some(map)) =
                (source_baml_function, &self.session_plan_functions)
            {
                if let Some(candidates) = map.get(func_name) {
                    match candidates.as_slice() {
                        [single] => resolve_tool_name_from_plan_type_with_registry(
                            &self.tool_registry,
                            single.as_str(),
                        )
                        .ok(),
                        _ => {
                            // Polymorphic: Open step carries tool_name. Subsequent
                            // hops (Send/Read/Finish) don't — the tool was selected
                            // once and the session is already bound. Fall back to the
                            // already-open session for this scope.
                            plan.selected_tool.clone().or_else(|| {
                                self.tool_session_handle()
                                    .tool_session_scopes
                                    .iter()
                                    .find(|entry| entry.value().scope == *scope)
                                    .and_then(|entry| {
                                        baml_rt_tools::ToolName::parse(&entry.value().tool_name)
                                            .ok()
                                    })
                            })
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let tool_name = tool_name.ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "Session plan tool could not be resolved: no manifest entry for the invoking function, or polymorphic Open missing tool_name. Build the agent with the builder so session_plan_functions.json is present and up to date.".to_string(),
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
            return self
                .execute_tool(scope, &tool_name.to_string(), call.args)
                .await;
        }
        Ok(baml_result)
    }

    async fn resolve_tool_name_from_input(&self, input: &Value) -> Result<baml_rt_tools::ToolName> {
        resolve_tool_name_from_input_with_registry(&self.tool_registry, input)
    }

    /// Execute a typed tool session plan.
    /// Recover a stale session ID by looking up the live session for this scope + tool.
    async fn recover_stale_session(
        &self,
        plan_scope: &context::RuntimeScope,
        tool_name_str: &str,
        stale_session: &ToolSessionId,
        phase: &str,
    ) -> Result<ToolSessionId> {
        let refreshed = self
            .tool_session_handle()
            .find_existing_session_for_scope_and_tool(plan_scope, tool_name_str)
            .await;
        match refreshed {
            Some(ref fresh) if fresh != stale_session => {
                tracing::warn!(
                    tool = %tool_name_str,
                    stale_session_id = %stale_session,
                    refreshed_session_id = %fresh,
                    "Recovered stale session id for {} step via scope+tool lookup",
                    phase
                );
                Ok(fresh.clone())
            }
            _ => Err(BamlRtError::SessionLifecycle(
                SessionLifecycleError::ToolSessionNotFound {
                    session_id: stale_session.to_string(),
                },
            )),
        }
    }

    /// The plan is a sequence of typed `ToolSessionOp` operations that must follow FSM rules:
    /// - First operation must be Open
    /// - Subsequent operations must be Send/Read/Finish/Abort (after Open)
    /// - After Finish/Abort, session is closed
    async fn execute_tool_session_plan(
        &self,
        scope: &context::RuntimeScope,
        tool_name: baml_rt_tools::ToolName,
        plan: ToolSessionPlan,
        _source_baml_function: Option<&str>,
        _invocation_args: Option<&Value>,
    ) -> Result<Value> {
        let tool_name_str = tool_name.to_string();

        let first_step = plan.step;

        let plan_scope = scope.clone();
        let mut steps = vec![first_step];
        // Strict linear mode: exactly one fragment per invocation.
        // If this fragment is not Open, try to reuse an existing session.
        let mut session_id: Option<ToolSessionId> = self
            .tool_session_handle()
            .find_existing_session_for_scope_and_tool(&plan_scope, &tool_name_str)
            .await;
        if let Some(existing) = &session_id {
            tracing::debug!(
                tool_name = %tool_name_str,
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
                .get_metadata(&tool_name_str)
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

        for step in steps {
            match step {
                ToolSessionOp::Open {
                    initial_input,
                    reason,
                } => {
                    tracing::debug!(
                        tool = %tool_name_str,
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
                                tool = %tool_name_str,
                                session_id = %existing,
                                reason = ?reason,
                                "FSM step Open while session already open with unit/null input; reusing existing session"
                            );
                            last_output = Some(serde_json::json!({
                                "status": "open",
                                "session_id": existing.to_string(),
                                "tool_name": tool_name_str
                            }));
                            continue;
                        }
                        let existing = existing.clone();
                        tracing::info!(
                            tool = %tool_name_str,
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
                        .open_tool_session(&plan_scope, &tool_name_str, open_input)
                        .await?;
                    last_output = Some(serde_json::json!({
                        "status": "open",
                        "session_id": session.to_string(),
                        "tool_name": tool_name_str
                    }));
                    // Emit session step so conversation_context reflects Open synchronously.
                    if let Some(emitter) = self.effect_emitter.as_ref() {
                        let _ = emitter
                            .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                context_id: plan_scope.context_id().clone(),
                                tool_name: tool_name_str.clone(),
                                session_id: session.to_string(),
                                op: baml_rt_core::bus::SessionStepOp::Open,
                            })
                            .await;
                    }
                    session_id = Some(session.clone());
                }
                ToolSessionOp::Send { input, reason } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        reason = ?reason,
                        "FSM step: Send (blocking)"
                    );
                    let current_session = session_id.clone().ok_or_else(|| {
                        BamlRtError::InvalidArgument(
                            "send step before open: FSM requires Open before Send".to_string(),
                        )
                    })?;
                    let normalized = normalize_plan_input(input)?;
                    // Send blocks until Done; returns archive ref + header.
                    let send_result = self
                        .tool_session_handle()
                        .tool_session_send_blocking(
                            &current_session,
                            normalized,
                            &plan_scope,
                            &self.archive_ref_tables,
                            std::time::Duration::from_secs(300),
                        )
                        .await;
                    // Handle stale session: recover and retry once.
                    let send_result = match send_result {
                        Err(BamlRtError::SessionLifecycle(
                            SessionLifecycleError::ToolSessionNotFound { .. },
                        )) => {
                            let fresh = self
                                .recover_stale_session(
                                    &plan_scope,
                                    &tool_name_str,
                                    &current_session,
                                    "Send",
                                )
                                .await?;
                            session_id = Some(fresh.clone());
                            self.tool_session_handle()
                                .tool_session_send_blocking(
                                    &fresh,
                                    normalize_plan_input(serde_json::Value::Null)?,
                                    &plan_scope,
                                    &self.archive_ref_tables,
                                    std::time::Duration::from_secs(300),
                                )
                                .await?
                        }
                        other => other?,
                    };
                    last_output = Some(serde_json::json!({
                        "status": "done",
                        // Human-readable header for LLM: "@1 tool 'summary' [N lines, KB]"
                        "output": send_result.header,
                        "archive_ref": send_result.archive_ref.to_string(),
                        // Raw structured output for TypeScript orchestrators that need
                        // to parse the result without going through archive Read.
                        "result": send_result.output,
                    }));
                    // Emit SendDone session step so conversation_context sees the archive.
                    if let Some(emitter) = self.effect_emitter.as_ref() {
                        let _ = emitter
                            .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                context_id: plan_scope.context_id().clone(),
                                tool_name: tool_name_str.clone(),
                                session_id: current_session.to_string(),
                                op: baml_rt_core::bus::SessionStepOp::SendDone {
                                    archive_ref: send_result.archive_ref.to_string(),
                                    header: send_result.header.clone(),
                                },
                            })
                            .await;
                    }
                }
                ToolSessionOp::Read {
                    archive_ref,
                    offset,
                    limit,
                    grep,
                    reason,
                } => {
                    tracing::debug!(
                        tool = %tool_name_str,
                        archive_ref = %archive_ref,
                        reason = ?reason,
                        "FSM step: Read (archive deref)"
                    );
                    // Pure archive deref — no tool I/O. Look up the archived entry and paginate.
                    let context_id = plan_scope.context_id().as_str().to_string();
                    let ref_table = baml_rt_tools::archive_refs::get_or_create_ref_table(
                        &self.archive_ref_tables,
                        &context_id,
                    );
                    let entry = ref_table.get(archive_ref).ok_or_else(|| {
                        BamlRtError::InvalidArgument(format!(
                            "Read step: archive ref {} not found in session context",
                            archive_ref
                        ))
                    })?;
                    let page = baml_rt_tools::archive_read::grep_paginate(
                        &entry.content,
                        grep.as_ref(),
                        offset,
                        limit,
                    );
                    let formatted = baml_rt_tools::archive_read::format_cat_n(&page.lines);
                    let header = entry.display_header(archive_ref);
                    let line_range = if page.lines.is_empty() {
                        String::new()
                    } else {
                        let first = page
                            .lines
                            .first()
                            .map(|l| l.original_line_number)
                            .unwrap_or(1);
                        let last = page
                            .lines
                            .last()
                            .map(|l| l.original_line_number)
                            .unwrap_or(1);
                        let more = if page.has_more {
                            format!(
                                "\n--- {} more lines (Read @{} offset={} for next page) ---",
                                page.total_matched - page.next_offset,
                                archive_ref,
                                page.next_offset,
                            )
                        } else {
                            String::new()
                        };
                        format!(
                            "\nlines {first}-{last} of {}:\n{formatted}{more}",
                            page.total_matched
                        )
                    };
                    let read_output = serde_json::json!({
                        "status": "done",
                        "output": format!("{header}{line_range}"),
                        "has_more": page.has_more,
                        "next_offset": page.next_offset,
                    });
                    last_output = Some(read_output.clone());

                    // Emit ToolStarted/ToolCompleted for the Read FSM step so the FE
                    // can display archive_ref, grep, offset as tool call args.
                    if let Some(emitter) = self.effect_emitter.as_ref() {
                        let grep_str = grep.as_ref().map(|g| g.pattern_text().to_string());
                        let read_args = serde_json::json!({
                            "archive_ref": archive_ref.to_string(),
                            "grep": grep_str,
                            "offset": offset.0,
                            "limit": limit.get(),
                        });
                        let read_meta = baml_rt_core::bus::ToolEffectMetadata {
                            tool_name: tool_name_str.clone(),
                            function_name: None,
                            args: read_args,
                            metadata: crate::tool_execution::build_metadata_map_with_phase(
                                &plan_scope,
                                Some("read"),
                            ),
                            delegation_target: None,
                        };
                        if let Ok(token) = emitter
                            .start_tool(plan_scope.context_id().clone(), read_meta)
                            .await
                        {
                            let _ = token
                                .complete(
                                    emitter.as_ref(),
                                    0,
                                    baml_rt_core::semantics::Outcome::Success,
                                    Some(read_output),
                                )
                                .await;
                        }

                        // Emit ToolSessionStep::Read for conversation history.
                        let _ = emitter
                            .emit(baml_rt_core::bus::EffectEvent::ToolSessionStep {
                                context_id: plan_scope.context_id().clone(),
                                tool_name: tool_name_str.clone(),
                                session_id: session_id
                                    .as_ref()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default(),
                                op: baml_rt_core::bus::SessionStepOp::Read {
                                    archive_ref: archive_ref.to_string(),
                                    grep: grep_str,
                                    offset: offset.0,
                                    limit: limit.get(),
                                },
                            })
                            .await;
                    }
                }
                ToolSessionOp::Finish { reason } => {
                    tracing::debug!(
                        tool = %tool_name_str,
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
                        tool = %tool_name_str,
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
            tool_step_executors: None,
            function_tool_manifest: Arc::new(FunctionToolManifest::default()),
            interceptor_registry: Arc::new(TokioMutex::new(InterceptorRegistry::new())),
            tool_session_scopes: Arc::new(DashMap::new()),
            tool_session_states: Arc::new(DashMap::new()),
            tool_session_effect_tokens: Arc::new(DashMap::new()),
            archive_ref_tables: Arc::new(baml_rt_tools::archive_refs::ContextRefTables::new()),
            effect_emitter: None,
            conversation_context_provider: None,
            pending_parse_retry_policy: None,
            llm_secret_resolver: None,
            planning_resolver: Arc::new(DefaultPlanningCanonicalResolver),
            execution_sessions: Arc::new(DashMap::new()),
        }
    }
}

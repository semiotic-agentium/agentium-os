//! BAML function execution engine
//!
//! This module executes BAML functions using the compiled IL (Intermediate Language)
//! from the BAML compiler.

use std::{collections::HashMap, path::Path, str::FromStr, sync::Arc, time::Instant};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, InvocationKind, Outcome, Result, bus::EffectEmitter, context};
use baml_rt_interceptor::{InterceptorDecision, InterceptorRegistry};
use baml_rt_tools::ToolRegistry;
use baml_runtime::{
    BamlRuntime, FunctionResultStream, RuntimeContextManager,
    client_registry::{ClientProperty, ClientProvider, ClientRegistry},
};
use baml_types::{BamlMap, BamlValue};
use serde_json::Value;
use tokio::{
    sync::Mutex,
    time::{Duration, sleep},
};

use crate::{
    baml_collector::{BamlLLMCollector, LLMCompletionHandle},
    baml_pre_execution::{extract_context_from_http_request, intercept_llm_call_pre_execution},
    llm_client_registry::{LlmSecretResolver, build_llm_client_registry},
};

fn planner_state_telemetry(args: &Value) -> Option<(usize, bool, usize, Option<String>)> {
    let obj = args.as_object()?;
    let context = obj.get("session_context").and_then(Value::as_object)?;
    let args_bytes = serde_json::to_vec(args).map_or(0, |v| v.len());
    let session_open = context
        .get("session_open")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status_token = context
        .get("status_token")
        .and_then(Value::as_str)
        .unwrap_or("");
    let allowed_ops_len = context
        .get("allowed_ops")
        .and_then(Value::as_array)
        .map_or(0usize, std::vec::Vec::len);
    let payload_bytes = serde_json::to_vec(context).map_or(0, |v| v.len());
    let token = if status_token.is_empty() {
        None
    } else {
        Some(status_token.to_string())
    };
    Some((
        args_bytes,
        session_open,
        payload_bytes.saturating_add(allowed_ops_len),
        token,
    ))
}

fn derive_base_url(url: &str) -> Option<String> {
    for suffix in [
        "/chat/completions",
        "/responses",
        "/v1/messages",
        "/messages",
    ] {
        if let Some(idx) = url.rfind(suffix) {
            return Some(url[..idx].to_string());
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn build_session_fsm_client_registry(
    runtime: &BamlRuntime,
    scope: &context::RuntimeScope,
    function_name: &str,
    args: &Value,
    force_session_fsm_client: bool,
    params: &BamlMap<String, BamlValue>,
    ctx_manager: &RuntimeContextManager,
    llm_secret_resolver: Option<&dyn LlmSecretResolver>,
    planning_step: Option<(&str, &str)>,
) -> Result<Option<ClientRegistry>> {
    if !force_session_fsm_client && planner_state_telemetry(args).is_none() {
        return Ok(None);
    }

    let request = runtime
        .build_request(
            function_name.to_string(),
            params,
            ctx_manager,
            None,
            None,
            HashMap::new(),
            false,
        )
        .await
        .map_err(|e| BamlRtError::RequestBuildFailed { source: e.into() })?;
    let context = extract_context_from_http_request(scope, &request, function_name, planning_step)?;
    let Some(url) = context
        .metadata
        .get("url")
        .and_then(Value::as_str)
        .map(ToString::to_string)
    else {
        return Ok(None);
    };

    let provider = if url.contains("openrouter.ai") {
        "openai-generic"
    } else {
        "openai"
    };
    let scope_id = scope.agent_id().as_str();
    let api_key = if provider == "openai-generic" {
        llm_secret_resolver.and_then(|r| {
            r.resolve_llm_api_key(scope_id, "OPENROUTER_API_KEY")
                .map(|(v, _)| v)
        })
    } else {
        llm_secret_resolver.and_then(|r| {
            r.resolve_llm_api_key(scope_id, "OPENAI_API_KEY")
                .map(|(v, _)| v)
        })
    };

    let mut options = BamlMap::new();
    options.insert("model".to_string(), BamlValue::String(context.model));
    let mut reasoning_options = BamlMap::new();
    reasoning_options.insert("enabled".to_string(), BamlValue::Bool(false));
    options.insert("reasoning".to_string(), BamlValue::Map(reasoning_options));
    if let Some(base_url) = derive_base_url(&url) {
        options.insert("base_url".to_string(), BamlValue::String(base_url));
    }
    if let Some(key) = api_key {
        options.insert("api_key".to_string(), BamlValue::String(key));
    }

    let client_name = "__session_fsm_runtime_client";
    let client_provider = ClientProvider::from_str(provider).map_err(|e| {
        BamlRtError::InvalidArgument(format!(
            "unsupported runtime session client provider '{provider}': {e}"
        ))
    })?;
    let client_property =
        ClientProperty::new(client_name.to_string(), client_provider, None, options);

    let mut registry = ClientRegistry::new();
    registry.add_client(client_property);
    registry.set_primary(client_name.to_string());
    Ok(Some(registry))
}

/// Bundles a BAML streaming invocation: stream, context manager, client registry, and env vars.
pub struct BamlStreamInvocation {
    pub stream: FunctionResultStream,
    pub ctx_manager: RuntimeContextManager,
    pub client_registry_opt: Option<baml_runtime::client_registry::ClientRegistry>,
    pub env_vars: HashMap<String, String>,
}

impl std::fmt::Debug for BamlStreamInvocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BamlStreamInvocation")
            .field("stream", &"<FunctionResultStream>")
            .field("ctx_manager", &self.ctx_manager)
            .field("client_registry_opt", &self.client_registry_opt)
            .field("env_vars", &self.env_vars)
            .finish()
    }
}

/// Policy for retrying BAML calls when the LLM response fails to parse.
///
/// Allows tests to use `max_attempts: 1` to avoid retry delay and non-determinism.
#[derive(Clone, Debug)]
pub struct ParseRetryPolicy {
    /// Total attempts (initial call + retries). Must be at least 1.
    pub max_attempts: u32,
    /// Base delay in milliseconds between attempts. Delay for attempt N is `delay_ms * N`.
    pub delay_ms: u64,
}

impl Default for ParseRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            delay_ms: 300,
        }
    }
}

/// BAML execution engine that executes BAML IL
#[async_trait]
pub trait ConversationContextProvider: Send + Sync {
    /// Return conversation-history payload for the current runtime scope.
    ///
    /// The payload is injected as `ctx.tags.conversation_history` in BAML templates.
    /// Provider is called with the runtime scope of the current invocation. For resume,
    /// scope must be TaskScoped with the session's `context_id` so history includes
    /// prior turns. Used in both stream and non-stream paths when conversation context
    /// is injected.
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>>;
}

pub struct BamlExecutor {
    runtime: Arc<BamlRuntime>,
    tool_registry: Arc<ToolRegistry>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    conversation_context_provider: Option<Arc<dyn ConversationContextProvider>>,
    parse_retry_policy: ParseRetryPolicy,
    /// When set, LLM API keys are injected via ClientRegistry (not env vars).
    llm_secret_resolver: Option<Arc<dyn LlmSecretResolver>>,
}

impl BamlExecutor {
    /// Load BAML IL from the compiled output
    ///
    /// This loads the BAML runtime from the baml_src directory using from_directory.
    /// `env_vars` should include resolved LLM secrets (e.g. OPENROUTER_API_KEY) so that
    /// BAML schema's `api_key env.X` references resolve correctly without relying on
    /// std::env::var. Pass the result of `BamlRuntimeManager::resolve_secrets_as_env_vars()`.
    pub fn load_il(
        baml_src_dir: &Path,
        tool_registry: Arc<ToolRegistry>,
        env_vars: HashMap<String, String>,
    ) -> Result<Self> {
        tracing::info!(?baml_src_dir, "Loading BAML runtime from directory");

        let feature_flags = internal_baml_core::feature_flags::FeatureFlags::default();

        let runtime = BamlRuntime::from_directory(baml_src_dir, env_vars, feature_flags)
            .map_err(|e| BamlRtError::RuntimeLoadFailed { source: e })?;

        Ok(Self {
            runtime: Arc::new(runtime),
            tool_registry,
            effect_emitter: None,
            conversation_context_provider: None,
            parse_retry_policy: ParseRetryPolicy::default(),
            llm_secret_resolver: None,
        })
    }

    /// Set the LLM secret resolver for ClientRegistry-based API key injection.
    /// When set, API keys are resolved via the resolver (e.g. fnox + llm mapping) and
    /// passed to BAML as ClientRegistry, not env vars.
    pub fn set_llm_secret_resolver(&mut self, resolver: Arc<dyn LlmSecretResolver>) {
        self.llm_secret_resolver = Some(resolver);
    }

    /// Set the policy for retrying on parse failure (e.g. use `max_attempts: 1` in tests).
    pub fn set_parse_retry_policy(&mut self, policy: ParseRetryPolicy) {
        self.parse_retry_policy = policy;
    }

    /// Set the effect emitter (for effects-first liveness)
    pub fn set_effect_emitter(&mut self, emitter: Arc<dyn EffectEmitter>) {
        self.effect_emitter = Some(emitter);
    }

    /// Set a provider that supplies conversation context for template tags.
    pub fn set_conversation_context_provider(
        &mut self,
        provider: Arc<dyn ConversationContextProvider>,
    ) {
        self.conversation_context_provider = Some(provider);
    }

    /// Execute a BAML function using the compiled IL.
    /// Returns `(value, Some(handle))` on success when the manager will run tool/plan execution;
    /// the manager must call `handle.complete(Success, None)` or `handle.complete(Failure, Some(reason))` after.
    pub async fn execute_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: Value,
        force_session_fsm_client: bool,
        interceptor_registry: Option<Arc<Mutex<InterceptorRegistry>>>,
        planning_step: Option<(String, String)>,
    ) -> Result<(Value, Option<LLMCompletionHandle>)> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            "Executing BAML function from IL"
        );

        // Convert JSON args to BamlValue map
        let params = self.json_to_baml_map(&args)?;

        // Build ClientRegistry from resolver; LLM keys are never passed via env vars.
        let scope_id = scope.agent_id().as_str();
        let llm_registry_result = build_llm_client_registry(
            self.runtime.as_ref(),
            self.llm_secret_resolver.as_deref(),
            scope_id,
        )
        .map_err(|e| BamlRtError::ClientRegistryBuild { source: e })?;
        let env_vars = HashMap::new();
        let tags = None;

        // Track execution start time for effect completion (our clock, not BAML trace)
        let start_time = Instant::now();

        // Create collector for LLM interception if registry is provided (Arc so we can pass a clone to completion handle).
        let collector: Option<Arc<BamlLLMCollector>> =
            interceptor_registry.as_ref().map(|registry| {
                let mut coll = BamlLLMCollector::new(registry.clone(), function_name.to_string());
                if let Some(ref emitter) = self.effect_emitter {
                    coll.set_effect_emitter(emitter.clone());
                }
                Arc::new(coll)
            });

        // Pre-execution interception: intercept LLM calls before they're sent
        let context_tags = self.build_conversation_context_tags(scope).await?;
        let ctx_manager = self.create_ctx_manager_for_scope(scope, context_tags)?;
        let planning_step_refs = planning_step
            .as_ref()
            .map(|(plan_id, step_id)| (plan_id.as_str(), step_id.as_str()));
        let session_client_registry = build_session_fsm_client_registry(
            &self.runtime,
            scope,
            function_name,
            &args,
            force_session_fsm_client,
            &params,
            &ctx_manager,
            self.llm_secret_resolver.as_deref(),
            planning_step_refs,
        )
        .await?;
        if let Some(ref registry) = interceptor_registry {
            match intercept_llm_call_pre_execution(
                &self.runtime,
                scope,
                function_name,
                &params,
                &ctx_manager,
                registry,
                env_vars.clone(),
                session_client_registry
                    .as_ref()
                    .or(llm_registry_result.registry()),
                llm_registry_result.secret_keys_accessed(),
                InvocationKind::Invoke,
                self.effect_emitter.as_ref(),
                collector.as_ref().map(Arc::as_ref),
                planning_step_refs,
            )
            .await
            {
                Ok(InterceptorDecision::Allow) => {
                    // Allow the call to proceed
                }
                Ok(InterceptorDecision::Block(msg)) => {
                    if let Some(ref collector) = collector {
                        collector
                            .complete_pending_effects(Outcome::Failure, 0, None, None)
                            .await;
                    }
                    return Err(BamlRtError::BamlRuntime(format!(
                        "LLM call blocked by interceptor: {}",
                        msg
                    )));
                }
                Ok(InterceptorDecision::Substitute(value)) => {
                    if let Some(ref collector) = collector {
                        collector
                            .complete_pending_effects(Outcome::Success, 0, None, None)
                            .await;
                    }
                    return Ok((value, None));
                }
                Err(e) => {
                    if let Some(ref collector) = collector {
                        collector
                            .complete_pending_effects(Outcome::Failure, 0, None, None)
                            .await;
                    }
                    return Err(e);
                }
            }
        }

        // Wire up the collector to track function execution
        // Note: We track the function call by passing the collector, but we also need
        // to manually track the call_id so we can process trace events later
        let collectors = collector
            .as_ref()
            .map(|collector| vec![collector.as_collector()]);

        let max_attempts = self.parse_retry_policy.max_attempts.max(1);
        let delay_ms = self.parse_retry_policy.delay_ms;
        let mut last_parse_err: Option<anyhow::Error> = None;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                let backoff_ms = delay_ms * attempt as u64;
                tracing::warn!(
                    function = function_name,
                    attempt = attempt + 1,
                    delay_ms = backoff_ms,
                    "Parse failed, retrying BAML call"
                );
                sleep(Duration::from_millis(backoff_ms)).await;
            }

            let planner_metrics = planner_state_telemetry(&args);
            tracing::info!(
                function = function_name,
                context_id = %scope.context_id().as_str(),
                message_id = %scope.message_id().as_str(),
                task_id = %scope.task_id_opt().map(|id| id.as_str()).unwrap_or("none"),
                attempt = attempt + 1,
                planner_hop = planner_metrics.is_some(),
                planner_args_bytes = planner_metrics.as_ref().map(|(bytes, _, _, _)| *bytes),
                planner_session_open = planner_metrics.as_ref().map(|(_, open, _, _)| *open),
                planner_last_tool_output_bytes = planner_metrics.as_ref().map(|(_, _, bytes, _)| *bytes),
                planner_last_status_token = planner_metrics
                    .as_ref()
                    .and_then(|(_, _, _, token)| token.clone()),
                "BAML call_function: start"
            );
            let attempt_start = Instant::now();
            let cancel_tripwire = baml_runtime::TripWire::new(None);
            // Use collectors only on first attempt to avoid duplicate trace events on retries
            let attempt_collectors = if attempt == 0 {
                collectors.clone()
            } else {
                None
            };
            let (result, _call_id) = self
                .runtime
                .call_function(
                    function_name.to_string(),
                    &params,
                    &ctx_manager,
                    None, // type_builder
                    session_client_registry
                        .as_ref()
                        .or(llm_registry_result.registry()),
                    attempt_collectors,
                    env_vars.clone(),
                    tags,
                    cancel_tripwire,
                )
                .await;

            if let Err(ref e) = result {
                tracing::warn!(
                    function = function_name,
                    error = ?e,
                    hop_elapsed_ms = attempt_start.elapsed().as_millis() as u64,
                    elapsed_ms = start_time.elapsed().as_millis() as u64,
                    "BAML call_function: error"
                );
                // Complete effect and return immediately; execution errors are not retried
                if let Some(ref collector) = collector {
                    collector
                        .complete_pending_effects(
                            Outcome::Failure,
                            start_time.elapsed().as_millis() as u64,
                            None,
                            None,
                        )
                        .await;
                }
            };
            let function_result = match result {
                Ok(r) => r,
                Err(e) => return Err(BamlRtError::ExecutionFailed { source: e }),
            };
            let parsed_result = function_result.parsed().as_ref().ok_or_else(|| {
                BamlRtError::BamlRuntime("Function returned no parsed result".to_string())
            })?;

            match parsed_result.as_ref() {
                Ok(parsed) => {
                    tracing::info!(
                        function = function_name,
                        hop_elapsed_ms = attempt_start.elapsed().as_millis() as u64,
                        elapsed_ms = start_time.elapsed().as_millis() as u64,
                        "BAML call_function: ok"
                    );
                    // Defer LLM completion until after tool plan execution: plan extraction/execution
                    // failure is part of the LLM call outcome in the graph (invalid output from the LLM).
                    let json_value = serde_json::to_value(parsed.serialize_partial())
                        .map_err(BamlRtError::Json)?;

                    let elapsed_ms = start_time.elapsed().as_millis() as u64;
                    match maybe_execute_tool_from_result(&self.tool_registry, &json_value, scope)
                        .await
                    {
                        Err(e) => {
                            if let Some(ref collector) = collector {
                                collector
                                    .complete_pending_effects(
                                        Outcome::Failure,
                                        elapsed_ms,
                                        None,
                                        None,
                                    )
                                    .await;
                            }
                            return Err(e);
                        }
                        Ok(None) => {
                            // Defer: manager will run execute_tool_from_baml_result_or_value (e.g.
                            // session plans). Do NOT call process_trace_events here — it always
                            // passes Ok to the interceptor, so we'd write Success. The real outcome
                            // (Success or Failure from plan extraction/execution) comes from the
                            // effect completion. If we notified here, we'd race with the correct
                            // outcome and risk showing Success in the sequence diagram when the
                            // plan had empty steps.
                            let handle = collector.as_ref().map(|c| {
                                BamlLLMCollector::completion_handle(
                                    c.clone(),
                                    start_time,
                                    scope.clone(),
                                    json_value.clone(),
                                )
                            });
                            return Ok((json_value, handle));
                        }
                        Ok(Some(tool_result)) => {
                            // Immediate tool execution completed; no plan to run. Safe to notify
                            // interceptors now.
                            if let Some(ref collector) = collector
                                && let Err(e) = collector.process_trace_events(scope).await
                            {
                                tracing::warn!(
                                    error = ?e,
                                    "Failed to process trace events for LLM interception"
                                );
                            }
                            if let Some(ref collector) = collector {
                                collector
                                    .complete_pending_effects(
                                        Outcome::Success,
                                        elapsed_ms,
                                        None,
                                        Some(json_value.clone()),
                                    )
                                    .await;
                            }
                            return Ok((tool_result, None));
                        }
                    }
                }
                Err(e) => {
                    last_parse_err = Some(anyhow::Error::msg(e.to_string()));
                    if attempt + 1 >= max_attempts {
                        if let Some(ref collector) = collector {
                            collector
                                .complete_pending_effects(
                                    Outcome::Failure,
                                    start_time.elapsed().as_millis() as u64,
                                    None,
                                    None,
                                )
                                .await;
                        }
                        return Err(BamlRtError::ParsedResultFailed {
                            source: last_parse_err.expect(
                                "last_parse_err set in Err branch when max_attempts exhausted",
                            ),
                        });
                    }
                }
            }
        }

        // Unreachable (loop returns on success or last attempt), but satisfy type checker
        Err(BamlRtError::ParsedResultFailed {
            source: last_parse_err.unwrap_or_else(|| anyhow::Error::msg("Parse failed")),
        })
    }

    /// Execute a BAML function with streaming support
    ///
    /// Returns a [`BamlStreamInvocation`] bundling stream, context manager, client registry, and env vars.
    /// Run it with `invocation.stream.run(..., &invocation.ctx_manager, None, invocation.client_registry_opt.as_ref(), invocation.env_vars)`.
    /// Pass `context_tags` (e.g. from `build_conversation_context_tags`) for resume so BAML sees prior turns.
    pub fn execute_function_stream(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: Value,
        context_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<BamlStreamInvocation> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            has_context_tags = context_tags.as_ref().map_or(0, |m| m.len()) > 0,
            "Starting streaming execution of BAML function"
        );

        // Convert JSON args to BamlValue map
        let params = self.json_to_baml_map(&args)?;
        let ctx_manager = self.create_ctx_manager_for_scope(scope, context_tags)?;

        // Build ClientRegistry from resolver; LLM keys are never passed via env vars.
        let scope_id = scope.agent_id().as_str();
        let llm_registry_result = build_llm_client_registry(
            self.runtime.as_ref(),
            self.llm_secret_resolver.as_deref(),
            scope_id,
        )
        .map_err(|e| BamlRtError::ClientRegistryBuild { source: e })?;
        let client_registry_opt = llm_registry_result.into_registry();
        let env_vars = HashMap::new();
        let tags = None;
        let cancel_tripwire = baml_runtime::TripWire::new(None);

        let stream = self
            .runtime
            .stream_function(
                function_name.to_string(),
                &params,
                &ctx_manager,
                None, // type_builder
                client_registry_opt.as_ref(),
                None, // collectors
                env_vars.clone(),
                cancel_tripwire,
                tags,
            )
            .map_err(|e| BamlRtError::FunctionStreamCreation { source: e })?;

        Ok(BamlStreamInvocation {
            stream,
            ctx_manager,
            client_registry_opt,
            env_vars,
        })
    }

    /// Create a context manager tied to an explicit runtime scope.
    /// We pass BamlValue::Null for the "language"
    pub fn create_ctx_manager_for_scope(
        &self,
        scope: &context::RuntimeScope,
        extra_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<RuntimeContextManager> {
        let _ = scope; // scope used only for extra_tags from conversation context
        let ctx_manager = self.runtime.create_ctx_manager(BamlValue::Null, None);

        if let Some(tags) = extra_tags
            && !tags.is_empty()
        {
            ctx_manager.upsert_tags(tags);
        }

        Ok(ctx_manager)
    }

    /// Build conversation-history tags for the given scope (used by stream path for resume).
    /// Returns None if no provider is set or provider returns empty.
    pub async fn build_conversation_context_tags(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        let Some(provider) = self.conversation_context_provider.as_ref() else {
            return Ok(None);
        };

        let Some(payload) = provider.conversation_history_json(scope).await? else {
            return Ok(None);
        };

        let mut tags = HashMap::new();
        if let Some(obj) = payload.as_object()
            && obj.contains_key("conversation_history")
        {
            for key in ["conversation_history", "event_log", "session_state"] {
                if let Some(value) = obj.get(key) {
                    tags.insert(key.to_string(), self.json_to_baml_value(value)?);
                }
            }
        } else {
            tags.insert(
                "conversation_history".to_string(),
                self.json_to_baml_value(&payload)?,
            );
        }
        Ok(Some(tags))
    }

    /// List all available function names from the loaded BAML runtime
    pub fn list_functions(&self) -> Vec<String> {
        self.runtime
            .function_names()
            .map(|s| s.to_string())
            .collect()
    }

    /// Convert JSON Value to BamlMap<String, BamlValue>
    fn json_to_baml_map(&self, value: &Value) -> Result<baml_types::BamlMap<String, BamlValue>> {
        let obj = value
            .as_object()
            .ok_or_else(|| BamlRtError::InvalidArgument("Expected JSON object".to_string()))?;

        let mut map = baml_types::BamlMap::new();
        for (k, v) in obj {
            map.insert(k.clone(), self.json_to_baml_value(v)?);
        }
        Ok(map)
    }

    /// Convert JSON Value to BamlValue
    #[allow(clippy::only_used_in_recursion)]
    fn json_to_baml_value(&self, value: &Value) -> Result<BamlValue> {
        match value {
            Value::String(s) => Ok(BamlValue::String(s.clone())),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(BamlValue::Int(i))
                } else if let Some(f) = n.as_f64() {
                    Ok(BamlValue::Float(f))
                } else {
                    Err(BamlRtError::TypeConversion(format!(
                        "Invalid number: {}",
                        n
                    )))
                }
            }
            Value::Bool(b) => Ok(BamlValue::Bool(*b)),
            Value::Array(arr) => {
                let mut vec = Vec::new();
                for item in arr {
                    vec.push(self.json_to_baml_value(item)?);
                }
                Ok(BamlValue::List(vec))
            }
            Value::Object(obj) => {
                let mut map = baml_types::BamlMap::new();
                for (k, v) in obj {
                    map.insert(k.clone(), self.json_to_baml_value(v)?);
                }
                Ok(BamlValue::Map(map))
            }
            Value::Null => Ok(BamlValue::Null),
        }
    }
}

async fn maybe_execute_tool_from_result(
    tool_registry: &Arc<ToolRegistry>,
    result: &Value,
    scope: &baml_rt_core::context::RuntimeScope,
) -> Result<Option<Value>> {
    let Some((tool_name, tool_args)) = extract_tool_call(result)? else {
        return Ok(None);
    };

    let tool_result = tool_registry
        .execute(&tool_name, tool_args, scope.context_id(), scope.agent_id())
        .await?;
    Ok(Some(tool_result))
}

fn extract_tool_call(result: &Value) -> Result<Option<(String, Value)>> {
    let obj = match result.as_object() {
        Some(obj) => obj,
        None => return Ok(None),
    };

    if let Some(tool_name) = obj.get("tool_name") {
        return Ok(Some(parse_tool_call_object(obj, tool_name)?));
    }

    if obj.len() == 1 {
        let (_, value) = obj.iter().next().ok_or_else(|| {
            BamlRtError::InvalidArgument("Expected non-empty tool object".to_string())
        })?;
        if let Some(inner) = value.as_object()
            && let Some(tool_name) = inner.get("tool_name")
        {
            return Ok(Some(parse_tool_call_object(inner, tool_name)?));
        }
    }

    Ok(None)
}

fn parse_tool_call_object(
    obj: &serde_json::Map<String, Value>,
    tool_name_value: &Value,
) -> Result<(String, Value)> {
    let tool_name = tool_name_value
        .as_str()
        .ok_or_else(|| BamlRtError::InvalidArgument("tool_name must be a string".to_string()))?;

    let mut tool_args = serde_json::Map::new();
    for (key, value) in obj {
        if key != "tool_name" && key != "__type" {
            tool_args.insert(key.clone(), value.clone());
        }
    }

    Ok((tool_name.to_string(), Value::Object(tool_args)))
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use baml_derive::BamlType;
    use baml_rt_tools::{BamlTool, bundles::BundleType};
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    // Test bundle for test tools
    struct Test;

    impl BundleType for Test {
        const NAME: &'static str = "test";
        fn description() -> &'static str {
            "Test tools for unit testing"
        }
    }

    struct EchoTool;

    #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    struct EchoInput {
        message: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    struct EchoOutput {
        echo: serde_json::Value,
    }

    #[async_trait]
    impl BamlTool for EchoTool {
        type Bundle = Test;
        const LOCAL_NAME: &'static str = "echo_tool";
        type OpenInput = ();
        type Input = EchoInput;
        type Output = EchoOutput;

        fn description(&self) -> &'static str {
            "Echoes the input payload."
        }

        async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
            Ok(EchoOutput {
                echo: json!({ "message": args.message }),
            })
        }
    }

    #[tokio::test]
    async fn executes_tool_when_explicit_variant_is_present() {
        use baml_rt_core::ids::{AgentId, UuidId};
        let registry = Arc::new(ToolRegistry::new());
        registry.register(EchoTool).unwrap();
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000040").unwrap());
        let scope = baml_rt_core::context::InvocationScope::synthetic_message(agent_id);

        let result = json!({
            "tool_name": "test/echo_tool",
            "message": "hello"
        });

        let tool_result = maybe_execute_tool_from_result(&registry, &result, scope.as_scope())
            .await
            .unwrap()
            .expect("expected tool execution");

        assert_eq!(tool_result["echo"]["message"], "hello");
    }

    #[tokio::test]
    async fn leaves_non_tool_results_untouched() {
        use baml_rt_core::ids::{AgentId, UuidId};
        let registry = Arc::new(ToolRegistry::new());
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000041").unwrap());
        let scope = baml_rt_core::context::InvocationScope::synthetic_message(agent_id);
        let result = json!({ "value": "not a tool" });

        let tool_result = maybe_execute_tool_from_result(&registry, &result, scope.as_scope())
            .await
            .unwrap();

        assert!(tool_result.is_none());
    }
}

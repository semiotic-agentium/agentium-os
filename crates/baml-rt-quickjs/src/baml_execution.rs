//! BAML function execution engine
//!
//! This module executes BAML functions using the compiled IL (Intermediate Language)
//! from the BAML compiler.

use std::{collections::HashMap, path::Path, sync::Arc, time::Instant};

use async_trait::async_trait;
use baml_rt_core::{
    BamlFunctionId, BamlRtError, InvocationKind, Outcome, Result, bus::EffectEmitter, context,
};
use baml_rt_interceptor::{InterceptorDecision, InterceptorRegistry};
use baml_rt_llm_config::LlmClientResolver;
use baml_runtime::{
    BamlRuntime, FunctionResultStream, RuntimeContextManager, client_registry::ClientRegistry,
};
use baml_types::{BamlMap, BamlValue};
use serde_json::Value;
use tokio::{
    sync::Mutex,
    time::{Duration, sleep},
};

use crate::{
    baml_collector::{BamlLLMCollector, LLMCompletionHandle},
    baml_pre_execution::intercept_llm_call_pre_execution,
    llm_client_registry::{LlmSecretResolver, build_llm_client_registry},
};

/// Logs a terminal BAML `call_function` failure at ERROR severity (single source for tests).
pub(crate) fn log_baml_call_function_terminal_error(
    function_name: &str,
    err: &impl std::fmt::Display,
    hop_elapsed_ms: u64,
    elapsed_ms: u64,
) {
    tracing::error!(
        function = function_name,
        error = %err,
        hop_elapsed_ms,
        elapsed_ms,
        "BAML call_function: terminal execution error (not parse-retry)"
    );
}

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
    let payload_bytes = serde_json::to_vec(context).map_or(0, |v| v.len());
    let token = if status_token.is_empty() {
        None
    } else {
        Some(status_token.to_string())
    };
    Some((args_bytes, session_open, payload_bytes, token))
}

#[allow(dead_code)] // Reserved for URL normalization when wiring multi-endpoint clients.
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

/// Build a session-FSM-aware `ClientRegistry` for step-executor calls.
///
/// These calls inject `session_context` into the BAML args, which is not a
/// declared function parameter — so calling `build_request` with those args
/// fails parameter validation.  Instead we resolve the API key directly via the
/// `LlmSecretResolver` (config/secret store) and delegate to
/// `build_llm_client_registry` which already knows how to walk the IR clients.
///
/// Returns `Ok(None)` when no resolver is configured or when it cannot resolve
/// any API key — in that case the caller falls back to the normal BAML client
/// resolution path (which may use env vars if they are present in the process).
/// Prefer resolving secrets from the configured store when available.
#[allow(clippy::too_many_arguments)]
async fn build_session_fsm_client_registry(
    runtime: &BamlRuntime,
    _scope: &context::RuntimeScope,
    _function_name: &str,
    args: &Value,
    _params: &BamlMap<String, BamlValue>,
    _ctx_manager: &RuntimeContextManager,
    llm_secret_resolver: Option<&dyn LlmSecretResolver>,
    _planning_step: Option<(&str, &str)>,
) -> Result<Option<ClientRegistry>> {
    if planner_state_telemetry(args).is_none() {
        return Ok(None);
    }
    // Only override the client when the secret resolver has a key — otherwise
    // fall through to the normal BAML env-var resolution path.
    Ok(
        build_llm_client_registry(runtime, llm_secret_resolver, "default")
            .map_err(|e| BamlRtError::ClientRegistryBuild { source: e })?
            .into_registry(),
    )
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

/// Parse provider `conversation_history_json` payload into a flat line array.
pub(crate) fn extract_conversation_array_from_payload(payload: &Value) -> Vec<Value> {
    let history = if payload.is_array() {
        payload
    } else if let Some(obj) = payload.as_object()
        && let Some(ch) = obj.get("conversation_history")
    {
        ch
    } else {
        payload
    };
    history.as_array().map(|a| a.to_vec()).unwrap_or_default()
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
    /// The payload is injected as `ctx.tags['conversation_history']` in BAML templates.
    /// Provider is called with the runtime scope of the current invocation. For resume,
    /// scope must be TaskScoped with the session's `context_id` so history includes
    /// prior turns. Used in both stream and non-stream paths when conversation context
    /// is injected.
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>>;

    /// Graph read for step-executor intra buffer delta (`p_before` / `p_after`).
    ///
    /// Defaults to the same as [`Self::conversation_history_json`]. Implementations
    /// that cap history (e.g. last _N_ items) should override and read without that
    /// cap, otherwise a sliding window can make consecutive reads **not** a strict
    /// prefix extension and break step-executor `p_before` / `p_after` hop checks.
    async fn conversation_history_json_for_intra_dedup(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<Value>> {
        self.conversation_history_json(scope).await
    }
}

pub struct BamlExecutor {
    runtime: Arc<BamlRuntime>,
    effect_emitter: Option<Arc<dyn EffectEmitter>>,
    conversation_context_provider: Option<Arc<dyn ConversationContextProvider>>,
    parse_retry_policy: ParseRetryPolicy,
    /// When set, LLM API keys are injected via ClientRegistry (not env vars).
    llm_secret_resolver: Option<Arc<dyn LlmSecretResolver>>,
    /// When set, per-agent/per-prompt LLM client overrides are resolved from host config
    /// instead of using the first BAML IR client as primary.
    llm_client_resolver: Option<Arc<dyn LlmClientResolver>>,
}

impl BamlExecutor {
    /// Load BAML IL from the compiled output
    ///
    /// This loads the BAML runtime from the baml_src directory using from_directory.
    /// `env_vars` should include resolved LLM secrets (e.g. OPENROUTER_API_KEY) so that
    /// BAML schema's `api_key env.X` references resolve correctly without relying on
    /// std::env::var. Pass the result of `BamlRuntimeManager::resolve_secrets_as_env_vars()`.
    pub fn load_il(baml_src_dir: &Path, env_vars: HashMap<String, String>) -> Result<Self> {
        tracing::debug!(?baml_src_dir, "Loading BAML runtime from directory");

        let feature_flags = internal_baml_core::feature_flags::FeatureFlags::default();

        let runtime = BamlRuntime::from_directory(baml_src_dir, env_vars, feature_flags)
            .map_err(|e| BamlRtError::RuntimeLoadFailed { source: e })?;

        Ok(Self {
            runtime: Arc::new(runtime),
            effect_emitter: None,
            conversation_context_provider: None,
            parse_retry_policy: ParseRetryPolicy::default(),
            llm_secret_resolver: None,
            llm_client_resolver: None,
        })
    }

    /// Set the LLM secret resolver for ClientRegistry-based API key injection.
    /// When set, API keys are resolved via the resolver (e.g. fnox + llm mapping) and
    /// passed to BAML as ClientRegistry, not env vars.
    pub fn set_llm_secret_resolver(&mut self, resolver: Arc<dyn LlmSecretResolver>) {
        self.llm_secret_resolver = Some(resolver);
    }

    /// Set the LLM client resolver for per-agent/per-prompt model overrides.
    /// When set, `execute_function` uses this to resolve which LLM client (and model)
    /// to use based on the agent and function name, instead of the IR-walk default.
    pub fn set_llm_client_resolver(&mut self, resolver: Arc<dyn LlmClientResolver>) {
        self.llm_client_resolver = Some(resolver);
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
    #[allow(clippy::too_many_arguments)]
    /// the manager must call `handle.complete(Success, None)` or `handle.complete(Failure, Some(reason))` after.
    pub async fn execute_function(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        args: Value,
        interceptor_registry: Option<Arc<Mutex<InterceptorRegistry>>>,
        planning_step: Option<(String, String)>,
        function_tool_manifest: &crate::baml::FunctionToolManifest,
        // Override context tags — merged intra-turn history. When Some, skips provider query.
        override_context_tags: Option<HashMap<String, BamlValue>>,
    ) -> Result<(Value, Option<LLMCompletionHandle>)> {
        tracing::debug!(
            function = function_name,
            args = ?args,
            "Executing BAML function from IL"
        );

        // Convert JSON args to BamlValue map
        let params = self.json_to_baml_map(&args)?;

        // Build ClientRegistry: prefer config-based resolver (per-agent/per-prompt overrides),
        // fall back to IR-walk registry (backwards compatible).
        let scope_id = scope.agent_id().as_str();
        let config_registry = if let Some(ref resolver) = self.llm_client_resolver {
            match resolver.resolve(scope, function_name).await {
                Ok(Some(registry)) => Some(registry),
                Ok(None) => {
                    tracing::debug!(
                        function = function_name,
                        "LLM client resolver returned None; falling back to IR walk"
                    );
                    None
                }
                Err(e) => {
                    tracing::warn!(function = function_name, error = %e, "LLM client resolver failed; falling back to IR walk");
                    None
                }
            }
        } else {
            None
        };
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
                let mut coll =
                    BamlLLMCollector::new(registry.clone(), BamlFunctionId::parse(function_name));
                if let Some(ref emitter) = self.effect_emitter {
                    coll.set_effect_emitter(emitter.clone());
                }
                Arc::new(coll)
            });

        // Pre-execution interception: intercept LLM calls before they're sent
        let context_tags = match override_context_tags {
            Some(tags) => Some(tags),
            None => self.build_conversation_context_tags(scope).await?,
        };
        let ctx_manager = self.create_ctx_manager_for_scope(scope, context_tags)?;
        let planning_step_refs = planning_step
            .as_ref()
            .map(|(plan_id, step_id)| (plan_id.as_str(), step_id.as_str()));
        let session_client_registry = build_session_fsm_client_registry(
            &self.runtime,
            scope,
            function_name,
            &args,
            &params,
            &ctx_manager,
            self.llm_secret_resolver.as_deref(),
            planning_step_refs,
        )
        .await?;
        if let Some(ref registry) = interceptor_registry {
            // `session_context` is a runtime-injected arg for step-executor functions.
            // Phase prompts may reference FSM facts (e.g. `session_context.session_open`);
            // legal ops come from narrowed per-phase return types.
            // Pass the full params (including session_context) into the interceptor probe
            // so templates match the real `call_function` invocation.
            let interceptor_params = params.clone();
            match intercept_llm_call_pre_execution(
                &self.runtime,
                scope,
                function_name,
                &interceptor_params,
                &ctx_manager,
                registry,
                env_vars.clone(),
                session_client_registry
                    .as_ref()
                    .or(config_registry.as_ref())
                    .or(llm_registry_result.registry()),
                llm_registry_result.secret_keys_accessed(),
                InvocationKind::Invoke,
                self.effect_emitter.as_ref(),
                collector.as_ref().map(Arc::as_ref),
                planning_step_refs,
                function_tool_manifest,
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
                            .complete_pending_effects(
                                Outcome::Success,
                                0,
                                None,
                                Some(value.clone()),
                            )
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
            tracing::debug!(
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
            // Priority: session FSM registry > config-based override > IR-walk fallback
            let effective_registry = session_client_registry
                .as_ref()
                .or(config_registry.as_ref())
                .or(llm_registry_result.registry());
            let (result, _call_id) = self
                .runtime
                .call_function(
                    function_name.to_string(),
                    &params,
                    &ctx_manager,
                    None, // type_builder
                    effective_registry,
                    attempt_collectors,
                    env_vars.clone(),
                    tags,
                    cancel_tripwire,
                )
                .await;

            if let Err(ref e) = result {
                log_baml_call_function_terminal_error(
                    function_name,
                    e,
                    attempt_start.elapsed().as_millis() as u64,
                    start_time.elapsed().as_millis() as u64,
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
                    tracing::debug!(
                        function = function_name,
                        hop_elapsed_ms = attempt_start.elapsed().as_millis() as u64,
                        elapsed_ms = start_time.elapsed().as_millis() as u64,
                        "BAML call_function: ok"
                    );
                    // Defer LLM completion until after tool plan execution: plan extraction/execution
                    // failure is part of the LLM call outcome in the graph (invalid output from the LLM).
                    let json_value = serde_json::to_value(parsed.serialize_partial())
                        .map_err(BamlRtError::Json)?;

                    // All tool/session-plan execution deferred to invoke_function's
                    // execute_tool_from_baml_result_or_value — the single canonical path.
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

    /// Raw `conversation_history` line objects from the provider only (no intra-turn merge).
    /// Matches `ctx.tags['conversation_history']` capping (e.g. last _N_ graph items).
    pub(crate) async fn provider_conversation_history_lines(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Vec<Value>> {
        let Some(provider) = self.conversation_context_provider.as_ref() else {
            return Ok(vec![]);
        };
        let Some(payload) = provider.conversation_history_json(scope).await? else {
            return Ok(vec![]);
        };
        Ok(extract_conversation_array_from_payload(&payload))
    }

    /// Full projected line list for step-executor `p_before` / `p_after` prefix checks (uncapped).
    pub(crate) async fn provider_conversation_history_lines_for_intra_dedup(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Vec<Value>> {
        let Some(provider) = self.conversation_context_provider.as_ref() else {
            return Ok(vec![]);
        };
        let Some(payload) = provider
            .conversation_history_json_for_intra_dedup(scope)
            .await?
        else {
            return Ok(vec![]);
        };
        Ok(extract_conversation_array_from_payload(&payload))
    }

    /// Build `ctx.tags` from a merged `conversation_history` line list.
    pub(crate) fn tags_from_merged_conversation_lines(
        &self,
        lines: Vec<Value>,
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        if lines.is_empty() {
            return Ok(None);
        }
        let mut tags = HashMap::new();
        tags.insert(
            "conversation_history".to_string(),
            self.json_to_baml_value(&Value::Array(lines))?,
        );
        Ok(Some(tags))
    }

    /// Build conversation-history tags for the given scope (used by stream path for resume).
    /// Returns None if no provider is set or provider returns empty. The manager-facing
    /// For step-executor hops, use the manager’s `invoke_function_with_intra` path instead.
    pub async fn build_conversation_context_tags(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        let lines = self.provider_conversation_history_lines(scope).await?;
        self.tags_from_merged_conversation_lines(lines)
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

#[cfg(test)]
mod terminal_error_log_tests {
    use std::sync::{Arc, Mutex};

    use tracing::Level;
    use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

    use super::log_baml_call_function_terminal_error;

    #[test]
    fn baml_call_function_terminal_error_emits_error_level() {
        let seen: Arc<Mutex<Vec<Level>>> = Arc::new(Mutex::new(Vec::new()));
        struct Capture(Arc<Mutex<Vec<Level>>>);
        impl<S: tracing::Subscriber> Layer<S> for Capture {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.0.lock().expect("lock").push(*event.metadata().level());
            }
        }

        let _g = tracing_subscriber::registry()
            .with(Capture(Arc::clone(&seen)))
            .set_default();

        log_baml_call_function_terminal_error("TestFn", &"simulated failure", 1, 2);

        let levels = seen.lock().expect("lock");
        assert!(
            levels.contains(&Level::ERROR),
            "expected ERROR event, got {levels:?}"
        );
    }
}

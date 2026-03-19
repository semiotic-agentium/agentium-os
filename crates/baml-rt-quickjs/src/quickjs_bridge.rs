//! QuickJS integration bridge
//!
//! This module maps BAML function calls (executed in Rust) to QuickJS,
//! allowing JavaScript code to invoke BAML functions.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64},
    },
    thread,
};

use baml_rt_core::{
    BamlRtError, Result, bus::EffectLiveness, context::InvocationScope, correlation,
};
use dashmap::DashMap;
use quickjs_runtime::{
    builder::QuickJsRuntimeBuilder, facades::QuickJsRuntimeFacade, jsutils::Script,
    values::JsValueFacade,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Semaphore, mpsc};

use crate::baml::BamlRuntimeManager;

mod baml_registration;
pub(crate) use baml_registration::ExecutionSession;
mod eval;
mod invocation;
mod js_codegen;
mod promise_polling;
mod scope;
mod stream;
pub(crate) mod stream_yield;
mod tools;
mod types;
mod wrappers;

pub use eval::EffectGatedTimeoutPolicy;
use scope::{
    InvocationContextId, InvocationContextRegistry, InvocationToken, next_invocation_token,
    resolve_scope_from_active_context, resolve_scope_from_session,
};
pub use types::StreamSessionId;
use types::{
    BriefPollParams, CorrelationMap, EvalLifecycleGuard, EvalNotifyMap, EvalOnceResult,
    EvalResultMap, InFlightCounter, InFlightGuard, InvocationContextRegistrySlot,
    InvocationScopeMap, PreparedBriefPollEval, StreamInvocationSession, StreamSemaphore,
    StreamSessionMap, empty_open_input, tool_step_to_value,
};

/// Bridge between QuickJS JavaScript runtime and BAML functions
///
/// BAML functions execute in Rust. This bridge exposes them to QuickJS
/// so JavaScript code can call them.
///
/// The runtime is held in an [`Arc`] so the promise-poll loop can run pending jobs
/// without holding the bridge lock (deadlock-free resume path).
pub struct QuickJSBridge {
    runtime: Arc<QuickJsRuntimeFacade>,
    baml_manager: Arc<Mutex<BamlRuntimeManager>>,
    js_tools: HashSet<String>, // Track JavaScript-only tools
    agent_id: baml_rt_core::ids::AgentId,
    effect_liveness: Option<Arc<dyn EffectLiveness>>,
    idle_timeout_ms: u64,
    max_attempts_ms: u64,
    /// Stream collector idle timeout (secs). Default 60; configurable for tests.
    stream_collector_idle_secs: u64,
    /// Host-only active invocation stack; natives resolve scope from current top (tokenless).
    invocation_context_registry: InvocationContextRegistrySlot,
    /// Token → scope, still populated for eval result tracking.
    invocation_scope_by_token: InvocationScopeMap,
    /// Token -> correlation id captured at invocation entry and propagated through native callbacks.
    correlation_id_by_token: CorrelationMap,
    /// Token → eval result (None while pending). Strictly keyed by token.
    eval_results_by_token: EvalResultMap,
    /// Token → Notify; when __set_eval_result runs for that token it notifies. Ensures poll loop observes result only after write (no ordering race).
    eval_notify_by_token: EvalNotifyMap,
    /// Up to N stream invocations may be active; each holds a permit in its session.
    /// Permit is released when the session is removed in finalize_a2a_stream_invocation.
    stream_semaphore: StreamSemaphore,
    /// Number of `__baml_invoke` / `__baml_stream` async bodies currently in-flight on tokio.
    /// Incremented synchronously on the event-loop thread when the native is called;
    /// decremented (via [`InFlightGuard`]) when the async body completes.
    in_flight_invoke_count: InFlightCounter,
    /// Active stream sessions keyed by session id. Populated in `invoke_js_function_stream`,
    /// drained in `finalize_a2a_stream_invocation`. Session-aware natives resolve scope
    /// from this map instead of the LIFO `invocation_context_registry`.
    stream_sessions: StreamSessionMap,
    /// Per-session yield senders used by host routing from the active invocation context.
    a2a_yield_tx_by_session: Arc<DashMap<StreamSessionId, mpsc::UnboundedSender<Value>>>,
    /// Monotonic counter for allocating unique `StreamSessionId` values.
    next_stream_session_id: AtomicU64,
    /// Shared execution session state (planning FSM). Passed into `register_execution_session_helper`
    /// and read by `resolve_planning_step` for step coordinate injection.
    execution_sessions: Arc<DashMap<String, ExecutionSession>>,
}

impl QuickJSBridge {
    /// Create a new QuickJS bridge with default configuration
    ///
    /// # Arguments
    /// * `baml_manager` - The BAML runtime manager to use
    /// * `agent_id` - REQUIRED agent ID for this bridge instance
    pub async fn new(
        baml_manager: Arc<Mutex<BamlRuntimeManager>>,
        agent_id: baml_rt_core::ids::AgentId,
    ) -> Result<Self> {
        Self::new_with_config(
            baml_manager,
            agent_id,
            crate::runtime::QuickJSConfig::default(),
        )
        .await
    }

    /// Create a new QuickJS bridge with custom configuration
    ///
    /// # Arguments
    /// * `baml_manager` - The BAML runtime manager to use
    /// * `agent_id` - REQUIRED agent ID for this bridge instance
    /// * `config` - QuickJS runtime configuration options
    pub async fn new_with_config(
        baml_manager: Arc<Mutex<BamlRuntimeManager>>,
        agent_id: baml_rt_core::ids::AgentId,
        config: crate::runtime::QuickJSConfig,
    ) -> Result<Self> {
        tracing::info!(
            memory_limit = ?config.memory_limit,
            max_stack_size = ?config.max_stack_size,
            gc_threshold = ?config.gc_threshold,
            gc_interval = ?config.gc_interval,
            "Initializing QuickJS bridge with configuration"
        );

        // Initialize QuickJS runtime using builder and apply configuration
        let mut builder = QuickJsRuntimeBuilder::new();

        if let Some(limit) = config.memory_limit {
            builder = builder.memory_limit(limit);
        }

        if let Some(stack_size) = config.max_stack_size {
            builder = builder.max_stack_size(stack_size);
        }

        if let Some(threshold) = config.gc_threshold {
            builder = builder.gc_threshold(threshold);
        }

        if let Some(interval) = config.gc_interval {
            builder = builder.gc_interval(interval);
        }

        let runtime = Arc::new(builder.build());

        let stream_permits = config
            .stream_concurrency
            .unwrap_or_else(|| thread::available_parallelism().map_or(4, |value| value.get()));

        // Create bridge instance
        let mut bridge = Self {
            runtime,
            baml_manager,
            js_tools: HashSet::new(),
            agent_id,
            effect_liveness: None,
            idle_timeout_ms: config.idle_timeout_ms.unwrap_or(5000), // Default 5s
            max_attempts_ms: config
                .max_attempts_ms
                .unwrap_or(EffectGatedTimeoutPolicy::DEFAULT_MAX_ATTEMPTS as u64), // Default 30 minutes
            stream_collector_idle_secs: config.stream_collector_idle_secs.unwrap_or(60),
            invocation_context_registry: Arc::new(std::sync::Mutex::new(
                InvocationContextRegistry::new(),
            )),
            invocation_scope_by_token: Arc::new(DashMap::new()),
            correlation_id_by_token: Arc::new(DashMap::new()),
            eval_results_by_token: Arc::new(DashMap::new()),
            eval_notify_by_token: Arc::new(DashMap::new()),
            stream_semaphore: Arc::new(Semaphore::new(stream_permits.max(1))),
            in_flight_invoke_count: Arc::new(AtomicU32::new(0)),
            stream_sessions: Arc::new(DashMap::new()),
            a2a_yield_tx_by_session: Arc::new(DashMap::new()),
            next_stream_session_id: AtomicU64::new(1),
            execution_sessions: Arc::new(DashMap::new()),
        };

        // Initialize sandbox - remove dangerous globals and implement safe console
        // INVARIANT L1: Bridge initialization must terminate within bounded time
        // Timeout is handled in initialize_sandbox() itself
        bridge.initialize_sandbox().await?;

        bridge.register_chat_yield_host().await?;

        Ok(bridge)
    }

    /// Set the effect liveness tracker (for effects-first liveness gating)
    pub fn set_effect_liveness(&mut self, liveness: Arc<dyn EffectLiveness>) {
        self.effect_liveness = Some(liveness);
    }

    /// Effect liveness for this bridge (used for effect-gated timeouts in collect and promise polling).
    pub fn effect_liveness(&self) -> Option<Arc<dyn EffectLiveness>> {
        self.effect_liveness.clone()
    }

    /// Idle timeout in ms (short timeout when no effects in-flight). Used by collect() quiescence and promise polling.
    pub fn idle_timeout_ms(&self) -> u64 {
        self.idle_timeout_ms
    }

    /// Max attempts/timeout in ms (long timeout when effects in-flight). Used by collect() quiescence and promise polling.
    pub fn max_attempts_ms(&self) -> u64 {
        self.max_attempts_ms
    }

    /// Stream collector idle timeout in seconds (default 60). Used by collect_into_channel_owned.
    pub fn stream_collector_idle_secs(&self) -> u64 {
        self.stream_collector_idle_secs
    }

    /// Agent ID for this bridge (set at construction; used for attribution and scope).
    pub fn agent_id(&self) -> &baml_rt_core::ids::AgentId {
        &self.agent_id
    }

    /// Getters for registration module (baml_registration) and other submodules.
    pub(crate) fn runtime(&self) -> &Arc<QuickJsRuntimeFacade> {
        &self.runtime
    }
    pub(crate) fn baml_manager(&self) -> &Arc<Mutex<BamlRuntimeManager>> {
        &self.baml_manager
    }

    /// List all BAML function names registered in this bridge's runtime manager.
    /// Used by the runner to populate agent discovery entries at boot time.
    pub async fn list_baml_functions(&self) -> Vec<String> {
        self.baml_manager.lock().await.list_functions()
    }
    pub(crate) fn invocation_context_registry(&self) -> &InvocationContextRegistrySlot {
        &self.invocation_context_registry
    }
    pub(crate) fn eval_results_by_token(&self) -> &EvalResultMap {
        &self.eval_results_by_token
    }
    pub(crate) fn eval_notify_by_token(&self) -> &EvalNotifyMap {
        &self.eval_notify_by_token
    }
    /// Arc to the in-flight counter (for registration closures). Use `in_flight_invoke_count()` for the current count.
    pub(crate) fn in_flight_invoke_count_arc(&self) -> &InFlightCounter {
        &self.in_flight_invoke_count
    }
    pub(crate) fn stream_sessions(&self) -> &StreamSessionMap {
        &self.stream_sessions
    }
    pub(crate) fn execution_sessions(&self) -> &Arc<DashMap<String, ExecutionSession>> {
        &self.execution_sessions
    }

    /// Initialize the sandbox environment
    ///
    /// This removes dangerous globals and modules, and implements a safe console API.
    /// Only console.log is available - no filesystem, network, or other I/O access.
    ///
    /// **INVARIANT L1:** This operation MUST terminate within bounded time (5 seconds).
    /// If it hangs, the QuickJS runtime may have a blocking operation.
    async fn initialize_sandbox(&mut self) -> Result<()> {
        tracing::info!("Initializing QuickJS sandbox environment");

        // Initialize safe console and ensure dangerous globals aren't available
        // QuickJS by default doesn't expose require, fetch, etc., but we ensure console.log works safely
        let sandbox_code = r#"
            (function() {
                // Implement safe console object - only log methods, no I/O
                // QuickJS handles console output through its runtime, preventing direct system I/O
                globalThis.console = {
                    log: function() {
                        // console.log output goes to QuickJS runtime logs
                        // No filesystem or network access
                        var args = arguments;
                        for (var i = 0; i < args.length; i++) {
                            var arg = args[i];
                            if (typeof arg === 'object') {
                                try {
                                    JSON.stringify(arg);
                                } catch (e) {
                                    String(arg);
                                }
                            }
                        }
                    },
                    info: function() {
                        globalThis.console.log.apply(globalThis.console, arguments);
                    },
                    warn: function() {
                        globalThis.console.log.apply(globalThis.console, arguments);
                    },
                    error: function() {
                        globalThis.console.log.apply(globalThis.console, arguments);
                    },
                    debug: function() {
                        globalThis.console.log.apply(globalThis.console, arguments);
                    }
                };
            })();
        "#;

        let script = Script::new("sandbox_init.js", sandbox_code);
        tracing::debug!("initialize_sandbox: Calling runtime.eval() for sandbox code");

        // INVARIANT L2: Runtime eval must yield control within bounded time
        // If this hangs, the QuickJS runtime may have internal blocking. 15s for CI/slow runners.
        use tokio::time::{Duration, timeout};
        timeout(
            Duration::from_secs(15),
            self.runtime.eval(None, script),
        )
        .await
        .map_err(|_| BamlRtError::QuickJs(
            "Sandbox initialization timed out after 15 seconds - QuickJS runtime.eval() may be blocking".to_string()
        ))?
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to initialize sandbox".to_string(),
            source: Box::new(e),
        })?;

        tracing::info!("QuickJS sandbox initialized - I/O restricted to runtime host functions");
        Ok(())
    }

    /// Register all BAML functions with the QuickJS context
    ///
    /// This maps Rust BAML functions to JavaScript callables.
    /// When JS calls the function, it will invoke the Rust BAML execution.
    pub async fn register_baml_functions(&mut self) -> Result<()> {
        tracing::info!("Registering BAML functions with QuickJS");

        {
            let mut manager = self.baml_manager.lock().await;
            manager.set_execution_sessions(self.execution_sessions.clone());
        }

        let manager = self.baml_manager.lock().await;
        let functions = manager.list_functions();
        drop(manager); // Release lock before async operation

        // First, register helper functions that JavaScript can call to invoke BAML functions
        self.register_baml_invoke_helper().await?;
        self.register_baml_stream_helper().await?;
        self.register_await_helper().await?;
        self.register_step_executor_runtime_helpers().await?;
        self.register_execution_session_helper().await?;

        // Session-aware natives for stream paths (resolve scope from session map).
        self.register_baml_invoke_session_helper().await?;
        self.register_baml_stream_session_helper().await?;

        for function_name in functions {
            self.register_single_function(&function_name).await?;
            self.register_single_stream_function(&function_name).await?;
        }

        // Register tool functions
        self.register_tool_functions().await?;

        Ok(())
    }

    /// Register __baml_invoke. Tokenless: host resolves scope from active context. JS calls (function_name, args).
    async fn register_baml_invoke_helper(&mut self) -> Result<()> {
        baml_registration::register_baml_invoke_helper(self).await
    }

    /// Register a helper function that can await promises and return JSON strings
    /// This helps with the synchronous eval() limitation
    async fn register_await_helper(&mut self) -> Result<()> {
        baml_registration::register_await_helper(self).await
    }

    async fn register_step_executor_runtime_helpers(&mut self) -> Result<()> {
        baml_registration::register_step_executor_runtime_helpers(self).await
    }

    async fn register_execution_session_helper(&mut self) -> Result<()> {
        baml_registration::register_execution_session_helper(self).await
    }

    /// Register __baml_stream. Tokenless: host resolves scope from active context. JS calls (function_name, args).
    async fn register_baml_stream_helper(&mut self) -> Result<()> {
        baml_registration::register_baml_stream_helper(self).await
    }

    /// Register `__baml_invoke_session(session_id, function_name, args_json)`.
    async fn register_baml_invoke_session_helper(&mut self) -> Result<()> {
        baml_registration::register_baml_invoke_session_helper(self).await
    }

    /// Register `__baml_stream_session(session_id, function_name, args_json)`.
    async fn register_baml_stream_session_helper(&mut self) -> Result<()> {
        baml_registration::register_baml_stream_session_helper(self).await
    }

    /// Register a single BAML function with QuickJS (tokenless wrapper).
    async fn register_single_function(&mut self, function_name: &str) -> Result<()> {
        baml_registration::register_single_function(self, function_name).await
    }

    /// Register a streaming version of a single BAML function with QuickJS
    async fn register_single_stream_function(&mut self, function_name: &str) -> Result<()> {
        baml_registration::register_single_stream_function(self, function_name).await
    }

    /// Legacy: create token and register scope (used only for correlation map when needed).
    #[allow(dead_code)] // reserved for legacy correlation/scope cleanup when re-enabled
    fn create_invocation_token(&mut self, scope: &InvocationScope) -> (InvocationToken, String) {
        let token = next_invocation_token();
        let prelude = format!(
            "const __baml_invocation_token = \"{}\";",
            token.0.replace('\\', "\\\\").replace('"', "\\\"")
        );
        self.invocation_scope_by_token
            .insert(token.clone(), scope.as_scope().clone());
        if let Some(correlation_id) = correlation::current_correlation_id() {
            self.correlation_id_by_token
                .insert(token.clone(), correlation_id);
        }
        (token, prelude)
    }

    /// Remove an invocation token so it can no longer be used for scope lookup. Reserved for
    /// post-invocation cleanup of scope/correlation maps.
    #[allow(dead_code)] // reserved for post-invocation scope/correlation cleanup when re-enabled
    fn remove_invocation_token(&mut self, token: &InvocationToken) {
        self.invocation_scope_by_token.remove(token);
        self.correlation_id_by_token.remove(token);
    }

    /// Run a single script with explicit invocation scope available through token prelude.
    ///
    /// Scope lookup for native callbacks is token-authoritative; this helper keeps API shape
    /// stable while delegating to runtime eval.
    pub(crate) async fn run_eval_with_scope(
        &self,
        scope: &InvocationScope,
        script: Script,
        clear_policy: scope::ClearPolicy,
    ) -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
        scope::run_eval_with_scope(&self.runtime, scope, script, clear_policy).await
    }

    /// Post a closure to the QuickJS event-loop worker thread and return immediately.
    ///
    /// Invariant: caller path remains non-blocking; closure execution happens later on the
    /// runtime worker thread and must deliver outcomes through channels/oneshots.
    pub fn post_to_worker_void<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.runtime.add_task_to_event_loop_void(f);
    }

    /// Run a closure on the QuickJS event-loop worker thread (facade over the runtime's event loop).
    ///
    /// Use this to run work that must execute on the same thread as the QuickJS context (e.g. A2A
    /// session handling that will later call back into the bridge). No extra thread is spawned;
    /// the closure is posted to the existing QuickJS worker and the future resolves when it completes.
    pub async fn run_on_worker<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.runtime.loop_realm(None, move |_rt, _realm| f()).await
    }

    /// Execute synchronous JavaScript code with no invocation scope.
    ///
    /// Use for setup/init code that does not invoke BAML functions or JS tools and
    /// therefore cannot return a promise that awaits an LLM response. If the code
    /// returns `"__EVAL_PROMISE_PENDING__"` this will return an error rather than poll.
    ///
    /// For concurrent-safe async invocations use [`invoke_js_function_nonblocking`](Self::invoke_js_function_nonblocking).
    pub async fn eval_sync(&mut self, code: &str) -> Result<Value> {
        self.evaluate(None, code).await
    }

    /// Execute JavaScript code that may return a promise, with an explicit invocation scope.
    ///
    /// **Only available in test builds.** This holds `&mut self` (and thus the outer
    /// `Mutex<QuickJSBridge>`) across the polling loop, which blocks concurrent contexts.
    /// For production concurrent paths use [`eval_brief_lock`](Self::eval_brief_lock).
    #[cfg(any(test, feature = "testing"))]
    pub async fn eval_scoped(&mut self, scope: &InvocationScope, code: &str) -> Result<Value> {
        self.evaluate(Some(scope), code).await
    }

    /// Execute JavaScript code in the QuickJS context.
    ///
    /// The code should return a JSON string or a promise that resolves to a JSON string.
    /// If code returns a promise, we wait for it to resolve (see [`promise_polling`]).
    ///
    /// **Scope:** When `scope` is `Some`, we push it on the host invocation-context stack so
    /// native callbacks resolve scope tokenlessly. We also create an eval token for result
    /// tracking. Cleanup is guarded so it always runs on success, error, or cancellation.
    pub(crate) async fn evaluate(
        &mut self,
        scope: Option<&InvocationScope>,
        code: &str,
    ) -> Result<Value> {
        tracing::trace!(code = code, "Executing JavaScript code");

        // Scoped path: use the brief-poll pattern so the bridge lock is released before
        // the promise-polling loop. This allows concurrent contexts on the same agent
        // to make progress (LLM calls, tool sessions, drain iterations) while one
        // context is awaiting an LLM or BAML result in the poll loop.
        if let Some(scope) = scope {
            let prepared = self.prepare_brief_poll_eval(scope, code)?;
            // `&mut self` is released here — all polling runs lock-free below.
            let eval_result = QuickJSBridge::run_prepared_brief_poll_eval(prepared).await?;
            return Self::resolve_eval_once_result(eval_result, scope).await;
        }

        // scope=None fast-path: no promise polling expected (e.g. evaluate_js ad-hoc code).
        let direct_code = {
            let code_expr_body = eval::normalize_code_to_expr_body(code);
            let token_literal = format!("__noscope_{}", next_invocation_token().0);
            eval::build_eval_direct_code(&code_expr_body, &token_literal)
        };
        let direct_script = Script::new("eval_direct.js", &direct_code);
        let js_result = self.runtime.eval(None, direct_script).await.map_err(|e| {
            let message = e.to_string();
            BamlRtError::QuickJsWithSource {
                context: format!("Failed to execute JavaScript: {}", message),
                source: Box::new(e),
            }
        })?;

        if !js_result.is_string() {
            return Ok(serde_json::json!({}));
        }
        let json_str = js_result.get_str();
        if json_str == "__EVAL_PROMISE_PENDING__" {
            return Err(BamlRtError::QuickJs(
                "Promise polling requires invocation scope; evaluate(scope=None) must not await promises"
                    .to_string(),
            ));
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
            return Ok(parsed);
        }
        Ok(serde_json::json!({ "result": json_str }))
    }

    /// Resolve an [`EvalOnceResult`]: for sync results return directly; for pending promises
    /// run the poll loop lock-free using the `BriefPollParams` from the setup step.
    async fn resolve_eval_once_result(
        eval_result: EvalOnceResult,
        scope: &InvocationScope,
    ) -> Result<Value> {
        match eval_result {
            EvalOnceResult::Sync(value) => Ok(value),
            EvalOnceResult::PromisePending(params) => {
                let result_str = promise_polling::poll_promise_until_result(
                    promise_polling::PollPromiseParams {
                        runtime: Some(params.runtime.clone()),
                        eval_results_by_token: &params.eval_results_by_token,
                        eval_token: &params.eval_token,
                        token_to_remove: None,
                        invocation_scope_by_token: &params.invocation_scope_by_token,
                        scope,
                        effect_liveness: params.effect_liveness.clone(),
                        idle_timeout_ms: params.idle_timeout_ms,
                        max_attempts_ms: params.max_attempts_ms,
                        run_pending_jobs_brief: None,
                        result_notify: Some(params.result_notify.clone()),
                    },
                )
                .await?;
                serde_json::from_str(result_str.as_str()).map_err(|e| {
                    let len = result_str.len();
                    let prefix = result_str
                        .get(..50.min(len))
                        .unwrap_or("")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r");
                    BamlRtError::JsonWithRaw {
                        source: e,
                        raw_length: len,
                        raw_prefix: prefix,
                    }
                })
            }
        }
    }

    /// Prepares eval for brief-poll path (sync only). Caller runs the eval without holding the bridge lock.
    fn prepare_brief_poll_eval(
        &mut self,
        scope: &InvocationScope,
        code: &str,
    ) -> Result<PreparedBriefPollEval> {
        let context_id_to_exit: Option<InvocationContextId> = {
            let correlation_id = correlation::current_correlation_id();
            let mut guard = self.invocation_context_registry.lock().map_err(|_| {
                BamlRtError::QuickJs("invocation context registry lock poisoned".to_string())
            })?;
            Some(guard.enter(scope.as_scope().clone(), correlation_id))
        };

        let eval_token = next_invocation_token();
        let mut lifecycle_guard = EvalLifecycleGuard::new(
            self.eval_results_by_token.clone(),
            self.invocation_context_registry.clone(),
            eval_token.clone(),
            context_id_to_exit,
            self.baml_manager.clone(),
        );

        if self.eval_results_by_token.contains_key(&eval_token) {
            return Err(BamlRtError::QuickJs("eval token collision".to_string()));
        }
        self.eval_results_by_token.insert(eval_token.clone(), None);
        lifecycle_guard.mark_eval_slot_registered();

        let code_expr_body = eval::normalize_code_to_expr_body(code);
        let token_literal = eval_token.0.replace('\\', "\\\\").replace('"', "\\\"");
        let direct_code = eval::build_eval_direct_code(&code_expr_body, &token_literal);
        let direct_script = Script::new("eval_direct.js", &direct_code);
        let result_notify = Arc::new(tokio::sync::Notify::new());
        self.eval_notify_by_token
            .insert(eval_token.clone(), Arc::clone(&result_notify));
        Ok(PreparedBriefPollEval {
            direct_script,
            scope: scope.clone(),
            runtime: Arc::clone(&self.runtime),
            eval_token: eval_token.clone(),
            lifecycle_guard,
            result_notify,
            eval_results_by_token: self.eval_results_by_token.clone(),
            eval_notify_by_token: self.eval_notify_by_token.clone(),
            invocation_scope_by_token: self.invocation_scope_by_token.clone(),
            effect_liveness: self.effect_liveness.clone(),
            idle_timeout_ms: self.idle_timeout_ms,
            max_attempts_ms: self.max_attempts_ms,
        })
    }

    /// Runs prepared eval without holding the bridge lock so the worker and TaskManager can make progress.
    async fn run_prepared_brief_poll_eval(
        prepared: PreparedBriefPollEval,
    ) -> Result<EvalOnceResult> {
        let js_result = scope::run_eval_with_scope(
            &prepared.runtime,
            &prepared.scope,
            prepared.direct_script,
            scope::ClearPolicy::Clear,
        )
        .await
        .map_err(|e| {
            let message = e.to_string();
            BamlRtError::QuickJsWithSource {
                context: format!("Failed to execute JavaScript: {}", message),
                source: Box::new(e),
            }
        })?;

        if !js_result.is_string() {
            return Ok(EvalOnceResult::Sync(serde_json::json!({})));
        }

        let json_str = js_result.get_str();
        if json_str != "__EVAL_PROMISE_PENDING__" {
            if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
                return Ok(EvalOnceResult::Sync(parsed));
            }
            return Ok(EvalOnceResult::Sync(
                serde_json::json!({ "result": json_str }),
            ));
        }

        let params = BriefPollParams {
            runtime: prepared.runtime,
            eval_results_by_token: prepared.eval_results_by_token,
            eval_token: prepared.eval_token,
            result_notify: prepared.result_notify,
            eval_notify_by_token: prepared.eval_notify_by_token,
            invocation_scope_by_token: prepared.invocation_scope_by_token,
            scope: prepared.scope,
            effect_liveness: prepared.effect_liveness,
            idle_timeout_ms: prepared.idle_timeout_ms,
            max_attempts_ms: prepared.max_attempts_ms,
            lifecycle_guard: prepared.lifecycle_guard,
        };
        Ok(EvalOnceResult::PromisePending(Box::new(params)))
    }

    /// Lock briefly for setup, release the lock, then poll to completion.
    ///
    /// This is the canonical pattern for concurrent contexts: the `Mutex<QuickJSBridge>` is
    /// released before the promise-polling loop so other contexts can make progress (LLM calls,
    /// tool sessions, drain iterations) while this one awaits an async result.
    pub async fn eval_brief_lock(
        bridge: Arc<Mutex<Self>>,
        scope: &InvocationScope,
        js_code: &str,
    ) -> Result<Value> {
        let prepared = {
            let mut guard = bridge.lock().await;
            guard.prepare_brief_poll_eval(scope, js_code)?
        }; // MutexGuard dropped here — bridge lock released before polling
        let eval_result = Self::run_prepared_brief_poll_eval(prepared).await?;
        Self::resolve_eval_once_result(eval_result, scope).await
    }

    /// Invoke a JS global function (e.g. `onChatMessage`) with the brief-lock pattern.
    /// The bridge lock is released before the promise-polling loop so concurrent contexts
    /// are not blocked behind this invocation's LLM/BAML awaits.
    pub async fn invoke_js_function_nonblocking(
        bridge: Arc<Mutex<Self>>,
        scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<Value> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code = invocation::build_js_function_invoke_js_code(function_name, &args_json);
        if correlation::current_correlation_id().is_some() {
            Self::eval_brief_lock(bridge, scope, &js_code).await
        } else {
            let cid = correlation::generate_correlation_id();
            correlation::with_correlation_id(cid, Self::eval_brief_lock(bridge, scope, &js_code))
                .await
        }
    }

    /// Invoke an optional JS global function with the brief-lock pattern.
    ///
    /// Returns `Ok(None)` when the JS function is absent and `Ok(Some(value))` when it exists.
    /// Any thrown/returned JS error is surfaced as `Err(BamlRtError::QuickJs(...))`.
    pub async fn invoke_optional_js_function_nonblocking(
        bridge: Arc<Mutex<Self>>,
        scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<Option<Value>> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code =
            invocation::build_optional_js_function_invoke_js_code(function_name, &args_json);
        let result = if correlation::current_correlation_id().is_some() {
            Self::eval_brief_lock(bridge, scope, &js_code).await?
        } else {
            let cid = correlation::generate_correlation_id();
            correlation::with_correlation_id(cid, Self::eval_brief_lock(bridge, scope, &js_code))
                .await?
        };
        Self::parse_optional_js_function_result(function_name, result)
    }

    /// Invoke a JS tool with the brief-lock pattern.
    /// The bridge lock is released before the promise-polling loop so concurrent contexts
    /// are not blocked behind this invocation's async tool execution.
    pub async fn invoke_js_tool_nonblocking(
        bridge: Arc<Mutex<Self>>,
        scope: &InvocationScope,
        tool_name: &str,
        args: Value,
    ) -> Result<Value> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code = invocation::build_js_tool_invoke_js_code(tool_name, &args_json);
        if correlation::current_correlation_id().is_some() {
            Self::eval_brief_lock(bridge, scope, &js_code).await
        } else {
            let cid = correlation::generate_correlation_id();
            correlation::with_correlation_id(cid, Self::eval_brief_lock(bridge, scope, &js_code))
                .await
        }
    }

    /// Invoke a JavaScript function and wait for its promise to resolve.
    ///
    /// **Scope / conversation routing:** Caller must pass the invocation scope for this request
    /// (one scope per A2A conversation). The entire JS run executes inside that scope so native
    /// callbacks and yielded chunks are attributed to the correct conversation. Multiple parallel
    /// conversations each use their own scope when the host invokes this.
    ///
    /// Deliver a resume message into an active stream session so the shim's pending
    /// `awaitInput` Promise resolves and the same JS run continues. Must be called from
    /// the same thread as the collector (handover thread).
    ///
    /// **Deadlock-free:** Takes `Arc<Mutex<QuickJSBridge>>` so the caller does not hold the lock.
    /// The promise-poll loop runs with brief locks only (lock → run pending jobs → unlock each
    /// iteration), so the LLM completion task can acquire the bridge lock to resolve the promise.
    pub async fn deliver_resume_input(
        bridge: Arc<Mutex<Self>>,
        session_id: StreamSessionId,
        request: Value,
    ) -> Result<()> {
        tracing::debug!(session_id = %session_id, "resume: deliver input begin");
        let (inv_scope, message) = {
            let scope = {
                let guard = bridge.lock().await;
                guard
                    .stream_sessions
                    .get(&session_id)
                    .map(|s| s.scope.clone())
                    .ok_or_else(|| {
                        BamlRtError::QuickJs(format!(
                            "stream session {} not found for resume delivery",
                            session_id
                        ))
                    })?
            };
            let inv_scope = InvocationScope::new(scope);
            let mut message = request
                .get("params")
                .and_then(|p| p.get("message"))
                .cloned()
                .unwrap_or(request);
            if let Some(obj) = message.as_object_mut() {
                obj.insert("__session".to_string(), json!(session_id.0));
            }
            (inv_scope, message)
        };
        tracing::debug!(
            session_id = %session_id,
            context_id = %inv_scope.context_id(),
            "resume: session scope resolved and message tagged"
        );
        let context_id_for_log = inv_scope.context_id().to_string();
        // Return the promise (do not await). run_eval_once_for_brief_poll wraps this in
        // direct_code that attaches .then/.catch and returns "__EVAL_PROMISE_PENDING__"
        // so the eval returns immediately and we never hold the bridge lock across the
        // promise resolution (avoids deadlock with LLM completion).
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    const args = {};
                    const func = globalThis["onChatMessage"];
                    if (func === undefined || typeof func !== 'function') {{
                        return JSON.stringify({{ error: "JS function not found: onChatMessage" }});
                    }}
                    return func(args);
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            serde_json::to_string(&message).map_err(BamlRtError::Json)?
        );

        let run_correlation = correlation::current_correlation_id().is_some();
        let correlation_id = correlation::current_correlation_id();
        let cid_for_run = correlation_id.clone();
        let run = async move {
            // Hold the bridge lock only for sync prepare; run the eval without the lock so the
            // worker and TaskManager can make progress (fixes resume deadlock).
            let prepared = {
                let mut guard = bridge.lock().await;
                guard.prepare_brief_poll_eval(&inv_scope, &js_code)?
            };
            tracing::debug!(
                token = %prepared.eval_token.0,
                "resume: prepared brief poll eval, running without bridge lock"
            );
            let eval_result = if run_correlation {
                QuickJSBridge::run_prepared_brief_poll_eval(prepared).await
            } else {
                correlation::with_correlation_id(
                    cid_for_run.unwrap_or_else(correlation::generate_correlation_id),
                    QuickJSBridge::run_prepared_brief_poll_eval(prepared),
                )
                .await
            }?;

            match eval_result {
                EvalOnceResult::Sync(result) => {
                    if let Some(err) = result.get("error").and_then(Value::as_str) {
                        return Err(BamlRtError::QuickJs(format!(
                            "JS stream resume error: {}",
                            err
                        )));
                    }
                    Ok(())
                }
                EvalOnceResult::PromisePending(params) => {
                    tracing::debug!(
                        token = %params.eval_token.0,
                        "resume: promise pending; bounded poll for resume completion"
                    );
                    // Bounded poll: preserve resume semantics when promise settles quickly, but
                    // never allow this path to hang indefinitely. 15s allows CI/slow runners.
                    let poll_result = tokio::time::timeout(
                        tokio::time::Duration::from_secs(15),
                        promise_polling::poll_promise_until_result(
                            promise_polling::PollPromiseParams {
                                runtime: Some(params.runtime.clone()),
                                eval_results_by_token: &params.eval_results_by_token,
                                eval_token: &params.eval_token,
                                token_to_remove: None,
                                invocation_scope_by_token: &params.invocation_scope_by_token,
                                scope: &params.scope,
                                effect_liveness: params.effect_liveness.clone(),
                                idle_timeout_ms: params.idle_timeout_ms,
                                max_attempts_ms: params.max_attempts_ms,
                                run_pending_jobs_brief: None,
                                result_notify: Some(params.result_notify.clone()),
                            },
                        ),
                    )
                    .await;
                    if let Ok(Ok(result_str)) = poll_result {
                        let result: Value = serde_json::from_str(&result_str).map_err(|e| {
                            BamlRtError::QuickJs(format!("resume eval result parse error: {}", e))
                        })?;
                        if let Some(err) = result.get("error").and_then(Value::as_str) {
                            return Err(BamlRtError::QuickJs(format!(
                                "JS stream resume error: {}",
                                err
                            )));
                        }
                    } else if let Ok(Err(e)) = poll_result {
                        return Err(e);
                    } else {
                        tracing::warn!(
                            token = %params.eval_token.0,
                            "resume: bounded poll timed out; continuing with stream-driven completion"
                        );
                    }
                    drop(params);
                    Ok(())
                }
            }
        };

        if run_correlation {
            let result = run.await;
            tracing::debug!(
                session_id = %session_id,
                context_id = %context_id_for_log,
                ok = result.is_ok(),
                "resume: deliver input end"
            );
            result
        } else {
            let result = correlation::with_correlation_id(
                correlation_id.unwrap_or_else(correlation::generate_correlation_id),
                run,
            )
            .await;
            tracing::debug!(
                session_id = %session_id,
                context_id = %context_id_for_log,
                ok = result.is_ok(),
                "resume: deliver input end"
            );
            result
        }
    }

    pub async fn invoke_optional_js_function(
        &mut self,
        function_name: &str,
        args: Value,
    ) -> Result<Option<Value>> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code =
            invocation::build_optional_js_function_invoke_js_code(function_name, &args_json);

        let result = if correlation::current_correlation_id().is_some() {
            self.evaluate(None, &js_code).await?
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(None, &js_code).await
            })
            .await?
        };
        Self::parse_optional_js_function_result(function_name, result)
    }

    /// Invoke a streaming JavaScript or BAML function by name.
    ///
    /// This prefers a JavaScript function named `<function_name>Stream` if present,
    /// then falls back to BAML streaming via __baml_stream.
    pub async fn invoke_function_stream(
        &mut self,
        invocation_scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<Vec<Value>> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code = invocation::build_stream_invoke_js_code(function_name, &args_json);

        let result = if correlation::current_correlation_id().is_some() {
            self.evaluate(Some(invocation_scope), &js_code).await?
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(Some(invocation_scope), &js_code).await
            })
            .await?
        };
        match result {
            Value::Array(values) => Ok(values),
            Value::Object(map) if map.get("error").is_some() => Err(BamlRtError::QuickJs(format!(
                "A2A stream invocation error: {}",
                map.get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            ))),
            other => Ok(vec![other]),
        }
    }

    fn parse_optional_js_function_result(
        function_name: &str,
        result: Value,
    ) -> Result<Option<Value>> {
        if let Value::Object(map) = &result {
            if map
                .get("__absent")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(None);
            }
            if let Some(error) = map.get("error").and_then(Value::as_str) {
                return Err(BamlRtError::QuickJs(format!(
                    "JS function invocation error ({function_name}): {error}"
                )));
            }
        }

        Ok(Some(result))
    }
}

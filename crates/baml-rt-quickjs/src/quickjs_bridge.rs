//! QuickJS integration bridge
//!
//! This module maps BAML function calls (executed in Rust) to QuickJS,
//! allowing JavaScript code to invoke BAML functions.

use crate::baml::BamlRuntimeManager;
use crate::js_value_converter::value_to_js_value_facade;
use baml_rt_core::bus::EffectLiveness;
use baml_rt_core::context::{self, InvocationScope, RuntimeScope};
use baml_rt_core::correlation;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::ToolStep;
use quickjs_runtime::builder::QuickJsRuntimeBuilder;
use quickjs_runtime::facades::QuickJsRuntimeFacade;
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

mod eval;
mod js_codegen;
mod promise_polling;
mod scope;
mod stream;
pub(crate) use stream::BufferDrain;
pub(crate) mod stream_yield;
mod tools;
mod wrappers;

pub use eval::EffectGatedTimeoutPolicy;
use scope::{
    InvocationContextId, InvocationContextRegistry, InvocationToken, next_invocation_token,
    resolve_scope_from_active_context, resolve_scope_from_session,
};

type InvocationScopeMap = Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>;
type CorrelationMap = Arc<StdMutex<HashMap<InvocationToken, baml_rt_core::ids::CorrelationId>>>;
type InvocationContextRegistrySlot = Arc<StdMutex<InvocationContextRegistry>>;
type EvalResultMap = Arc<StdMutex<HashMap<InvocationToken, Option<String>>>>;
type StreamSemaphore = Arc<Semaphore>;
type StreamPermit = tokio::sync::OwnedSemaphorePermit;
type YieldSenderSlot = Arc<StdMutex<Option<mpsc::UnboundedSender<Value>>>>;
type YieldReceiver = mpsc::UnboundedReceiver<Value>;
type InFlightCounter = Arc<AtomicU32>;

/// Opaque session identifier for stream invocations.
///
/// Each `invoke_js_function_stream` call allocates a unique `StreamSessionId`.
/// Session-aware native callbacks receive this as their first argument and use it
/// to look up the owning [`StreamInvocationSession`] in `QuickJSBridge::stream_sessions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct StreamSessionId(pub(crate) u64);

impl std::fmt::Display for StreamSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stream-session-{}", self.0)
    }
}

/// Active stream invocation session.
///
/// Held in `QuickJSBridge::stream_sessions` for the lifetime of a single stream
/// invocation. Native callbacks clone the `Arc` to capture the session and resolve
/// scope, correlation id, and cancellation state.
pub(crate) struct StreamInvocationSession {
    /// Retained for diagnostic tracing; not read in production paths yet.
    #[allow(dead_code)]
    pub(crate) id: StreamSessionId,
    pub(crate) scope: RuntimeScope,
    pub(crate) correlation_id: Option<baml_rt_core::ids::CorrelationId>,
    pub(crate) cancel: CancellationToken,
    pub(crate) closed: AtomicBool,
}

impl StreamInvocationSession {
    /// Check whether this session has been finalized or cancelled.
    pub(crate) fn is_terminated(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.cancel.is_cancelled()
    }
}

pub(crate) type StreamSessionMap =
    Arc<StdMutex<HashMap<StreamSessionId, Arc<StreamInvocationSession>>>>;

/// RAII guard that decrements the in-flight counter when dropped.
///
/// Used inside `JsValueFacade::new_promise` async bodies so the counter is
/// decremented even on panic/cancellation.
struct InFlightGuard(InFlightCounter);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// Ensures evaluate() bookkeeping is cleaned up on all exits, including cancellation/drop.
struct EvalLifecycleGuard {
    eval_results_by_token: EvalResultMap,
    invocation_context_registry: InvocationContextRegistrySlot,
    eval_token: InvocationToken,
    context_id_to_exit: Option<InvocationContextId>,
    eval_slot_registered: bool,
}

impl EvalLifecycleGuard {
    fn new(
        eval_results_by_token: EvalResultMap,
        invocation_context_registry: InvocationContextRegistrySlot,
        eval_token: InvocationToken,
        context_id_to_exit: Option<InvocationContextId>,
    ) -> Self {
        Self {
            eval_results_by_token,
            invocation_context_registry,
            eval_token,
            context_id_to_exit,
            eval_slot_registered: false,
        }
    }

    fn mark_eval_slot_registered(&mut self) {
        self.eval_slot_registered = true;
    }
}

impl Drop for EvalLifecycleGuard {
    fn drop(&mut self) {
        if self.eval_slot_registered
            && let Ok(mut guard) = self.eval_results_by_token.lock()
        {
            guard.remove(&self.eval_token);
        }

        if let Some(id) = self.context_id_to_exit.as_ref()
            && let Ok(mut guard) = self.invocation_context_registry.lock()
        {
            guard.exit(id);
        }
    }
}

/// Helper function for creating an empty open_input value.
///
/// This centralizes the pattern of using an empty JSON object as the default
/// open_input when none is provided.
fn empty_open_input() -> Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn tool_step_to_value(step: ToolStep) -> Value {
    match step {
        ToolStep::Streaming { output } => json!({ "status": "streaming", "output": output }),
        ToolStep::Done { output } => json!({ "status": "done", "output": output }),
        ToolStep::Error { error } => json!({
            "status": "error",
            "error": {
                "kind": format!("{:?}", error.kind),
                "message": error.message,
                "retryable": error.retryable
            }
        }),
    }
}

/// Bridge between QuickJS JavaScript runtime and BAML functions
///
/// BAML functions execute in Rust. This bridge exposes them to QuickJS
/// so JavaScript code can call them.
pub struct QuickJSBridge {
    runtime: QuickJsRuntimeFacade,
    baml_manager: Arc<Mutex<BamlRuntimeManager>>,
    js_tools: HashSet<String>, // Track JavaScript-only tools
    agent_id: baml_rt_core::ids::AgentId,
    effect_liveness: Option<Arc<dyn EffectLiveness>>,
    idle_timeout_ms: u64,
    max_attempts_ms: u64,
    /// Host-only active invocation stack; natives resolve scope from current top (tokenless).
    invocation_context_registry: InvocationContextRegistrySlot,
    /// Token → scope (legacy); still populated for eval result tracking; natives prefer active context.
    invocation_scope_by_token: InvocationScopeMap,
    /// Token -> correlation id captured at invocation entry and propagated through native callbacks.
    correlation_id_by_token: CorrelationMap,
    /// Token → eval result (None while pending). Strictly keyed by token.
    eval_results_by_token: EvalResultMap,
    /// Token for the active stream (legacy); unused when tokenless.
    current_stream_token: Option<InvocationToken>,
    /// Active stream invocation context; exited when stream is finalized or next stream starts.
    current_stream_context_id: Option<InvocationContextId>,
    /// Only one stream invocation may be active at a time so invocation token state
    /// is not overwritten by a concurrent stream. Acquired in invoke_js_function_stream, released in get_a2a_yield_buffer.
    stream_semaphore: StreamSemaphore,
    /// Permit held while a stream is active; dropped in get_a2a_yield_buffer.
    stream_permit: Option<StreamPermit>,
    /// Active stream yield sink. JS host callback pushes chunks into this channel.
    a2a_yield_tx_slot: YieldSenderSlot,
    /// Active stream yield receiver, drained by get_a2a_yield_buffer().
    a2a_yield_rx: Option<YieldReceiver>,
    /// Number of `__baml_invoke` / `__baml_stream` async bodies currently in-flight on tokio.
    /// Incremented synchronously on the event-loop thread when the native is called;
    /// decremented (via [`InFlightGuard`]) when the async body completes.
    in_flight_invoke_count: InFlightCounter,
    /// Active stream sessions keyed by session id. Populated in `invoke_js_function_stream`,
    /// drained in `finalize_a2a_stream_invocation`. Session-aware natives resolve scope
    /// from this map instead of the LIFO `invocation_context_registry`.
    stream_sessions: StreamSessionMap,
    /// Monotonic counter for allocating unique `StreamSessionId` values.
    next_stream_session_id: AtomicU64,
    /// Session id for the currently active stream invocation (at most one due to semaphore).
    /// Used by `finalize_a2a_stream_invocation` to close and remove the session.
    current_stream_session_id: Option<StreamSessionId>,
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

        let runtime = builder.build();

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
            invocation_context_registry: Arc::new(StdMutex::new(InvocationContextRegistry::new())),
            invocation_scope_by_token: Arc::new(StdMutex::new(HashMap::new())),
            correlation_id_by_token: Arc::new(StdMutex::new(HashMap::new())),
            eval_results_by_token: Arc::new(StdMutex::new(HashMap::new())),
            current_stream_token: None,
            current_stream_context_id: None,
            stream_semaphore: Arc::new(Semaphore::new(1)),
            stream_permit: None,
            a2a_yield_tx_slot: Arc::new(StdMutex::new(None)),
            a2a_yield_rx: None,
            in_flight_invoke_count: Arc::new(AtomicU32::new(0)),
            stream_sessions: Arc::new(StdMutex::new(HashMap::new())),
            next_stream_session_id: AtomicU64::new(1),
            current_stream_session_id: None,
        };

        // Initialize sandbox - remove dangerous globals and implement safe console
        // INVARIANT L1: Bridge initialization must terminate within bounded time
        // Timeout is handled in initialize_sandbox() itself
        bridge.initialize_sandbox().await?;

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

    /// Agent ID for this bridge (set at construction; used for attribution and scope).
    pub fn agent_id(&self) -> &baml_rt_core::ids::AgentId {
        &self.agent_id
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
        // If this hangs, the QuickJS runtime may have internal blocking
        use tokio::time::{Duration, timeout};
        timeout(
            Duration::from_secs(5),
            self.runtime.eval(None, script),
        )
        .await
        .map_err(|_| BamlRtError::QuickJs(
            "Sandbox initialization timed out after 5 seconds - QuickJS runtime.eval() may be blocking".to_string()
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

        let manager = self.baml_manager.lock().await;
        let functions = manager.list_functions();
        drop(manager); // Release lock before async operation

        // First, register helper functions that JavaScript can call to invoke BAML functions
        self.register_baml_invoke_helper().await?;
        self.register_baml_stream_helper().await?;
        self.register_await_helper().await?;

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
        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();
        let in_flight = self.in_flight_invoke_count.clone();

        self.runtime.set_function(
            &[],
            "__baml_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                // Resolve scope from active context. When no context is active (e.g. orphaned
                // continuation after stream finalization), return a rejected promise instead of
                // throwing synchronously. A synchronous throw inside a promise-reaction handler
                // becomes an unhandled rejection that crashes the runtime; a rejected promise
                // propagates cleanly through JS await/try-catch.
                let scope = match resolve_scope_from_active_context(&registry) {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = e.to_string();
                        tracing::debug!(error = %msg, "__baml_invoke: no active invocation context, returning rejected promise");
                        return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                            Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                        }));
                    }
                };
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (function_name, args)"));
                }

                let func_name_js = &args[0];
                let func_name = if func_name_js.is_string() {
                    func_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Function name must be a string"));
                };

                let args_js = &args[1];
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string - use JSON.stringify in JavaScript"));
                };

                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let func_name_clone = func_name.clone();
                let manager_for_promise = manager_clone.clone();
                let correlation_id = registry
                    .lock()
                    .ok()
                    .and_then(|g| g.current_frame().ok())
                    .and_then(|f| f.correlation_id);
                let scope_for_tools = scope.clone();

                // Track this async body in the in-flight counter so the stream
                // quiescence barrier knows when it is safe to tear down the context.
                in_flight.fetch_add(1, Ordering::Release);
                let guard_counter = in_flight.clone();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let _in_flight_guard = InFlightGuard(guard_counter);
                    let run = async move {
                        context::with_scope(scope, async move {
                            let manager = manager_for_promise.lock().await;
                            let invocation_scope = InvocationScope::new(scope_for_tools.clone());
                            let value = manager
                                .invoke_function(&invocation_scope, &func_name_clone, args_json)
                                .await
                                .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                            let result = manager.execute_tool_from_baml_result_or_value(&scope_for_tools, value).await
                                .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                            Ok(value_to_js_value_facade(result))
                        })
                        .await
                    };
                    if let Some(correlation_id) = correlation_id {
                        correlation::with_correlation_id(correlation_id, run).await
                    } else {
                        run.await
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register helper function".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered __baml_invoke helper function with async promise support");
        Ok(())
    }

    /// Register a helper function that can await promises and return JSON strings
    /// This helps with the synchronous eval() limitation
    async fn register_await_helper(&mut self) -> Result<()> {
        // Register a helper that synchronously extracts promise results
        // This will be used by evaluate() to handle promises
        let js_code = r#"
            globalThis.__awaitAndStringify = async function(promise) {
                try {
                    const result = await promise;
                    // Return the result directly, not wrapped in success notification
                    return JSON.stringify(result);
                } catch (e) {
                    return JSON.stringify({ error: e.toString() });
                }
            };

            // Helper to synchronously check if a value is a promise
            globalThis.__isPromise = function(value) {
                return value && typeof value.then === 'function';
            };
        "#;

        let script = Script::new("await_helper.js", js_code);
        self.runtime
            .eval(None, script)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register await helper".to_string(),
                source: Box::new(e),
            })?;

        // Register eval-result setter: __set_eval_result(token, json_string)
        let eval_results = self.eval_results_by_token.clone();
        self.runtime.set_function(
            &[],
            "__set_eval_result",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, json_string)"));
                }
                let token = if args[0].is_string() {
                    args[0].get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Token must be a string"));
                };
                let json_str = if args[1].is_string() {
                    args[1].get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("json_string must be a string"));
                };
                let mut guard = eval_results
                    .lock()
                    .map_err(|_| quickjs_runtime::jsutils::JsError::new_str("eval_results lock poisoned"))?;
                let key = InvocationToken(token);
                if !guard.contains_key(&key) {
                    // Late completion after timeout/cancel: token slot may already be gone.
                    // Ignore to keep completion idempotent.
                    return Ok(JsValueFacade::Undefined);
                }
                guard.insert(key, Some(json_str));
                Ok(JsValueFacade::Undefined)
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __set_eval_result".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered __awaitAndStringify helper function");
        Ok(())
    }

    /// Register __baml_stream. Tokenless: host resolves scope from active context. JS calls (function_name, args).
    async fn register_baml_stream_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();
        let in_flight = self.in_flight_invoke_count.clone();

        self.runtime.set_function(
            &[],
            "__baml_stream",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                // Same graceful missing-context handling as __baml_invoke.
                let scope = match resolve_scope_from_active_context(&registry) {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = e.to_string();
                        tracing::debug!(error = %msg, "__baml_stream: no active invocation context, returning rejected promise");
                        return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                            Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                        }));
                    }
                };
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (function_name, args)"));
                }

                let func_name_js = &args[0];
                let func_name = if func_name_js.is_string() {
                    func_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Function name must be a string"));
                };

                let args_js = &args[1];
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Second argument must be a JSON string"));
                };

                let args_json: Value = match serde_json::from_str(&args_json_str) {
                    Ok(v) => v,
                    Err(e) => return Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e))),
                };

                let func_name_clone = func_name.clone();
                let correlation_id = registry
                    .lock()
                    .ok()
                    .and_then(|g| g.current_frame().ok())
                    .and_then(|f| f.correlation_id);
                let manager_for_stream = manager_clone.clone();
                let correlation_id_for_spawn = correlation_id.clone();

                // Track in-flight for quiescence barrier (same as __baml_invoke).
                in_flight.fetch_add(1, Ordering::Release);
                let guard_counter = in_flight.clone();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let _in_flight_guard = InFlightGuard(guard_counter);
                    let run = async move {
                        use tokio::sync::mpsc;
                        let (tx, mut rx) = mpsc::channel::<serde_json::Value>(100);
                        let func_name_stream = func_name_clone.clone();
                        let args_json_stream = args_json.clone();
                        let spawn_correlation_id = correlation_id_for_spawn.clone();
                        let spawn_scope = scope.clone();
                        let scope_for_stream = spawn_scope.clone();
                        // Spawn a task to run the stream and send incremental results
                        tokio::spawn(async move {
                            let run = async move {
                                context::with_scope(spawn_scope, async move {
                                if args_json_stream
                                    .get("__scope_probe")
                                    .and_then(Value::as_bool)
                                    == Some(true)
                                {
                                    let payload = json!({
                                        "context_id": scope_for_stream.context_id().to_string(),
                                        "message_id": scope_for_stream.message_id().to_string(),
                                        "task_id": scope_for_stream.task_id_opt().map(|id| id.to_string()),
                                    });
                                    if let Err(e) = tx.send(payload).await {
                                        tracing::warn!(error = ?e, "Failed to send scope probe payload");
                                    }
                                    return;
                                }

                                // Create the stream
                                let manager = manager_for_stream.lock().await;
                                let invocation_scope =
                                    InvocationScope::new(scope_for_stream.clone());
                                let stream_result = manager.invoke_function_stream(
                                    &invocation_scope,
                                    &func_name_stream,
                                    args_json_stream,
                                );
                                let executor_ref = match manager.executor.as_ref() {
                                    Some(exec) => exec,
                                    None => {
                                        let error_value = serde_json::json!({
                                            "error": "BAML executor not initialized"
                                        });
                                        if let Err(e) = tx.send(error_value).await {
                                            tracing::warn!(error = ?e, "Stream channel send failed");
                                        }
                                        return;
                                    }
                                };
                                let ctx_manager =
                                    match executor_ref.create_ctx_manager_for_scope(
                                        &scope_for_stream,
                                        None,
                                    ) {
                                    Ok(manager) => manager,
                                    Err(err) => {
                                        let error_value = serde_json::json!({
                                            "error": format!("Failed to create context manager: {}", err)
                                        });
                                        if let Err(e) = tx.send(error_value).await {
                                            tracing::warn!(error = ?e, "Stream channel send failed");
                                        }
                                        return;
                                    }
                                };
                                // Create the stream
                                let mut stream = match stream_result {
                                    Ok(s) => s,
                                    Err(e) => {
                                        drop(manager); // Release lock
                                        let error_value = serde_json::json!({"error": format!("Failed to create stream: {}", e)});
                                    if let Err(e) = tx.send(error_value).await {
                                        tracing::warn!(error = ?e, "Stream channel send failed");
                                    }
                                        return;
                                    }
                                };
                                // ctx_manager is owned; drop manager lock before stream.run so that
                                // tool calls (or other re-entry) during the stream can take the lock.
                                drop(manager);
                                let env_vars = HashMap::new();
                                // Set task-local so execute_tool_session_plan can push tool streaming chunks into this channel.
                                let tx_for_yield = tx.clone();
                                stream_yield::scope_stream_yield(Some(tx_for_yield), async move {
                                    let (final_result, _call_id) = stream
                                        .run(
                                            None::<fn()>, // on_tick
                                            Some(|result: baml_runtime::FunctionResult| {
                                                if let Some(Ok(parsed)) = result.parsed()
                                                    && let Ok(parsed_value) =
                                                        serde_json::to_value(parsed.serialize_partial())
                                                    && let Err(e) = tx.try_send(parsed_value)
                                                {
                                                    tracing::warn!(error = ?e, "Stream channel try_send failed");
                                                }
                                            }),
                                            &ctx_manager,
                                            None, // type_builder
                                            None, // client_registry
                                            env_vars,
                                        )
                                        .await;

                                    // Send final result
                                    match final_result {
                                        Ok(result) => {
                                            // parsed() returns Option<Result<ResponseBamlValue, Error>>
                                            if let Some(Ok(parsed)) = result.parsed()
                                                && let Ok(final_value) =
                                                    serde_json::to_value(parsed.serialize_partial())
                                                && let Err(e) = tx.send(final_value).await
                                            {
                                                tracing::warn!(error = ?e, "Stream channel send failed");
                                            }
                                        }
                                        Err(e) => {
                                            let error_value = serde_json::json!({"error": format!("{}", e)});
                                            if let Err(e) = tx.send(error_value).await {
                                                tracing::warn!(error = ?e, "Stream channel send failed");
                                            }
                                        }
                                    }
                                })
                                .await;
                                }).await;
                            };
                            if let Some(correlation_id) = spawn_correlation_id {
                                correlation::with_correlation_id(correlation_id, run).await;
                            } else {
                                run.await;
                            }
                        });

                        // Collect results from the channel into an array
                        let mut results = Vec::new();
                        while let Some(value) = rx.recv().await {
                            results.push(value);
                        }

                        // Convert results array to JsValueFacade directly
                        Ok(value_to_js_value_facade(serde_json::Value::Array(results)))
                    };
                    if let Some(correlation_id) = correlation_id {
                        correlation::with_correlation_id(correlation_id, run).await
                    } else {
                        run.await
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register streaming helper function".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered __baml_stream helper function");
        Ok(())
    }

    /// Register `__baml_invoke_session(session_id, function_name, args_json)`.
    ///
    /// Session-aware mirror of `__baml_invoke`. Stream JS wrappers call this with a
    /// baked-in session id so scope is resolved from the session map, not the LIFO registry.
    async fn register_baml_invoke_session_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let sessions = self.stream_sessions.clone();
        let in_flight = self.in_flight_invoke_count.clone();

        self.runtime.set_function(
            &[],
            "__baml_invoke_session",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 3 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (session_id, function_name, args)"));
                }
                let session_id = tools::parse_session_id_arg(&args)?;
                let (scope, session) = match resolve_scope_from_session(&sessions, session_id) {
                    Ok(pair) => pair,
                    Err(e) => {
                        let msg = e.to_string();
                        tracing::debug!(error = %msg, %session_id, "__baml_invoke_session: session lookup failed, returning rejected promise");
                        return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                            Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                        }));
                    }
                };

                let func_name = if args[1].is_string() {
                    args[1].get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Function name must be a string"));
                };
                let args_json_str = if args[2].is_string() {
                    args[2].get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string"));
                };
                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let manager_for_promise = manager_clone.clone();
                let correlation_id = session.correlation_id.clone();
                let scope_for_tools = scope.clone();
                let cancel = session.cancel.clone();

                in_flight.fetch_add(1, Ordering::Release);
                let guard_counter = in_flight.clone();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let _in_flight_guard = InFlightGuard(guard_counter);
                    // Cancellation checkpoint: entry
                    if cancel.is_cancelled() {
                        return Err(quickjs_runtime::jsutils::JsError::new_str("Invocation cancelled"));
                    }
                    let cancel_inner = cancel.clone();
                    let run = async move {
                        context::with_scope(scope, async move {
                            let manager = manager_for_promise.lock().await;
                            // Cancellation checkpoint: after acquiring manager lock
                            if cancel_inner.is_cancelled() {
                                return Err(quickjs_runtime::jsutils::JsError::new_str("Invocation cancelled"));
                            }
                            let invocation_scope = InvocationScope::new(scope_for_tools.clone());
                            let value = manager
                                .invoke_function(&invocation_scope, &func_name, args_json)
                                .await
                                .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                            // Cancellation checkpoint: after BAML invoke, before tool execution
                            if cancel_inner.is_cancelled() {
                                return Err(quickjs_runtime::jsutils::JsError::new_str("Invocation cancelled"));
                            }
                            let result = manager.execute_tool_from_baml_result_or_value(&scope_for_tools, value).await
                                .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                            Ok(value_to_js_value_facade(result))
                        })
                        .await
                    };
                    if let Some(correlation_id) = correlation_id {
                        correlation::with_correlation_id(correlation_id, run).await
                    } else {
                        run.await
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __baml_invoke_session".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered __baml_invoke_session helper function");
        Ok(())
    }

    /// Register `__baml_stream_session(session_id, function_name, args_json)`.
    ///
    /// Session-aware mirror of `__baml_stream`. Same structure as `__baml_invoke_session`
    /// but spawns a BAML stream with incremental results.
    async fn register_baml_stream_session_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let sessions = self.stream_sessions.clone();
        let in_flight = self.in_flight_invoke_count.clone();

        self.runtime.set_function(
            &[],
            "__baml_stream_session",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 3 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (session_id, function_name, args)"));
                }
                let session_id = tools::parse_session_id_arg(&args)?;
                let (scope, session) = match resolve_scope_from_session(&sessions, session_id) {
                    Ok(pair) => pair,
                    Err(e) => {
                        let msg = e.to_string();
                        tracing::debug!(error = %msg, %session_id, "__baml_stream_session: session lookup failed, returning rejected promise");
                        return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                            Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                        }));
                    }
                };

                let func_name = if args[1].is_string() {
                    args[1].get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Function name must be a string"));
                };
                let args_json_str = if args[2].is_string() {
                    args[2].get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string"));
                };
                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let manager_for_stream = manager_clone.clone();
                let correlation_id = session.correlation_id.clone();
                let cancel = session.cancel.clone();

                in_flight.fetch_add(1, Ordering::Release);
                let guard_counter = in_flight.clone();

                let correlation_id_for_wrap = correlation_id.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let _in_flight_guard = InFlightGuard(guard_counter);
                    if cancel.is_cancelled() {
                        return Err(quickjs_runtime::jsutils::JsError::new_str("Invocation cancelled"));
                    }
                    let run = async move {
                        use tokio::sync::mpsc;
                        let (tx, mut rx) = mpsc::channel::<serde_json::Value>(100);
                        let func_name_stream = func_name.clone();
                        let args_json_stream = args_json.clone();
                        let spawn_correlation_id = correlation_id.clone();
                        let spawn_scope = scope.clone();
                        let scope_for_stream = spawn_scope.clone();
                        let cancel_for_spawn = cancel.clone();
                        tokio::spawn(async move {
                            let run = async move {
                                context::with_scope(spawn_scope, async move {
                                if cancel_for_spawn.is_cancelled() {
                                    return;
                                }
                                if args_json_stream
                                    .get("__scope_probe")
                                    .and_then(Value::as_bool)
                                    == Some(true)
                                {
                                    let payload = json!({
                                        "context_id": scope_for_stream.context_id().to_string(),
                                        "message_id": scope_for_stream.message_id().to_string(),
                                        "task_id": scope_for_stream.task_id_opt().map(|id| id.to_string()),
                                    });
                                    if let Err(e) = tx.send(payload).await {
                                        tracing::warn!(error = ?e, "Failed to send scope probe payload");
                                    }
                                    return;
                                }

                                let manager = manager_for_stream.lock().await;
                                // Cancellation checkpoint: after acquiring manager lock
                                if cancel_for_spawn.is_cancelled() {
                                    return;
                                }
                                let invocation_scope =
                                    InvocationScope::new(scope_for_stream.clone());
                                let stream_result = manager.invoke_function_stream(
                                    &invocation_scope,
                                    &func_name_stream,
                                    args_json_stream,
                                );
                                let executor_ref = match manager.executor.as_ref() {
                                    Some(exec) => exec,
                                    None => {
                                        let error_value = serde_json::json!({
                                            "error": "BAML executor not initialized"
                                        });
                                        if let Err(e) = tx.send(error_value).await {
                                            tracing::warn!(error = ?e, "Stream channel send failed");
                                        }
                                        return;
                                    }
                                };
                                let ctx_manager =
                                    match executor_ref.create_ctx_manager_for_scope(
                                        &scope_for_stream,
                                        None,
                                    ) {
                                    Ok(manager) => manager,
                                    Err(err) => {
                                        let error_value = serde_json::json!({
                                            "error": format!("Failed to create context manager: {}", err)
                                        });
                                        if let Err(e) = tx.send(error_value).await {
                                            tracing::warn!(error = ?e, "Stream channel send failed");
                                        }
                                        return;
                                    }
                                };
                                let mut stream = match stream_result {
                                    Ok(s) => s,
                                    Err(e) => {
                                        drop(manager);
                                        let error_value = serde_json::json!({"error": format!("Failed to create stream: {}", e)});
                                    if let Err(e) = tx.send(error_value).await {
                                        tracing::warn!(error = ?e, "Stream channel send failed");
                                    }
                                        return;
                                    }
                                };
                                drop(manager);
                                // Cancellation checkpoint: after setup, before expensive stream.run()
                                if cancel_for_spawn.is_cancelled() {
                                    return;
                                }
                                let env_vars = HashMap::new();
                                let tx_for_yield = tx.clone();
                                stream_yield::scope_stream_yield(Some(tx_for_yield), async move {
                                    let (final_result, _call_id) = stream
                                        .run(
                                            None::<fn()>,
                                            Some(|result: baml_runtime::FunctionResult| {
                                                if let Some(Ok(parsed)) = result.parsed()
                                                    && let Ok(parsed_value) =
                                                        serde_json::to_value(parsed.serialize_partial())
                                                    && let Err(e) = tx.try_send(parsed_value)
                                                {
                                                    tracing::warn!(error = ?e, "Stream channel try_send failed");
                                                }
                                            }),
                                            &ctx_manager,
                                            None,
                                            None,
                                            env_vars,
                                        )
                                        .await;

                                    match final_result {
                                        Ok(result) => {
                                            if let Some(Ok(parsed)) = result.parsed()
                                                && let Ok(final_value) =
                                                    serde_json::to_value(parsed.serialize_partial())
                                                && let Err(e) = tx.send(final_value).await
                                            {
                                                tracing::warn!(error = ?e, "Stream channel send failed");
                                            }
                                        }
                                        Err(e) => {
                                            let error_value = serde_json::json!({"error": format!("{}", e)});
                                            if let Err(e) = tx.send(error_value).await {
                                                tracing::warn!(error = ?e, "Stream channel send failed");
                                            }
                                        }
                                    }
                                })
                                .await;
                                }).await;
                            };
                            if let Some(correlation_id) = spawn_correlation_id {
                                correlation::with_correlation_id(correlation_id, run).await;
                            } else {
                                run.await;
                            }
                        });

                        let mut results = Vec::new();
                        while let Some(value) = rx.recv().await {
                            results.push(value);
                        }
                        Ok(value_to_js_value_facade(serde_json::Value::Array(results)))
                    };
                    if let Some(correlation_id) = correlation_id_for_wrap {
                        correlation::with_correlation_id(correlation_id, run).await
                    } else {
                        run.await
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __baml_stream_session".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered __baml_stream_session helper function");
        Ok(())
    }

    /// Register a single BAML function with QuickJS (tokenless wrapper).
    async fn register_single_function(&mut self, function_name: &str) -> Result<()> {
        let js_code = wrappers::build_token_args_wrapper(
            function_name,
            &format!(
                "__baml_invoke(\"{}\", JSON.stringify(argObj))",
                function_name.replace('\\', "\\\\").replace('"', "\\\"")
            ),
        );

        let script = Script::new("register_function.js", &js_code);
        let _result =
            self.runtime
                .eval(None, script)
                .await
                .map_err(|e| BamlRtError::QuickJsWithSource {
                    context: "Failed to register function".to_string(),
                    source: Box::new(e),
                })?;

        tracing::debug!(function = function_name, "Registered function with QuickJS");

        Ok(())
    }

    /// Register a streaming version of a single BAML function with QuickJS
    async fn register_single_stream_function(&mut self, function_name: &str) -> Result<()> {
        // Register a JavaScript wrapper function for streaming
        let stream_function_name = format!("{}Stream", function_name);
        let js_code = wrappers::build_token_args_wrapper(
            &stream_function_name,
            &format!(
                "__baml_stream(\"{}\", JSON.stringify(argObj))",
                function_name.replace('\\', "\\\\").replace('"', "\\\"")
            ),
        );

        let script = Script::new("register_stream_function.js", &js_code);
        let _result =
            self.runtime
                .eval(None, script)
                .await
                .map_err(|e| BamlRtError::QuickJsWithSource {
                    context: "Failed to register stream function".to_string(),
                    source: Box::new(e),
                })?;

        tracing::debug!(
            function = function_name,
            stream_function = stream_function_name,
            "Registered streaming function with QuickJS"
        );

        Ok(())
    }

    /// Legacy: create token and register scope (used only for correlation map when needed).
    #[allow(dead_code)]
    fn create_invocation_token(&mut self, scope: &InvocationScope) -> (InvocationToken, String) {
        let token = next_invocation_token();
        let prelude = format!(
            "const __baml_invocation_token = \"{}\";",
            token.0.replace('\\', "\\\\").replace('"', "\\\"")
        );
        if let Ok(mut map) = self.invocation_scope_by_token.lock() {
            map.insert(token.clone(), scope.as_scope().clone());
        }
        if let Some(correlation_id) = correlation::current_correlation_id()
            && let Ok(mut map) = self.correlation_id_by_token.lock()
        {
            map.insert(token.clone(), correlation_id);
        }
        (token, prelude)
    }

    /// Remove an invocation token so it can no longer be used for scope lookup. Call when the
    /// invocation completes (after evaluate returns for non-stream, or in get_a2a_yield_buffer for stream).
    fn remove_invocation_token(&mut self, token: &InvocationToken) {
        if let Ok(mut map) = self.invocation_scope_by_token.lock() {
            map.remove(token);
        }
        if let Ok(mut map) = self.correlation_id_by_token.lock() {
            map.remove(token);
        }
    }

    /// Run a single script with explicit invocation scope available through token prelude.
    ///
    /// Scope lookup for native callbacks is token-authoritative; this helper keeps API shape
    /// stable while delegating to runtime eval.
    pub async fn run_eval_with_scope(
        &self,
        scope: &InvocationScope,
        script: Script,
        clear_after: bool,
    ) -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
        scope::run_eval_with_scope(&self.runtime, scope, script, clear_after).await
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

    /// Execute JavaScript code in the QuickJS context.
    ///
    /// The code should return a JSON string or a promise that resolves to a JSON string.
    /// If code returns a promise, we wait for it to resolve (see [`promise_polling`]).
    ///
    /// **Scope:** When `scope` is `Some`, we push it on the host invocation-context stack so
    /// native callbacks resolve scope tokenlessly. We also create an eval token for result
    /// tracking. Cleanup is guarded so it always runs on success, error, or cancellation.
    pub async fn evaluate(&mut self, scope: Option<&InvocationScope>, code: &str) -> Result<Value> {
        tracing::trace!(code = code, "Executing JavaScript code");

        // Enter host invocation context so natives can resolve scope without a token from JS.
        let context_id_to_exit: Option<InvocationContextId> = if let Some(s) = scope {
            let correlation_id = correlation::current_correlation_id();
            let mut guard = self.invocation_context_registry.lock().map_err(|_| {
                BamlRtError::QuickJs("invocation context registry lock poisoned".to_string())
            })?;
            Some(guard.enter(s.as_scope().clone(), correlation_id))
        } else {
            None
        };

        let eval_token = next_invocation_token();
        let mut lifecycle_guard = EvalLifecycleGuard::new(
            self.eval_results_by_token.clone(),
            self.invocation_context_registry.clone(),
            eval_token.clone(),
            context_id_to_exit,
        );

        {
            let mut guard = self
                .eval_results_by_token
                .lock()
                .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
            if guard.contains_key(&eval_token) {
                return Err(BamlRtError::QuickJs("eval token collision".to_string()));
            }
            guard.insert(eval_token.clone(), None);
        }
        lifecycle_guard.mark_eval_slot_registered();

        let code_trimmed = code.trim_start();
        let is_arrow_iife =
            code_trimmed.starts_with("(()") || code_trimmed.starts_with("(async ()");
        let already_wrapped = code_trimmed.starts_with("(function()")
            || code_trimmed.starts_with("(async function()")
            || is_arrow_iife;
        let code_expr_body = if already_wrapped {
            code_trimmed.to_string()
        } else {
            format!("(function() {{ {} }})()", code)
        };

        // Execute user code exactly once. Promise results are handed off by installing inline
        // then/catch handlers that call __set_eval_result(token, ...), avoiding any second eval
        // or globalThis pending-promise storage.
        let token_literal = eval_token.0.replace('\\', "\\\\").replace('"', "\\\"");
        let direct_code = format!(
            r#"(function() {{
var __r = {code};
if (__r && typeof __r.then === 'function') {{
  Promise.resolve(__r).then(function(__v) {{
    var __json;
    if (typeof __v === 'string') {{
      __json = __v;
    }} else if (typeof __v === 'undefined') {{
      __json = "{{}}";
    }} else {{
      __json = JSON.stringify(__v);
      if (typeof __json === 'undefined') {{
        __json = "{{}}";
      }}
    }}
    __set_eval_result("{token}", __json);
  }}).catch(function(__e) {{
    __set_eval_result("{token}", JSON.stringify({{ error: (__e && __e.toString ? __e.toString() : String(__e)) }}));
  }});
  return "__EVAL_PROMISE_PENDING__";
}}
if (typeof __r === 'string') {{ return __r; }}
if (typeof __r === 'undefined') {{ return "{{}}"; }}
var __sync_json = JSON.stringify(__r);
if (typeof __sync_json === 'undefined') {{ return "{{}}"; }}
return __sync_json;
}})()"#,
            code = code_expr_body,
            token = token_literal,
        );
        let direct_script = Script::new("eval_direct.js", &direct_code);
        let js_result = match scope {
            Some(s) => {
                self.run_eval_with_scope(s, direct_script.clone(), false)
                    .await
            }
            None => self.runtime.eval(None, direct_script).await,
        }
        .map_err(|e| {
            let message = e.to_string();
            BamlRtError::QuickJsWithSource {
                context: format!("Failed to execute JavaScript: {}", message),
                source: Box::new(e),
            }
        })?;

        if !js_result.is_string() {
            // Non-string result (rare edge cases such as unserializable values).
            return Ok(serde_json::json!({}));
        }

        let json_str = js_result.get_str();
        if json_str != "__EVAL_PROMISE_PENDING__" {
            // Sync fast-path: parse JSON; if it's a plain string, keep compatibility.
            if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
                return Ok(parsed);
            }
            return Ok(serde_json::json!({ "result": json_str }));
        }

        // Promise path: wait for __set_eval_result(token, json) from inline handlers above.
        let invocation_scope = scope.ok_or_else(|| {
            BamlRtError::QuickJs(
                "Promise polling requires invocation scope; evaluate(scope=None) must not await promises"
                    .to_string(),
            )
        })?;
        let result_str =
            promise_polling::poll_promise_until_result(promise_polling::PollPromiseParams {
                runtime: &self.runtime,
                eval_results_by_token: &self.eval_results_by_token,
                eval_token: &eval_token,
                token_to_remove: None,
                invocation_scope_by_token: &self.invocation_scope_by_token,
                scope: invocation_scope,
                effect_liveness: self.effect_liveness.clone(),
                idle_timeout_ms: self.idle_timeout_ms,
                max_attempts_ms: self.max_attempts_ms,
            })
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

    /// Invoke a BAML function by name.
    ///
    /// This is a helper method that generates and executes JavaScript code to:
    /// 1. Call the BAML runtime via __baml_invoke
    /// 2. Handle promises correctly using __awaitAndStringify
    ///
    /// # Arguments
    /// * `function_name` - Name of the function to invoke
    /// * `args` - JSON arguments to pass to the function
    ///
    /// # Returns
    /// The result of the function call, either as a string (for successful calls)
    /// or as an error object if the call failed.
    ///
    /// **Scope:** Requires an invocation scope (e.g. run inside `context::with_scope`). A per-invocation
    /// token is bound for `__baml_invoke` calls within the eval scope.
    pub async fn invoke_function(
        &mut self,
        scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<Value> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    const args = {};
                    const promise = __baml_invoke("{}", JSON.stringify(args));
                    return __awaitAndStringify(promise);
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            args_json, function_name
        );

        if correlation::current_correlation_id().is_some() {
            self.evaluate(Some(scope), &js_code).await
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(Some(scope), &js_code).await
            })
            .await
        }
    }

    /// Invoke a JavaScript tool by name.
    ///
    /// This only executes a JavaScript function from globalThis and does not fall back to BAML.
    /// Requires an invocation scope (e.g. run inside `context::with_scope`) so that
    /// a per-invocation token is available for nested __tool_invoke / __baml_invoke calls.
    pub async fn invoke_js_tool(
        &mut self,
        invocation_scope: &InvocationScope,
        tool_name: &str,
        args: Value,
    ) -> Result<Value> {
        self.invoke_js_tool_with_scope(invocation_scope, tool_name, args)
            .await
    }

    /// Invoke a JavaScript tool with an explicit scope.
    ///
    /// Use when scope is not task-local (e.g. when running inside `spawn_blocking` after
    /// capturing scope on the original task). Same behavior as [`invoke_js_tool`](Self::invoke_js_tool).
    pub async fn invoke_js_tool_with_scope(
        &mut self,
        invocation_scope: &InvocationScope,
        tool_name: &str,
        args: Value,
    ) -> Result<Value> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    const args = {};
                    const func = globalThis.__js_tools && globalThis.__js_tools["{}"];
                    if (func === undefined || typeof func !== 'function') {{
                        return JSON.stringify({{ error: "JS tool not found" }});
                    }}
                    return __awaitAndStringify(func(args));
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            args_json, tool_name
        );

        if correlation::current_correlation_id().is_some() {
            self.evaluate(Some(invocation_scope), &js_code).await
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(Some(invocation_scope), &js_code).await
            })
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
    /// **INVARIANT:** For non-stream functions, the promise MUST resolve within bounded time.
    /// For stream functions, use `invoke_js_function_stream()` instead.
    pub async fn invoke_js_function(
        &mut self,
        scope: &InvocationScope,
        function_name: &str,
        args: Value,
    ) -> Result<Value> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    const args = {};
                    const func = globalThis["{}"];
                    if (func === undefined || typeof func !== 'function') {{
                        return JSON.stringify({{ error: "JS function not found: {}" }});
                    }}
                    return __awaitAndStringify(func(args));
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            args_json, function_name, function_name
        );

        let result = if correlation::current_correlation_id().is_some() {
            self.evaluate(Some(scope), &js_code).await
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(Some(scope), &js_code).await
            })
            .await
        }?;

        match &result {
            Value::Object(map) if map.get("error").is_some() => Err(BamlRtError::QuickJs(format!(
                "JS function invocation error ({}): {}",
                function_name,
                map.get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            ))),
            _ => Ok(result),
        }
    }

    pub async fn invoke_optional_js_function(
        &mut self,
        function_name: &str,
        args: Value,
    ) -> Result<Option<Value>> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;

        let js_code = format!(
            r#"
            (function() {{
                try {{
                    const args = {};
                    const func = globalThis["{}"];
                    if (func === undefined || typeof func !== 'function') {{
                        return JSON.stringify({{ __absent: true }});
                    }}
                    return __awaitAndStringify(func(args));
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            args_json, function_name
        );

        let result = if correlation::current_correlation_id().is_some() {
            self.evaluate(None, &js_code).await?
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(None, &js_code).await
            })
            .await?
        };

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
                    "JS function invocation error ({}): {}",
                    function_name, error
                )));
            }
        }

        Ok(Some(result))
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
        let stream_function = format!("{}Stream", function_name);

        let js_code = format!(
            r#"
            (function() {{
                try {{
                    const args = {};
                    let promise;
                    const streamFunc = globalThis["{}"];
                    if (streamFunc !== undefined && typeof streamFunc === 'function') {{
                        promise = streamFunc(args);
                    }} else {{
                        promise = __baml_stream("{}", JSON.stringify(args));
                    }}
                    return __awaitAndStringify(promise);
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            args_json, stream_function, function_name
        );

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
}

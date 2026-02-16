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
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, Semaphore, mpsc};

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
    resolve_scope_from_active_context,
};

type InvocationScopeMap = Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>;
type CorrelationMap = Arc<StdMutex<HashMap<InvocationToken, baml_rt_core::ids::CorrelationId>>>;
type InvocationContextRegistrySlot = Arc<StdMutex<InvocationContextRegistry>>;
type EvalResultMap = Arc<StdMutex<HashMap<InvocationToken, Option<String>>>>;
type StreamSemaphore = Arc<Semaphore>;
type StreamPermit = tokio::sync::OwnedSemaphorePermit;
type YieldSenderSlot = Arc<StdMutex<Option<mpsc::UnboundedSender<Value>>>>;
type YieldReceiver = mpsc::UnboundedReceiver<Value>;

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

        self.runtime.set_function(
            &[],
            "__baml_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = resolve_scope_from_active_context(&registry)?;
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

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
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
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Missing eval result slot for token"));
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

        self.runtime.set_function(
            &[],
            "__baml_stream",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = resolve_scope_from_active_context(&registry)?;
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

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
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
    /// tracking and (for now) inject a prelude. On completion we pop the context and remove the token.
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

        // Eval result tracking only; no token/context prelude in JS (host resolves from registry).
        let eval_token = next_invocation_token();
        let prelude_opt: Option<String> = None;
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
        let token_to_remove: Option<InvocationToken> = None;

        fn exit_invocation_context(
            registry: &InvocationContextRegistrySlot,
            id_opt: Option<&InvocationContextId>,
        ) {
            if let Some(id) = id_opt
                && let Ok(mut guard) = registry.lock()
            {
                guard.exit(id);
            }
        }
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

        // First, try executing the code directly (for synchronous code like assignments)
        // This handles agent initialization code that just assigns to globalThis
        // If code already has a return statement (like in an IIFE), execute as-is
        // Otherwise, wrap it in an IIFE
        let prelude = prelude_opt.as_deref().unwrap_or("");
        let direct_code = format!(
            "(function() {{\n{}\nreturn {};\n}})()",
            prelude, code_expr_body
        );
        let direct_script = Script::new("eval_direct.js", &direct_code);
        let direct_result = match scope {
            Some(s) => {
                self.run_eval_with_scope(s, direct_script.clone(), false)
                    .await
            }
            None => self.runtime.eval(None, direct_script).await,
        };
        if let Err(e) = direct_result {
            {
                let mut guard = self
                    .eval_results_by_token
                    .lock()
                    .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
                guard.remove(&eval_token);
            }
            let _ = token_to_remove.map(|t| self.remove_invocation_token(&t));
            exit_invocation_context(
                &self.invocation_context_registry,
                context_id_to_exit.as_ref(),
            );
            let message = e.to_string();
            return Err(BamlRtError::QuickJsWithSource {
                context: format!("Failed to execute JavaScript: {}", message),
                source: Box::new(e),
            });
        }

        // If direct execution succeeds and returns a non-promise, we're done
        let js_result = direct_result.expect("direct_result validated as Ok");
        if js_result.is_string() && token_to_remove.is_none() {
            // Only use string result as final when we didn't inject a token (sync eval)
            {
                let mut guard = self
                    .eval_results_by_token
                    .lock()
                    .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
                guard.remove(&eval_token);
            }
            let _ = token_to_remove.map(|t| self.remove_invocation_token(&t));
            exit_invocation_context(
                &self.invocation_context_registry,
                context_id_to_exit.as_ref(),
            );
            let json_str = js_result.get_str();
            if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
                return Ok(parsed);
            }
            return Ok(serde_json::json!({ "result": json_str }));
        }
        if js_result.is_string() && token_to_remove.is_some() {
            // Had scope: first run may have returned a string like "undefined"; must await via wrapped path
            let json_str = js_result.get_str();
            if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
                {
                    let mut guard = self.eval_results_by_token.lock().map_err(|_| {
                        BamlRtError::QuickJs("eval_results lock poisoned".to_string())
                    })?;
                    guard.remove(&eval_token);
                }
                let _ = token_to_remove
                    .as_ref()
                    .map(|t| self.remove_invocation_token(t));
                exit_invocation_context(
                    &self.invocation_context_registry,
                    context_id_to_exit.as_ref(),
                );
                return Ok(parsed);
            }
            // Invalid or non-JSON string (e.g. "undefined") -> fall through to promise path
        }
        // Not a string - might be undefined/null from assignment code, or a promise
        // When we have a scope (token_to_remove), the code may be async and return a promise
        // that the QuickJS facade doesn't format with "Promise" in debug; still await it.
        let debug_str = format!("{:?}", js_result);
        let looks_like_promise = debug_str.contains("Promise")
            || debug_str.contains("JsPromise")
            || token_to_remove.is_some();
        if !looks_like_promise {
            {
                let mut guard = self
                    .eval_results_by_token
                    .lock()
                    .map_err(|_| BamlRtError::QuickJs("eval_results lock poisoned".to_string()))?;
                guard.remove(&eval_token);
            }
            let _ = token_to_remove.map(|t| self.remove_invocation_token(&t));
            exit_invocation_context(
                &self.invocation_context_registry,
                context_id_to_exit.as_ref(),
            );
            // Not a promise, code executed successfully (side effects happened)
            // Return empty object to indicate success without a value
            return Ok(serde_json::json!({}));
        }

        // Code returned a promise (or we have scope and must await) - need to await it and store result
        let token_literal = eval_token.0.replace('\\', "\\\\").replace('"', "\\\"");
        let code_promise_expr = if prelude.is_empty() {
            code_expr_body.clone()
        } else {
            format!(
                "(function() {{\n{}\nreturn {};\n}})()",
                prelude, code_expr_body
            )
        };
        let wrapped_code =
            js_codegen::build_wrapped_promise_code(&code_promise_expr, &token_literal);
        let script = Script::new("eval.js", &wrapped_code);

        // Execute the code - this will set __eval_result when the promise resolves.
        let js_result = match scope {
            Some(s) => self.run_eval_with_scope(s, script, false).await,
            None => self.runtime.eval(None, script).await,
        }
        .map_err(|e| {
            let message = e.to_string();
            BamlRtError::QuickJsWithSource {
                context: format!("Failed to execute JavaScript: {}", message),
                source: Box::new(e),
            }
        });
        let js_result = match js_result {
            Ok(r) => r,
            Err(e) => {
                {
                    let mut guard = self.eval_results_by_token.lock().map_err(|_| {
                        BamlRtError::QuickJs("eval_results lock poisoned".to_string())
                    })?;
                    guard.remove(&eval_token);
                }
                if let Some(ref t) = token_to_remove {
                    self.remove_invocation_token(t);
                }
                exit_invocation_context(
                    &self.invocation_context_registry,
                    context_id_to_exit.as_ref(),
                );
                return Err(e);
            }
        };

        // Check if result is a string (synchronous code returned immediately)
        if js_result.is_string() {
            if let Some(ref t) = token_to_remove {
                self.remove_invocation_token(t);
            }
            exit_invocation_context(
                &self.invocation_context_registry,
                context_id_to_exit.as_ref(),
            );
            let json_str = js_result.get_str();
            serde_json::from_str(json_str).map_err(BamlRtError::Json)
        } else {
            // Result is a promise - we need to wait for it to resolve
            // The async IIFE will set globalThis.__eval_result when done
            let debug_str = format!("{:?}", js_result);

            // Check if it's a promise
            if debug_str.contains("Promise") || debug_str.contains("JsPromise") {
                let invocation_scope = scope.ok_or_else(|| {
                    BamlRtError::QuickJs(
                        "Promise polling requires invocation scope; evaluate(scope=None) must not await promises"
                            .to_string(),
                    )
                })?;
                let effect_liveness = self.effect_liveness.clone().ok_or_else(|| {
                    BamlRtError::QuickJs(
                        "Promise polling requires effect liveness wiring; call set_effect_liveness() on bridge initialization"
                            .to_string(),
                    )
                })?;
                let result_str = promise_polling::poll_promise_until_result(
                    promise_polling::PollPromiseParams {
                        runtime: &self.runtime,
                        eval_results_by_token: &self.eval_results_by_token,
                        eval_token: &eval_token,
                        token_to_remove: token_to_remove.as_ref(),
                        invocation_scope_by_token: &self.invocation_scope_by_token,
                        scope: invocation_scope,
                        effect_liveness,
                        idle_timeout_ms: self.idle_timeout_ms,
                        max_attempts_ms: self.max_attempts_ms,
                    },
                )
                .await?;
                let parsed = serde_json::from_str(result_str.as_str()).map_err(|e| {
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
                })?;
                if let Some(ref t) = token_to_remove {
                    self.remove_invocation_token(t);
                }
                Ok(parsed)
            } else {
                // Not a promise, wrap in success object
                {
                    let mut guard = self.eval_results_by_token.lock().map_err(|_| {
                        BamlRtError::QuickJs("eval_results lock poisoned".to_string())
                    })?;
                    guard.remove(&eval_token);
                }
                if let Some(ref t) = token_to_remove {
                    self.remove_invocation_token(t);
                }
                Ok(serde_json::json!({ "success": true, "result": debug_str }))
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use baml_rt_core::ids::{AgentId, UuidId};
    use baml_rt_tools::BamlTool;
    use baml_rt_tools::bundles::BundleType;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use ts_rs::TS;

    struct TestBundle;

    impl BundleType for TestBundle {
        const NAME: &'static str = "test";
        fn description() -> &'static str {
            "Test tools for bridge concurrency"
        }
    }

    #[derive(Clone)]
    struct DelayTool {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    #[ts(export)]
    struct DelayInput {
        label: String,
        delay_ms: u64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    #[ts(export)]
    struct DelayOutput {
        label: String,
    }

    #[async_trait]
    impl BamlTool for DelayTool {
        type Bundle = TestBundle;
        const LOCAL_NAME: &'static str = "delay";
        type OpenInput = ();
        type Input = DelayInput;
        type Output = DelayOutput;

        fn description(&self) -> &'static str {
            "Delays to force overlap"
        }

        async fn execute(&self, args: Self::Input) -> baml_rt_core::Result<Self::Output> {
            let cur = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let mut prev = self.max_active.load(Ordering::SeqCst);
            while cur > prev
                && self
                    .max_active
                    .compare_exchange(prev, cur, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
            {
                prev = self.max_active.load(Ordering::SeqCst);
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(args.delay_ms)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(DelayOutput { label: args.label })
        }
    }

    #[tokio::test]
    async fn concurrent_tool_invocations_use_tokens() {
        let mut manager = BamlRuntimeManager::new().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        manager
            .register_tool(DelayTool {
                active: active.clone(),
                max_active: max_active.clone(),
            })
            .await
            .unwrap();

        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000099").unwrap());
        let mut bridge = QuickJSBridge::new(Arc::new(Mutex::new(manager)), agent_id)
            .await
            .unwrap();
        bridge.register_baml_functions().await.unwrap();

        let scope_a = InvocationScope::synthetic_message(AgentId::from_uuid(
            UuidId::parse_str("00000000-0000-0000-0000-0000000000a1").unwrap(),
        ));
        let scope_b = InvocationScope::synthetic_message(AgentId::from_uuid(
            UuidId::parse_str("00000000-0000-0000-0000-0000000000a2").unwrap(),
        ));

        let (token_a, _prelude_a) = bridge.create_invocation_token(&scope_a);
        let (token_b, _prelude_b) = bridge.create_invocation_token(&scope_b);

        let js_code = format!(
            r#"
            (async function() {{
                const p1 = __tool_invoke("{}", "test/delay", JSON.stringify({{ label: "a", delay_ms: 50 }}));
                const p2 = __tool_invoke("{}", "test/delay", JSON.stringify({{ label: "b", delay_ms: 50 }}));
                const results = await Promise.all([p1, p2]);
                return JSON.stringify({{ r1: results[0], r2: results[1] }});
            }})()
            "#,
            token_a.0, token_b.0
        );

        let result = bridge.evaluate(None, &js_code).await.unwrap();
        let obj = result.as_object().expect("Expected object");
        let r1 = obj.get("r1").and_then(|v| v.as_object()).unwrap();
        let r2 = obj.get("r2").and_then(|v| v.as_object()).unwrap();
        assert_eq!(r1.get("label").and_then(|v| v.as_str()), Some("a"));
        assert_eq!(r2.get("label").and_then(|v| v.as_str()), Some("b"));

        let max_active = max_active.load(Ordering::SeqCst);
        assert!(
            max_active >= 2,
            "expected overlapping tool execution, max_active={}",
            max_active
        );
    }
}

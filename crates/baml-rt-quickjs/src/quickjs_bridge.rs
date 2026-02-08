//! QuickJS integration bridge
//!
//! This module maps BAML function calls (executed in Rust) to QuickJS,
//! allowing JavaScript code to invoke BAML functions.

use crate::baml::BamlRuntimeManager;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_core::effects::EffectLiveness;
use crate::js_value_converter::value_to_js_value_facade;
use baml_rt_core::correlation;
use baml_rt_core::context::{self, InvocationScope, RuntimeScope};
use baml_rt_core::ids::ContextId;
use baml_rt_tools::{ToolSessionId, ToolStep};
use quickjs_runtime::builder::QuickJsRuntimeBuilder;
use quickjs_runtime::facades::QuickJsRuntimeFacade;
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde_json::{json, Value};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, Semaphore};

/// Encapsulates effect-gated timeout logic for promise polling.
/// 
/// Determines timeout attempts based on whether effects are in-flight:
/// - Effects active: use max_attempts (configurable, default 30 minutes) to allow I/O to complete
/// - No effects: use idle_timeout_attempts (default 5s) to detect deadlocks
pub struct EffectGatedPoller {
    liveness: Option<Arc<dyn EffectLiveness>>,
    context_id: Option<ContextId>,
    idle_timeout_attempts: u32,
    max_attempts: u32,
}

impl EffectGatedPoller {
    /// Default maximum attempts when effects are in-flight (30 minutes)
    pub const DEFAULT_MAX_ATTEMPTS: u32 = 1_800_000;
    
    pub fn new(
        liveness: Option<Arc<dyn EffectLiveness>>,
        context_id: Option<ContextId>,
        idle_timeout_ms: u64,
        max_attempts_ms: u64,
    ) -> Self {
        Self {
            liveness,
            context_id,
            idle_timeout_attempts: idle_timeout_ms as u32,
            max_attempts: max_attempts_ms as u32,
        }
    }
    
    /// Get the timeout attempts based on current effect state.
    /// 
    /// Returns max_attempts if effects are in-flight, otherwise idle_timeout_attempts.
    pub async fn timeout_attempts(&self) -> u32 {
        match (&self.liveness, &self.context_id) {
            (Some(liveness), Some(ctx_id)) => {
                let counts = liveness.in_flight(ctx_id).await;
                if counts.any() {
                    // Effects active: use long timeout
                    self.max_attempts
                } else {
                    // No effects: use short idle timeout
                    self.idle_timeout_attempts
                }
            }
            _ => {
                // No liveness tracker: use long timeout (backward compat)
                self.max_attempts
            }
        }
    }
}

/// Helper function to serialize an ID to a JSON string for JavaScript prelude code.
fn serialize_id(id: &impl Serialize) -> Result<String> {
    serde_json::to_string(id).map_err(BamlRtError::Json)
}

/// Opaque token issued by the host for the duration of an invocation. JS receives only this
/// string; natives look up scope by token so JS cannot forge attribution. See
/// docs/QUICKJS_THREADING_AND_SCOPE.md.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct InvocationToken(pub(crate) String);

static INVOCATION_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_invocation_token() -> InvocationToken {
    let n = INVOCATION_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    InvocationToken(format!("inv-{}", n))
}

// Worker-thread invocation scope: set by the bridge when running eval via `loop_realm` with
// an `InvocationScope`. Native callbacks run on the QuickJS worker thread and read this
// instead of task-local context (see docs/QUICKJS_THREADING_AND_SCOPE.md).
thread_local! {
    static WORKER_INVOCATION_SCOPE: RefCell<Option<RuntimeScope>> = const { RefCell::new(None) };
}

/// Scope for the current invocation when running on the QuickJS worker thread. Use this in
/// native callbacks (e.g. `__tool_invoke`, `__tool_session_open`) instead of `context::current_scope()`
/// so scope is available without passing it through JavaScript.
/// Resolve invocation scope from native args: if first arg is a non-empty token string, look up
/// in map; else fall back to worker_thread_scope. Returns (scope, skip_count) where skip_count
/// is how many args to skip before the actual payload (1 when token present, 0 for legacy).
fn resolve_scope_from_token_arg(
    map: &Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>,
    args: &[JsValueFacade],
    fallback_worker: bool,
) -> std::result::Result<(RuntimeScope, usize), quickjs_runtime::jsutils::JsError> {
    if !args.is_empty()
        && let Some(token_js) = args.first()
            && token_js.is_string() {
                let s = token_js.get_str().to_string();
                if !s.is_empty() {
                    if let Ok(guard) = map.lock()
                        && let Some(scope) = guard.get(&InvocationToken(s)) {
                            return Ok((scope.clone(), 1));
                        }
                    return Err(quickjs_runtime::jsutils::JsError::new_str(
                        "Invalid or expired invocation token",
                    ));
                }
            }
    if fallback_worker
        && let Some(scope) = worker_thread_scope() {
            return Ok((scope, 0));
        }
    Err(quickjs_runtime::jsutils::JsError::new_str(
        "Missing or invalid invocation token (set globalThis.__baml_invocation_token by running via invoke_js_function/invoke_js_function_stream)",
    ))
}

pub(crate) fn worker_thread_scope() -> Option<RuntimeScope> {
    WORKER_INVOCATION_SCOPE.with(|cell| cell.borrow().clone())
}

/// Clear the worker-thread invocation scope. Call when a stream invocation is done (e.g. in
/// [`get_a2a_yield_buffer`](QuickJSBridge::get_a2a_yield_buffer)) so the next operation doesn't see the old scope.
pub(crate) fn clear_worker_thread_scope() {
    WORKER_INVOCATION_SCOPE.with(|cell| {
        let _ = cell.replace(None);
    });
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
    #[allow(dead_code)] // Required for API; used when constructing scope for eval
    agent_id: baml_rt_core::ids::AgentId,
    effect_liveness: Option<Arc<dyn EffectLiveness>>,
    idle_timeout_ms: u64,
    max_attempts_ms: u64,
    /// Token → scope for native callbacks; host issues token, JS passes it, natives look up scope.
    invocation_scope_by_token: Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>,
    /// Token for the active stream invocation; cleared in get_a2a_yield_buffer so token can be removed.
    current_stream_token: Option<InvocationToken>,
    /// Only one stream invocation may be active at a time so globalThis.__baml_invocation_token is
    /// not overwritten by a concurrent stream. Acquired in invoke_js_function_stream, released in get_a2a_yield_buffer.
    stream_semaphore: Arc<Semaphore>,
    /// Permit held while a stream is active; dropped in get_a2a_yield_buffer.
    stream_permit: Option<tokio::sync::OwnedSemaphorePermit>,
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
        Self::new_with_config(baml_manager, agent_id, crate::runtime::QuickJSConfig::default()).await
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
            max_attempts_ms: config.max_attempts_ms.unwrap_or(EffectGatedPoller::DEFAULT_MAX_ATTEMPTS as u64), // Default 30 minutes
            invocation_scope_by_token: Arc::new(StdMutex::new(HashMap::new())),
            current_stream_token: None,
            stream_semaphore: Arc::new(Semaphore::new(1)),
            stream_permit: None,
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
        use tokio::time::{timeout, Duration};
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

    /// Register all tool functions with QuickJS
    async fn register_tool_functions(&mut self) -> Result<()> {
        tracing::info!("Registering tool functions with QuickJS");

        // Register helper function to execute tools
        self.register_tool_invoke_helper().await?;
        self.register_tool_session_helpers().await?;
        self.register_tool_session_wrapper().await?;

        Ok(())
    }

    /// Register a single tool function with QuickJS
    #[allow(dead_code)]
    async fn register_single_tool(&mut self, tool_name: &str) -> Result<()> {
        let _manager_clone = self.baml_manager.clone();
        let _tool_name_clone = tool_name.to_string();

        // Register a JavaScript wrapper function for the tool
        let js_code = format!(
            r#"
            globalThis.{} = async function(...args) {{
                const argObj = {{}};
                if (args.length === 1 && typeof args[0] === 'object') {{
                    Object.assign(argObj, args[0]);
                }} else {{
                    args.forEach((arg, idx) => {{
                        argObj[`arg${{idx}}`] = arg;
                    }});
                }}
                return await __tool_invoke(globalThis.__baml_invocation_token, "{}", JSON.stringify(argObj));
            }};
            "#,
            tool_name, tool_name
        );

        let script = Script::new("register_tool.js", &js_code);
        self.runtime
            .eval(None, script)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register tool function".to_string(),
                source: Box::new(e),
            })?;

        tracing::debug!(tool = tool_name, "Registered tool function with QuickJS");
        Ok(())
    }

    /// Register helper function for tool invocation
    async fn register_tool_invoke_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();

        // Register __tool_invoke for Rust tools (low-level helper). Accepts (token, tool_name, args) or legacy (tool_name, args).
        self.runtime.set_function(
            &[],
            "__tool_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = resolve_scope_from_token_arg(&scope_map, &args, true)?;
                if args.len() < skip + 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token?, tool_name, args) or (tool_name, args)"));
                }

                let tool_name_js = &args[skip];
                let tool_name = if tool_name_js.is_string() {
                    tool_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Tool name must be a string"));
                };

                let args_js = &args[skip + 1];
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string"));
                };

                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let tool_name_clone = tool_name.clone();
                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        context::with_scope(scope, async move {
                            let manager = manager_for_promise.lock().await;
                            let result = manager.execute_tool(&tool_name_clone, args_json).await;
                            match result {
                                Ok(json_value) => Ok(value_to_js_value_facade(json_value)),
                                Err(e) => {
                                    let error_msg = format!("Tool execution error: {}", e);
                                    tracing::error!(error = ?e, "Tool execution failed");
                                    Err(quickjs_runtime::jsutils::JsError::new_str(&error_msg))
                                }
                            }
                        })
                        .await
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register tool helper function".to_string(),
            source: Box::new(e),
        })?;

        // Register __tool_from_baml_result for executing tools based on BAML union output. Accepts (token?, baml_result).
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();
        self.runtime.set_function(
            &[],
            "__tool_from_baml_result",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = resolve_scope_from_token_arg(&scope_map, &args, true)?;
                if args.len() < skip + 1 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token?, baml_result_json)"));
                }

                let baml_result_js = &args[skip];
                let baml_result_str = if baml_result_js.is_string() {
                    baml_result_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("BAML result must be a JSON string"));
                };

                let baml_result: Value = serde_json::from_str(&baml_result_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse BAML result JSON: {}", e)))?;

                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        context::with_scope(scope, async move {
                            let manager = manager_for_promise.lock().await;
                            let result = manager.execute_tool_from_baml_result(baml_result).await;
                            match result {
                                Ok(json_value) => Ok(value_to_js_value_facade(json_value)),
                                Err(e) => {
                                    let error_msg = format!("Tool execution error: {}", e);
                                    tracing::error!(error = ?e, "Tool execution from BAML result failed");
                                    Err(quickjs_runtime::jsutils::JsError::new_str(&error_msg))
                                }
                            }
                        })
                        .await
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register tool from BAML helper function".to_string(),
            source: Box::new(e),
        })?;

        // Register invokeTool for JS tools only; host tools must use openToolSession.
        let dispatch_code = r#"
            globalThis.invokeTool = async function(toolName, args) {
                const argsObj = typeof args === 'object' && args !== null ? args : { value: args };
                const jsTools = globalThis.__js_tools || {};
                if (typeof jsTools[toolName] === 'function') {
                    return await jsTools[toolName](argsObj);
                }
                throw new Error(`Tool '${toolName}' is a host tool. Use openToolSession().`);
            };
        "#;

        let script = Script::new("register_tool_dispatch.js", dispatch_code);
        self.runtime
            .eval(None, script)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register tool dispatch function".to_string(),
                source: Box::new(e),
            })?;

        tracing::debug!("Registered __tool_invoke, __tool_from_baml_result, and invokeTool helper functions");
        Ok(())
    }

    async fn register_tool_session_helpers(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();

        self.runtime.set_function(
            &[],
            "__tool_session_open",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let args_len = args.len();
                let first_arg_str = args.first().and_then(|a| if a.is_string() { Some(a.get_str().to_string()) } else { None });
                let (scope, skip) = match resolve_scope_from_token_arg(&scope_map, &args, true) {
                    Ok((s, sk)) => {
                        tracing::debug!(
                            __tool_session_open_args = args_len,
                            first_arg = ?first_arg_str,
                            scope_via = if sk == 1 { "token" } else { "worker_thread_scope" },
                            context_id = %s.context_id,
                            "__tool_session_open: resolved scope"
                        );
                        (s, sk)
                    }
                    Err(e) => {
                        tracing::warn!(
                            __tool_session_open_args = args_len,
                            first_arg = ?first_arg_str,
                            error = %e,
                            "__tool_session_open: scope resolution failed"
                        );
                        return Err(e);
                    }
                };
                if args.len() < skip + 1 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token?, tool_name)"));
                }
                let tool_name_js = &args[skip];
                let tool_name = if tool_name_js.is_string() {
                    tool_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Tool name must be a string"));
                };
                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        context::with_scope(scope, async move {
                            let manager = manager_for_promise.lock().await;
                            let open_input = empty_open_input();
                            let session_id = manager.open_tool_session(&tool_name, open_input).await;
                            match session_id {
                                Ok(id) => Ok(JsValueFacade::new_string(id.as_str().into_owned())),
                                Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session open error: {}", e))),
                            }
                        })
                        .await
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_open".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_send",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 2 arguments: session_id and args"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (session id)"));
                };
                let args_json_str = if args[1].is_string() {
                    args[1].get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string"));
                };
                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let manager_for_promise = manager_clone.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let manager = manager_for_promise.lock().await;
                    let result = manager.tool_session_send(&session_id, args_json).await;
                    match result {
                        Ok(_) => Ok(value_to_js_value_facade(Value::Null)),
                        Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session send error: {}", e))),
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_send".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_next",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 1 argument: session_id"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (session id)"));
                };
                let manager_for_promise = manager_clone.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let manager = manager_for_promise.lock().await;
                    let result = manager.tool_session_next(&session_id).await;
                    match result {
                        Ok(step) => {
                            let value = tool_step_to_value(step);
                            Ok(value_to_js_value_facade(value))
                        }
                        Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session next error: {}", e))),
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_next".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_finish",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 1 argument: session_id"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (session id)"));
                };
                let manager_for_promise = manager_clone.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let manager = manager_for_promise.lock().await;
                    let result = manager.tool_session_finish(&session_id).await;
                    match result {
                        Ok(_) => Ok(value_to_js_value_facade(Value::Null)),
                        Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session finish error: {}", e))),
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_finish".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_abort",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 1 argument: session_id"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (session id)"));
                };
                let reason = args.get(1).and_then(|value| {
                    if value.is_string() {
                        Some(value.get_str().to_string())
                    } else {
                        None
                    }
                });
                let manager_for_promise = manager_clone.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let manager = manager_for_promise.lock().await;
                    let result = manager.tool_session_abort(&session_id, reason).await;
                    match result {
                        Ok(_) => Ok(value_to_js_value_facade(Value::Null)),
                        Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session abort error: {}", e))),
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_abort".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered tool session helper functions");
        Ok(())
    }

    async fn register_tool_session_wrapper(&mut self) -> Result<()> {
        let js_code = r#"
        globalThis.openToolSession = async function(toolName) {
            const sessionId = await __tool_session_open(
                globalThis.__baml_invocation_token,
                toolName
            );
            return {
                sessionId,
                send: async function(args) {
                    const argObj = args ?? {};
                    return await __tool_session_send(sessionId, JSON.stringify(argObj));
                },
                continue: async function() {
                    return await __tool_session_next(sessionId);
                },
                finish: async function() {
                    return await __tool_session_finish(sessionId);
                },
                abort: async function(reason) {
                    return await __tool_session_abort(sessionId, reason);
                }
            };
        };
        "#;

        let script = Script::new("register_tool_session_wrapper.js", js_code);
        self.runtime
            .eval(None, script)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register tool session wrapper".to_string(),
                source: Box::new(e),
            })?;

        tracing::debug!("Registered openToolSession wrapper");
        Ok(())
    }

    /// Register a helper function that JavaScript can call to invoke BAML functions. Accepts (token?, function_name, args).
    async fn register_baml_invoke_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();

        self.runtime.set_function(
            &[],
            "__baml_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = resolve_scope_from_token_arg(&scope_map, &args, true)?;
                if args.len() < skip + 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token?, function_name, args)"));
                }

                let func_name_js = &args[skip];
                let func_name = if func_name_js.is_string() {
                    func_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Function name must be a string"));
                };

                let args_js = &args[skip + 1];
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string - use JSON.stringify in JavaScript"));
                };

                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let func_name_clone = func_name.clone();
                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        context::with_scope(scope, async move {
                            let manager = manager_for_promise.lock().await;
                            let value = manager.invoke_function(&func_name_clone, args_json).await
                                .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                            let result = manager.execute_tool_from_baml_result_or_value(value).await
                                .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                            Ok(value_to_js_value_facade(result))
                        })
                        .await
                    })
                    .await
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
        
        tracing::debug!("Registered __awaitAndStringify helper function");
        Ok(())
    }

    /// Set up the A2A stream yield buffer and __baml_a2a_yield so JS can yield chunks asynchronously
    /// instead of collecting and returning an array. Call before invoking handle_a2a_request for stream requests.
    ///
    /// **Liveness:** □(this returns Ok → ◇(get_a2a_yield_buffer is called after one invoke_js_function("handle_a2a_request", ·))).
    /// Use [`a2a_stream::begin_a2a_yield_session`] for a type-safe sequence.
    pub async fn setup_a2a_yield_buffer(&mut self) -> Result<()> {
        let js_code = r#"
            globalThis.__baml_a2a_yield_buffer = [];
            globalThis.__baml_a2a_yield = function(chunk) {
                if (globalThis.__baml_a2a_yield_buffer) globalThis.__baml_a2a_yield_buffer.push(chunk);
            };
        "#;
        let script = Script::new("setup_a2a_yield.js", js_code);
        self.runtime.eval(None, script).await.map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to setup A2A yield buffer".to_string(),
            source: Box::new(e),
        })?;
        Ok(())
    }

    /// Read chunks yielded via __baml_a2a_yield during the last handle_a2a_request call.
    /// Returns the buffer and clears it. Call after invoking handle_a2a_request for stream requests.
    /// Uses evaluate() so we get correct JSON parsing; buffer is cleared in JS before return.
    ///
    /// **Liveness (L3):** □(this is called → ◇(this returns)); returns in finite time.
    ///
    /// Clears the worker-thread invocation scope so the next operation doesn't see the stream's scope.
    pub async fn get_a2a_yield_buffer(&mut self) -> Result<Vec<Value>> {
        // Run pending jobs and yield so this stream's async continuations (e.g. openToolSession)
        // get a chance to run. We do not remove the token here; it is removed when the next
        // stream starts (in invoke_js_function_stream) so late async continuations can still
        // resolve the token.
        const PENDING_JOBS_ITERATIONS: u32 = 50;
        tracing::debug!(iterations = PENDING_JOBS_ITERATIONS, "get_a2a_yield_buffer: running pending jobs");
        for _ in 0..PENDING_JOBS_ITERATIONS {
            self.runtime.exe_rt_task_in_event_loop(|rt| {
                rt.run_pending_jobs_if_any();
            });
            tokio::task::yield_now().await;
        }
        // Release stream semaphore so the next invoke_js_function_stream can proceed.
        self.stream_permit.take();
        self.runtime
            .exe_rt_task_in_event_loop(|_rt| clear_worker_thread_scope());
        let js_code = r#"
            (function() {
                var buf = globalThis.__baml_a2a_yield_buffer || [];
                globalThis.__baml_a2a_yield_buffer = [];
                return JSON.stringify(buf);
            })()
        "#;
        let value = self.evaluate(None, js_code).await.map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to get A2A yield buffer".to_string(),
            source: Box::new(e),
        })?;
        match value {
            Value::Array(arr) => Ok(arr),
            _ => Ok(Vec::new()),
        }
    }

    /// Register a JavaScript tool function
    /// 
    /// JavaScript tools are implemented entirely in JavaScript and run in the QuickJS runtime.
    /// They are NOT available to Rust - they only exist in the JavaScript context.
    /// 
    /// # Arguments
    /// * `name` - The name of the tool (stored under globalThis.__js_tools[name])
    /// * `js_function_code` - JavaScript function code (should be a complete function definition)
    /// 
    /// # Example
    /// ```rust,no_run
    /// # use baml_rt::quickjs_bridge::QuickJSBridge;
    /// # use std::sync::Arc;
    /// # use tokio::sync::Mutex;
    /// # use baml_rt::baml::BamlRuntimeManager;
    /// # use baml_rt_core::ids::{AgentId, UuidId};
    /// # tokio_test::block_on(async {
    /// # let baml_manager = Arc::new(Mutex::new(BamlRuntimeManager::new()?));
    /// # let agent_id = AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000010").unwrap());
    /// # let mut bridge = QuickJSBridge::new(baml_manager.clone(), agent_id).await?;
    /// bridge.register_js_tool("greet_js", r#"
    ///     async function(name) {
    ///         return { greeting: `Hello, ${name}!` };
    ///     }
    /// "#).await?;
    /// # Ok::<(), baml_rt::BamlRtError>(())
    /// # }).unwrap();
    /// ```
    /// 
    /// The tool will be available in JavaScript as:
    /// ```javascript
    /// const result = await invokeTool("interface/tool", { name: "World" });
    /// ```
    pub async fn register_js_tool(
        &mut self,
        name: impl Into<String>,
        js_function_code: impl AsRef<str>,
    ) -> Result<()> {
        let tool_name = name.into();
        let function_code = js_function_code.as_ref();

        if tool_name.split('/').count() != 2 {
            return Err(BamlRtError::InvalidArgument(format!(
                "JavaScript tool name '{}' must be formatted as interface/tool",
                tool_name
            )));
        }

        // Check if tool name conflicts with existing Rust tools
        {
            let manager = self.baml_manager.lock().await;
            let rust_tools = manager.list_tools().await;
            if rust_tools.contains(&tool_name) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool name '{}' conflicts with existing Rust tool",
                    tool_name
                )));
            }
        }

        // Check if already registered as a JS tool
        if self.js_tools.contains(&tool_name) {
            return Err(BamlRtError::InvalidArgument(format!(
                "JavaScript tool '{}' is already registered",
                tool_name
            )));
        }

        // Register the JavaScript function in the QuickJS runtime
        let js_code = format!(
            r#"
            globalThis.__js_tools = globalThis.__js_tools || {{}};
            globalThis.__js_tools["{}"] = {};
            "#,
            tool_name, function_code
        );

        let script = Script::new("register_js_tool.js", &js_code);
        self.runtime
            .eval(None, script)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: format!("Failed to register JavaScript tool '{}'", tool_name),
                source: Box::new(e),
            })?;

        self.js_tools.insert(tool_name.clone());

        tracing::info!(
            tool = tool_name.as_str(),
            "Registered JavaScript tool function"
        );

        Ok(())
    }

    /// List all registered JavaScript tools
    pub fn list_js_tools(&self) -> Vec<String> {
        self.js_tools.iter().cloned().collect()
    }

    /// Check if a tool name is a JavaScript tool (not a Rust tool)
    pub fn is_js_tool(&self, name: &str) -> bool {
        self.js_tools.contains(name)
    }

    /// Register a helper function for streaming BAML function execution. Accepts (token?, function_name, args).
    async fn register_baml_stream_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();

        self.runtime.set_function(
            &[],
            "__baml_stream",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = resolve_scope_from_token_arg(&scope_map, &args, true)?;
                if args.len() < skip + 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token?, function_name, args)"));
                }

                let func_name_js = &args[skip];
                let func_name = if func_name_js.is_string() {
                    func_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Function name must be a string"));
                };

                let args_js = &args[skip + 1];
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
                let correlation_id = correlation::current_or_new();
                let manager_for_stream = manager_clone.clone();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        use tokio::sync::mpsc;
                        let (tx, mut rx) = mpsc::channel::<serde_json::Value>(100);
                        
                        let func_name_stream = func_name_clone.clone();
                        let args_json_stream = args_json.clone();
                        let spawn_correlation_id = correlation::current_or_new();
                        let spawn_scope = scope.clone();
                        
                        // Spawn a task to run the stream and send incremental results
                        tokio::spawn(async move {
                            correlation::with_correlation_id(spawn_correlation_id, async move {
                                context::with_scope(spawn_scope, async move {
                                if args_json_stream
                                    .get("__scope_probe")
                                    .and_then(Value::as_bool)
                                    == Some(true)
                                {
                                    let payload = json!({
                                        "context_id": context::current_context_id()
                                            .map(|id| id.to_string()),
                                        "message_id": context::current_message_id()
                                            .map(|id| id.to_string()),
                                        "task_id": context::current_task_id()
                                            .map(|id| id.to_string()),
                                    });
                                    if let Err(e) = tx.send(payload).await {
                                        tracing::warn!(error = ?e, "Failed to send scope probe payload");
                                    }
                                    return;
                                }

                                // Create the stream
                                let manager = manager_for_stream.lock().await;
                                let stream_result = manager.invoke_function_stream(&func_name_stream, args_json_stream);
                                
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
                                let ctx_manager = match executor_ref
                                    .create_ctx_manager_for_current_scope()
                                {
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
                                
                                // We need to keep the manager lock during stream execution
                                // because ctx_manager is a reference. For now, we'll collect all results
                                // in the callback and then drop the lock.
                                let env_vars = HashMap::new();
                                let (final_result, _call_id) = {
                                    stream.run(
                                        None::<fn()>, // on_tick
                                        Some(|result: baml_runtime::FunctionResult| {
                                            // Extract incremental result and send it
                                            // parsed() returns Option<Result<ResponseBamlValue, Error>>
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
                                    ).await
                                };
                                drop(manager); // Release lock after stream completes

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
                                }).await;
                            })
                            .await;
                        });

                        // Collect results from the channel into an array
                        let mut results = Vec::new();
                        while let Some(value) = rx.recv().await {
                            results.push(value);
                        }

                        // Convert results array to JsValueFacade directly
                        Ok(value_to_js_value_facade(serde_json::Value::Array(results)))
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register streaming helper function".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered __baml_stream helper function");
        Ok(())
    }

    /// Register a single BAML function with QuickJS
    async fn register_single_function(&mut self, function_name: &str) -> Result<()> {
        // Register a JavaScript wrapper function that calls the Rust helper
        // Use JSON.stringify to convert arguments to JSON
        // Note: For now, we're using a synchronous approach, but the JS function is async
        // to match the expected interface
        let js_code = format!(
            r#"
            globalThis.{} = async function(...args) {{
                // Convert arguments to a JSON object
                const argObj = {{}};
                // For now, handle simple cases - can be enhanced later
                if (args.length === 1 && typeof args[0] === 'object') {{
                    Object.assign(argObj, args[0]);
                }} else {{
                    // Try to map positional args to object properties
                    // This is a simplified mapping - could be improved with function signatures
                    args.forEach((arg, idx) => {{
                        argObj[`arg${{idx}}`] = arg;
                    }});
                }}
                
                // Call the Rust helper function - JSON.stringify once here is efficient
                // The helper returns a promise that will resolve asynchronously
                return await __baml_invoke(globalThis.__baml_invocation_token, "{}", JSON.stringify(argObj));
            }};
            "#,
            function_name, function_name
        );

        let script = Script::new("register_function.js", &js_code);
        let _result = self.runtime
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
        let js_code = format!(
            r#"
            globalThis.{} = async function(...args) {{
                // Convert arguments to a JSON object
                const argObj = {{}};
                if (args.length === 1 && typeof args[0] === 'object') {{
                    Object.assign(argObj, args[0]);
                }} else {{
                    args.forEach((arg, idx) => {{
                        argObj[`arg${{idx}}`] = arg;
                    }});
                }}
                
                // Call the Rust streaming helper function - JSON.stringify once here
                // This returns an array of incremental results
                const results = await __baml_stream(globalThis.__baml_invocation_token, "{}", JSON.stringify(argObj));
                
                // Return the array directly - JavaScript can iterate over it
                return results;
            }};
            "#,
            stream_function_name, function_name
        );

        let script = Script::new("register_stream_function.js", &js_code);
        let _result = self.runtime
            .eval(None, script)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register stream function".to_string(),
                source: Box::new(e),
            })?;
        
        tracing::debug!(function = function_name, stream_function = stream_function_name, "Registered streaming function with QuickJS");
        
        Ok(())
    }

    /// Create an opaque invocation token and prelude string. Register scope under the token;
    /// call [`remove_invocation_token`](QuickJSBridge::remove_invocation_token) when the invocation ends.
    fn create_invocation_token(&mut self, scope: &InvocationScope) -> (InvocationToken, String) {
        let token = next_invocation_token();
        let prelude = format!(
            "globalThis.__baml_invocation_token = \"{}\";",
            token.0.replace('\\', "\\\\").replace('"', "\\\"")
        );
        if let Ok(mut map) = self.invocation_scope_by_token.lock() {
            map.insert(token.clone(), scope.as_scope().clone());
        }
        (token, prelude)
    }

    /// Remove an invocation token so it can no longer be used for scope lookup. Call when the
    /// invocation completes (after evaluate returns for non-stream, or in get_a2a_yield_buffer for stream).
    fn remove_invocation_token(&mut self, token: &InvocationToken) {
        if let Ok(mut map) = self.invocation_scope_by_token.lock() {
            map.remove(token);
        }
    }

    /// Run a single script on the QuickJS worker thread with the given invocation scope set
    /// in the worker-thread thread-local. Native callbacks (e.g. __tool_invoke) run on that
    /// thread and read scope via [`worker_thread_scope()`]; no JS-passed context needed.
    /// See docs/QUICKJS_THREADING_AND_SCOPE.md.
    ///
    /// When `clear_after` is true (e.g. non-stream invoke), the scope is restored when the eval
    /// returns. When false (stream invoke), the scope is left set so that async promise
    /// continuations (e.g. openToolSession) still see it; clear via [`clear_worker_thread_scope`].
    pub async fn run_eval_with_scope(
        &self,
        scope: &InvocationScope,
        script: Script,
        clear_after: bool,
    ) -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
        let scope_runtime = scope.as_scope().clone();
        self.runtime
            .loop_realm(None, move |_rt, realm| {
                WORKER_INVOCATION_SCOPE.with(|cell| {
                    let prev = cell.replace(Some(scope_runtime));
                    let res = realm.eval(script);
                    let out = match res {
                        Ok(jsvr) => realm.to_js_value_facade(&jsvr),
                        Err(e) => Err(e),
                    };
                    if clear_after {
                        cell.replace(prev);
                    }
                    out
                })
            })
            .await
    }

    /// Execute JavaScript code in the QuickJS context
    /// 
    /// The code should return a JSON string or a promise that resolves to a JSON string.
    /// If code returns a promise, we wait for it to resolve.
    ///
    /// When `scope` is `Some`, every eval runs via [`run_eval_with_scope`] so native callbacks
    /// see the scope via [`worker_thread_scope()`] (no JS-passed context).
    pub async fn evaluate(&mut self, scope: Option<&InvocationScope>, code: &str) -> Result<Value> {
        tracing::trace!(code = code, "Executing JavaScript code");
        
        // First, try executing the code directly (for synchronous code like assignments)
        // This handles agent initialization code that just assigns to globalThis
        // If code already has a return statement (like in an IIFE), execute as-is
        // Otherwise, wrap it in an IIFE
        let code_trimmed = code.trim();
        let is_arrow_iife = code_trimmed.starts_with("(()") || code_trimmed.starts_with("(async ()");
        let already_wrapped = code_trimmed.starts_with("(function()")
            || code_trimmed.starts_with("(async function()")
            || is_arrow_iife;
        
        let direct_code = if already_wrapped {
            // Code is already wrapped in an IIFE - execute directly
            code.to_string()
        } else {
            // Code needs wrapping - wrap in IIFE (preserves side effects for assignments)
            format!("(function() {{ {} }})()", code)
        };
        let direct_script = Script::new("eval_direct.js", &direct_code);
        let direct_result = match scope {
            Some(s) => self.run_eval_with_scope(s, direct_script.clone(), true).await,
            None => self.runtime.eval(None, direct_script).await,
        };
        if let Err(e) = direct_result {
            let message = e.to_string();
            return Err(BamlRtError::QuickJsWithSource {
                context: format!("Failed to execute JavaScript: {}", message),
                source: Box::new(e),
            });
        }
        
        // If direct execution succeeds and returns a non-promise, we're done
        let js_result = direct_result.expect("direct_result validated as Ok");
        if js_result.is_string() {
            // Got a string result - try parsing as JSON
            let json_str = js_result.get_str();
            if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
                return Ok(parsed);
            }
            // Not JSON - return the string wrapped in a result object
            return Ok(serde_json::json!({ "result": json_str }));
        }
        // Not a string - might be undefined/null from assignment code
        // Check if it's a promise
        let debug_str = format!("{:?}", js_result);
        if !debug_str.contains("Promise") && !debug_str.contains("JsPromise") {
            // Not a promise, code executed successfully (side effects happened)
            // Return empty object to indicate success without a value
            return Ok(serde_json::json!({}));
        }
        
        // Code returned a promise - need to await it and store result
        // The code is already wrapped in (function() { ... })(), so execute it directly
        // It returns a promise (from __awaitAndStringify), so we await it
        let wrapped_code = format!(
            r#"
            (async function() {{
                try {{
                    // Execute the code (it's already an IIFE) which returns a promise
                    const codePromise = {};
                    const result = await codePromise;
                    // result is the JSON string from __awaitAndStringify
                    globalThis.__eval_result = typeof result === 'string' ? result : JSON.stringify(result);
                }} catch (error) {{
                    globalThis.__eval_result = JSON.stringify({{ error: error.toString() }});
                }}
            }})()
            "#,
            code
        );
        
        let script = Script::new("eval.js", &wrapped_code);
        
        // Execute the code - this will set __eval_result when the promise resolves
        let js_result = match scope {
            Some(s) => self.run_eval_with_scope(s, script, true).await,
            None => self.runtime.eval(None, script).await,
        }
        .map_err(|e| {
                let message = e.to_string();
                BamlRtError::QuickJsWithSource {
                    context: format!("Failed to execute JavaScript: {}", message),
                    source: Box::new(e),
                }
            })?;

        // Check if result is a string (synchronous code returned immediately)
        if js_result.is_string() {
            let json_str = js_result.get_str();
            serde_json::from_str(json_str)
                .map_err(BamlRtError::Json)
        } else {
            // Result is a promise - we need to wait for it to resolve
            // The async IIFE will set globalThis.__eval_result when done
            let debug_str = format!("{:?}", js_result);
            
            // Check if it's a promise
            if debug_str.contains("Promise") || debug_str.contains("JsPromise") {
                // Liveness (L4-L6, effect-gated): Effect-gated timeout distinguishes "waiting on effect"
                // (progress possible) from "will never yield" (deadlock/infinite sync).
                // See docs/HOST_QUICKJS_STREAM_INVARIANTS.md.
                // Wait for the promise to resolve by running pending jobs in a loop
                // and checking if __eval_result has been set.
                // CG6: Timeout is monotonic - we never decrease it mid-loop to avoid premature timeout.
                let poll_span = tracing::trace_span!("baml_rt.poll_promise_resolution");
                let _poll_guard = poll_span.enter();
                let mut attempts = 0u32;
                let context_id = scope
                    .map(|s| s.context_id.clone())
                    .or_else(context::current_context_id);
                let poller = EffectGatedPoller::new(
                    self.effect_liveness.clone(),
                    context_id,
                    self.idle_timeout_ms,
                    self.max_attempts_ms,
                );
                let mut timeout_attempts = poller.timeout_attempts().await;
                const EFFECT_CHECK_INTERVAL: u32 = 100;

                loop {
                    // CG5: Yield within bounded steps so other tasks can progress
                    self.runtime.exe_rt_task_in_event_loop(|rt| {
                        rt.run_pending_jobs_if_any();
                    });
                    tokio::task::yield_now().await;

                    let check_code = r#"
                        (function() {
                            if (typeof globalThis.__eval_result !== 'undefined') {
                                return globalThis.__eval_result;
                            }
                            return null;
                        })()
                    "#;
                    let check_script = Script::new("check_result.js", check_code);
                    let check_result = match scope {
                        Some(s) => self.run_eval_with_scope(s, check_script, true).await,
                        None => self.runtime.eval(None, check_script).await,
                    }
                    .map_err(|e| BamlRtError::QuickJsWithSource {
                        context: "Failed to check result".to_string(),
                        source: Box::new(e),
                    })?;

                    if check_result.is_string() {
                        let result_str = check_result.get_str();
                        let cleanup_script = Script::new("cleanup.js", "delete globalThis.__eval_result");
                        if let Err(e) = match scope {
                            Some(s) => self.run_eval_with_scope(s, cleanup_script, true).await,
                            None => self.runtime.eval(None, cleanup_script).await,
                        } {
                            tracing::warn!(error = ?e, "Failed to clean up eval result");
                        }
                        tracing::trace!(attempts = attempts, "Promise resolved");
                        return serde_json::from_str(result_str).map_err(BamlRtError::Json);
                    }

                    // CG6: Re-check effects periodically; only increase timeout, never decrease
                    if attempts > 0 && attempts.is_multiple_of(EFFECT_CHECK_INTERVAL) {
                        let new_timeout = poller.timeout_attempts().await;
                        timeout_attempts = timeout_attempts.max(new_timeout);
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    attempts += 1;
                    if attempts >= timeout_attempts {
                        let cleanup_script = Script::new("cleanup.js", "delete globalThis.__eval_result");
                        if let Err(e) = match scope {
                            Some(s) => self.run_eval_with_scope(s, cleanup_script, true).await,
                            None => self.runtime.eval(None, cleanup_script).await,
                        } {
                            tracing::warn!(error = ?e, "Failed to clean up eval result");
                        }
                        return Err(BamlRtError::QuickJs(format!(
                            "Promise did not resolve after {} attempts ({}ms)",
                            timeout_attempts,
                            timeout_attempts
                        )));
                    }
                }
            } else {
                // Not a promise, wrap in success object
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
    /// or as an error object if the call failed
    pub async fn invoke_function(&mut self, function_name: &str, args: Value) -> Result<Value> {
        let args_json = serde_json::to_string(&args)
            .map_err(BamlRtError::Json)?;
        let context_prelude = match context::current_context_id() {
            Some(id) => format!(
                "globalThis.__baml_context_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_context_id;".to_string(),
        };
        let message_prelude = match context::current_message_id() {
            Some(id) => format!(
                "globalThis.__baml_message_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_message_id;".to_string(),
        };
        let task_prelude = match context::current_task_id() {
            Some(id) => format!(
                "globalThis.__baml_task_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_task_id;".to_string(),
        };
        let scope_prelude = format!("{context_prelude}\n{message_prelude}\n{task_prelude}");
        
        // Generate JavaScript code that invokes the BAML runtime only (no JS fallback)
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    {}
                    const args = {};
                    const promise = __baml_invoke(globalThis.__baml_invocation_token, "{}", JSON.stringify(args));
                    return __awaitAndStringify(promise);
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            scope_prelude, args_json, function_name
        );

        if correlation::current_correlation_id().is_some() {
            self.evaluate(None, &js_code).await
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(None, &js_code).await
            })
            .await
        }
    }

    /// Invoke a JavaScript tool by name.
    ///
    /// This only executes a JavaScript function from globalThis and does not fall back to BAML.
    pub async fn invoke_js_tool(&mut self, tool_name: &str, args: Value) -> Result<Value> {
        let args_json = serde_json::to_string(&args)
            .map_err(BamlRtError::Json)?;
        let context_prelude = match context::current_context_id() {
            Some(id) => format!(
                "globalThis.__baml_context_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_context_id;".to_string(),
        };
        let message_prelude = match context::current_message_id() {
            Some(id) => format!(
                "globalThis.__baml_message_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_message_id;".to_string(),
        };
        let task_prelude = match context::current_task_id() {
            Some(id) => format!(
                "globalThis.__baml_task_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_task_id;".to_string(),
        };
        let scope_prelude = format!("{context_prelude}\n{message_prelude}\n{task_prelude}");

        let js_code = format!(
            r#"
            (function() {{
                try {{
                    {}
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
            scope_prelude, args_json, tool_name
        );

        if correlation::current_correlation_id().is_some() {
            self.evaluate(None, &js_code).await
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(None, &js_code).await
            })
            .await
        }
    }

    /// Invoke a JavaScript function and wait for its promise to resolve.
    ///
    /// **Scope:** Caller must pass the invocation scope; the entire JS run executes inside
    /// `with_scope(scope, ...)` so native callbacks see the correct task-local scope.
    ///
    /// **INVARIANT:** For non-stream functions, the promise MUST resolve within bounded time.
    /// For stream functions, use `invoke_js_function_stream()` instead.
    pub async fn invoke_js_function(&mut self, scope: &InvocationScope, function_name: &str, args: Value) -> Result<Value> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let (token, token_prelude) = self.create_invocation_token(scope);
        let context_prelude = format!(
            "globalThis.__baml_context_id = {};",
            serialize_id(&scope.context_id)?
        );
        let message_prelude = match scope.message_id.as_ref() {
            Some(id) => format!("globalThis.__baml_message_id = {};", serialize_id(id)?),
            None => "delete globalThis.__baml_message_id;".to_string(),
        };
        let task_prelude = match scope.task_id.as_ref() {
            Some(id) => format!("globalThis.__baml_task_id = {};", serialize_id(id)?),
            None => "delete globalThis.__baml_task_id;".to_string(),
        };
        let scope_prelude = format!("{token_prelude}\n{context_prelude}\n{message_prelude}\n{task_prelude}");

        let js_code = format!(
            r#"
            (function() {{
                try {{
                    {}
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
            scope_prelude, args_json, function_name, function_name
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
        self.remove_invocation_token(&token);

        match &result {
            Value::Object(map) if map.get("error").is_some() => Err(BamlRtError::QuickJs(format!(
                "JS function invocation error ({}): {}",
                function_name,
                map.get("error").and_then(Value::as_str).unwrap_or("unknown")
            ))),
            _ => Ok(result),
        }
    }

    /// Invoke a JavaScript function for streaming (yield-based) requests.
    ///
    /// **Scope:** Caller must pass the invocation scope; the entire JS run executes inside
    /// `with_scope(scope, ...)` so native callbacks see the correct task-local scope.
    ///
    /// **INVARIANT L6 (Stream Promise Non-Termination):**
    /// For stream requests, the promise from `handle_a2a_request()` is DESIGNED to never resolve.
    /// It yields chunks via `__baml_a2a_yield()` and only completes on agent exit or crash.
    /// This method starts the async function but does NOT wait for promise resolution.
    ///
    /// **Property:**
    /// ```
    /// ∀ stream request s:
    ///   invoke_js_function_stream(s) starts async execution AND returns immediately
    ///   The promise from handle_a2a_request() never resolves (by design)
    ///   Chunks are collected via get_a2a_yield_buffer() after invocation
    /// ```
    pub async fn invoke_js_function_stream(&mut self, scope: &InvocationScope, function_name: &str, args: Value) -> Result<()> {
        // Only one stream active at a time so globalThis.__baml_invocation_token is not overwritten by a concurrent stream.
        let permit = self
            .stream_semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BamlRtError::QuickJs("stream semaphore closed".to_string()))?;
        self.stream_permit = Some(permit);

        // Remove previous stream's token so it cannot be reused; leave current token for late continuations until next stream.
        if let Some(prev) = self.current_stream_token.take() {
            self.remove_invocation_token(&prev);
            tracing::debug!(token = %prev.0, "invoke_js_function_stream: removed previous stream token");
        }

        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let (token, token_prelude) = self.create_invocation_token(scope);
        self.current_stream_token = Some(token.clone());
        tracing::debug!(
            token = %token.0,
            context_id = %scope.context_id,
            function_name = function_name,
            "invoke_js_function_stream: created token and prelude"
        );
        let context_prelude = format!(
            "globalThis.__baml_context_id = {};",
            serialize_id(&scope.context_id)?
        );
        let message_prelude = match scope.message_id.as_ref() {
            Some(id) => format!("globalThis.__baml_message_id = {};", serialize_id(id)?),
            None => "delete globalThis.__baml_message_id;".to_string(),
        };
        let task_prelude = match scope.task_id.as_ref() {
            Some(id) => format!("globalThis.__baml_task_id = {};", serialize_id(id)?),
            None => "delete globalThis.__baml_task_id;".to_string(),
        };
        let scope_prelude = format!("{token_prelude}\n{context_prelude}\n{message_prelude}\n{task_prelude}");

        // For stream requests, we start the async function but DON'T wait for promise resolution.
        // The function yields chunks via __baml_a2a_yield() and the promise never resolves (by design).
        // We just need to ensure the function starts executing and can yield chunks.
        let js_code = format!(
            r#"
            (function() {{
                try {{
                    {}
                    const args = {};
                    const func = globalThis["{}"];
                    if (func === undefined || typeof func !== 'function') {{
                        throw new Error("JS function not found: {}");
                    }}
                    // Start the async function but don't await it - it's designed to never resolve
                    // for stream requests. Chunks are collected via __baml_a2a_yield_buffer.
                    func(args);
                    return JSON.stringify({{ success: true }});
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            scope_prelude, args_json, function_name, function_name
        );

        // Execute with worker-thread scope so native callbacks see scope (no JS-passed context).
        // Leave scope set (clear_after: false) so async promise continuations (e.g. openToolSession) see it.
        let script = Script::new("invoke_stream.js", &js_code);
        let js_result = self
            .run_eval_with_scope(scope, script, false)
            .await
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: format!("Failed to invoke stream function {}", function_name),
                source: Box::new(e),
            })?;

        // Check for immediate errors (synchronous errors, function not found, etc.)
        if js_result.is_string() {
            let json_str = js_result.get_str();
            if let Ok(value) = serde_json::from_str::<Value>(json_str)
                && let Some(error) = value.get("error").and_then(Value::as_str) {
                    return Err(BamlRtError::QuickJs(format!(
                        "JS stream function invocation error ({}): {}",
                        function_name, error
                    )));
                }
        }

        // Run pending jobs to allow the async function to start executing
        // This ensures the function begins running and can yield chunks
        self.runtime.exe_rt_task_in_event_loop(|rt| {
            rt.run_pending_jobs_if_any();
        });

        // Yield to tokio to allow the async function to progress
        tokio::task::yield_now().await;

        Ok(())
    }

    pub async fn invoke_optional_js_function(
        &mut self,
        function_name: &str,
        args: Value,
    ) -> Result<Option<Value>> {
        let args_json = serde_json::to_string(&args).map_err(BamlRtError::Json)?;
        let context_prelude = match context::current_context_id() {
            Some(id) => format!(
                "globalThis.__baml_context_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_context_id;".to_string(),
        };
        let message_prelude = match context::current_message_id() {
            Some(id) => format!(
                "globalThis.__baml_message_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_message_id;".to_string(),
        };
        let task_prelude = match context::current_task_id() {
            Some(id) => format!(
                "globalThis.__baml_task_id = {};",
                serialize_id(&id)?
            ),
            None => "delete globalThis.__baml_task_id;".to_string(),
        };
        let scope_prelude = format!("{context_prelude}\n{message_prelude}\n{task_prelude}");

        let js_code = format!(
            r#"
            (function() {{
                try {{
                    {}
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
            scope_prelude, args_json, function_name
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
            if map.get("__absent").and_then(Value::as_bool).unwrap_or(false) {
                return Ok(None);
            }
            if let Some(error) = map.get("error").and_then(Value::as_str) {
                return Err(BamlRtError::QuickJs(format!(
                    "JS function invocation error ({}): {}",
                    function_name,
                    error
                )));
            }
        }

        Ok(Some(result))
    }

    /// Invoke a streaming JavaScript or BAML function by name.
    ///
    /// This prefers a JavaScript function named `<function_name>Stream` if present,
    /// then falls back to BAML streaming via __baml_stream.
    pub async fn invoke_function_stream(&mut self, function_name: &str, args: Value) -> Result<Vec<Value>> {
        let args_json = serde_json::to_string(&args)
            .map_err(BamlRtError::Json)?;
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
                        promise = __baml_stream(globalThis.__baml_invocation_token, "{}", JSON.stringify(args));
                    }}
                    return __awaitAndStringify(promise);
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            args_json,
            stream_function,
            function_name
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
        match result {
            Value::Array(values) => Ok(values),
            Value::Object(map) if map.get("error").is_some() => Err(BamlRtError::QuickJs(format!(
                "A2A stream invocation error: {}",
                map.get("error").and_then(|v| v.as_str()).unwrap_or("unknown")
            ))),
            other => Ok(vec![other]),
        }
    }

}

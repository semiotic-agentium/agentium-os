//! QuickJS integration bridge
//!
//! This module maps BAML function calls (executed in Rust) to QuickJS,
//! allowing JavaScript code to invoke BAML functions.

use crate::baml::BamlRuntimeManager;
use baml_rt_core::{BamlRtError, Result};
use crate::js_value_converter::value_to_js_value_facade;
use baml_rt_core::correlation;
use baml_rt_core::context;
use baml_rt_core::ids::{ContextId, ExternalId, MessageId, TaskId};
use baml_rt_tools::{ToolSessionId, ToolStep};
use quickjs_runtime::builder::QuickJsRuntimeBuilder;
use quickjs_runtime::facades::QuickJsRuntimeFacade;
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde_json::{json, Value};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Helper function to serialize an ID to a JSON string for JavaScript prelude code.
fn serialize_id(id: &impl Serialize) -> Result<String> {
    serde_json::to_string(id).map_err(BamlRtError::Json)
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
    agent_id: baml_rt_core::ids::AgentId, // REQUIRED - agent_id is never optional
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
        };

        // Initialize sandbox - remove dangerous globals and implement safe console
        bridge.initialize_sandbox().await?;

        Ok(bridge)
    }

    /// Initialize the sandbox environment
    /// 
    /// This removes dangerous globals and modules, and implements a safe console API.
    /// Only console.log is available - no filesystem, network, or other I/O access.
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
        self.runtime
            .eval(None, script)
            .await
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
                return await __tool_invoke("{}", JSON.stringify(argObj), globalThis.__baml_context_id, globalThis.__baml_message_id, globalThis.__baml_task_id);
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
        let agent_id = self.agent_id.clone(); // REQUIRED - capture agent_id from bridge

        // Register __tool_invoke for Rust tools (low-level helper)
        self.runtime.set_function(
            &[],
            "__tool_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 2 arguments: tool_name and args"));
                }

                let tool_name_js = &args[0];
                let tool_name = if tool_name_js.is_string() {
                    tool_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (tool name)"));
                };

                let args_js = &args[1];
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string"));
                };

                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let context_id_arg = args.get(2).and_then(|value| {
                    if value.is_string() {
                        ContextId::parse_temporal(value.get_str())
                    } else {
                        None
                    }
                });
                let message_id_arg = args.get(3).and_then(|value| {
                    if value.is_string() {
                        Some(MessageId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                let task_id_arg = args.get(4).and_then(|value| {
                    if value.is_string() {
                        Some(TaskId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });

                let tool_name_clone = tool_name.clone();
                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();
                let context_id = context_id_arg.unwrap_or_else(context::current_or_new);
                // agent_id is REQUIRED and captured from bridge - never optional
                let scope = context::RuntimeScope::new(context_id, agent_id.clone(), message_id_arg, task_id_arg);

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        context::with_scope(scope, async move {
                        let manager = manager_for_promise.lock().await;
                        let result = manager.execute_tool(&tool_name_clone, args_json).await;

                        match result {
                            Ok(json_value) => {
                                Ok(value_to_js_value_facade(json_value))
                            }
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

        // Register __tool_from_baml_result for executing tools based on BAML union output.
        let manager_clone = self.baml_manager.clone();
        let agent_id = self.agent_id.clone(); // REQUIRED - capture agent_id from bridge
        self.runtime.set_function(
            &[],
            "__tool_from_baml_result",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 1 argument: baml_result_json"));
                }

                let baml_result_js = &args[0];
                let baml_result_str = if baml_result_js.is_string() {
                    baml_result_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("BAML result must be a JSON string"));
                };

                let baml_result: Value = serde_json::from_str(&baml_result_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse BAML result JSON: {}", e)))?;

                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();
                let context_id = args
                    .get(1)
                    .and_then(|value| {
                        if value.is_string() {
                            ContextId::parse_temporal(value.get_str())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(context::current_or_new);
                let message_id = args.get(2).and_then(|value| {
                    if value.is_string() {
                        Some(MessageId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                let task_id = args.get(3).and_then(|value| {
                    if value.is_string() {
                        Some(TaskId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                // agent_id is REQUIRED and captured from bridge - never optional
                let scope = context::RuntimeScope::new(context_id, agent_id.clone(), message_id, task_id);

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
        let agent_id = self.agent_id.clone();

        self.runtime.set_function(
            &[],
            "__tool_session_open",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 1 argument: tool_name"));
                }

                let tool_name_js = &args[0];
                let tool_name = if tool_name_js.is_string() {
                    tool_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (tool name)"));
                };

                let context_id_arg = args.get(1).and_then(|value| {
                    if value.is_string() {
                        ContextId::parse_temporal(value.get_str())
                    } else {
                        None
                    }
                });
                let message_id_arg = args.get(2).and_then(|value| {
                    if value.is_string() {
                        Some(MessageId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                let task_id_arg = args.get(3).and_then(|value| {
                    if value.is_string() {
                        Some(TaskId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });

                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();
                let context_id = context_id_arg.unwrap_or_else(context::current_or_new);
                let scope = context::RuntimeScope::new(context_id, agent_id.clone(), message_id_arg, task_id_arg);

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        context::with_scope(scope, async move {
                            let manager = manager_for_promise.lock().await;
                            // Default to empty object for open_input if not provided
                            let open_input = serde_json::Value::Object(serde_json::Map::new());
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
                toolName,
                globalThis.__baml_context_id,
                globalThis.__baml_message_id,
                globalThis.__baml_task_id
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

    /// Register a helper function that JavaScript can call to invoke BAML functions
    async fn register_baml_invoke_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let agent_id = self.agent_id.clone(); // REQUIRED - capture agent_id from bridge
        
        // Register a native Rust function that JavaScript can call
        // This function will handle the async BAML execution using promises
        self.runtime.set_function(
            &[],
            "__baml_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 2 arguments: function_name and args"));
                }

                // Extract function name (first arg should be a string)
                let func_name_js = &args[0];
                let func_name = if func_name_js.is_string() {
                    func_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (function name)"));
                };

                // Extract args (second arg) - for complex objects, we still use JSON.stringify
                // but we can optimize this in the future
                let args_js = &args[1];
                // For now, convert to string and parse back - we can optimize this later
                // The issue is that JsValueFacade doesn't expose direct access to object properties
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    // For non-strings, try to convert via debug format (fallback)
                    // In practice, JavaScript should pass JSON.stringify'd values
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string - use JSON.stringify in JavaScript"));
                };

                // Parse JSON string to Value
                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let context_id_arg = args.get(2).and_then(|value| {
                    if value.is_string() {
                        ContextId::parse_temporal(value.get_str())
                    } else {
                        None
                    }
                });
                let message_id_arg = args.get(3).and_then(|value| {
                    if value.is_string() {
                        Some(MessageId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                let task_id_arg = args.get(4).and_then(|value| {
                    if value.is_string() {
                        Some(TaskId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                let context_id = context_id_arg.unwrap_or_else(context::current_or_new);
                // agent_id is REQUIRED and captured from bridge - never optional
                let scope = context::RuntimeScope::new(context_id, agent_id.clone(), message_id_arg, task_id_arg);

                // Create a promise that will execute the BAML call asynchronously
                let func_name_clone = func_name.clone();
                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation::current_or_new();

                // Use JsValueFacade::new_promise to create a non-blocking promise
                // The producer is a Future that will be executed asynchronously
                // Type parameters: R is the result type (JsValueFacade), P is the Future, M is unused/mapper
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    correlation::with_correlation_id(correlation_id, async move {
                        // Execute the BAML function asynchronously
                        let manager = manager_for_promise.lock().await;
                        let result = context::with_scope(scope, async move {
                            let value = manager.invoke_function(&func_name_clone, args_json).await?;
                            manager.execute_tool_from_baml_result_or_value(value).await
                        })
                        .await;

                        match result {
                            Ok(json_value) => {
                                // Convert JSON value to JsValueFacade directly (no stringify needed)
                                Ok(value_to_js_value_facade(json_value))
                            }
                            Err(e) => {
                                let error_msg = format!("BAML execution error: {}", e);
                                tracing::error!(error = ?e, "BAML execution failed");
                                Err(quickjs_runtime::jsutils::JsError::new_str(&error_msg))
                            }
                        }
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

    /// Register a helper function for streaming BAML function execution
    async fn register_baml_stream_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let agent_id = self.agent_id.clone(); // REQUIRED - capture agent_id from bridge
        
        // Register a native Rust function that JavaScript can call for streaming
        self.runtime.set_function(
            &[],
            "__baml_stream",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected 2 arguments: function_name and args"));
                }

                // Extract function name
                let func_name_js = &args[0];
                let func_name = if func_name_js.is_string() {
                    func_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("First argument must be a string (function name)"));
                };

                // Extract args (second arg) - JSON string from JavaScript
                let args_js = &args[1];
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Second argument must be a JSON string"));
                };

                // Parse JSON string to Value
                let args_json: Value = match serde_json::from_str(&args_json_str) {
                    Ok(v) => v,
                    Err(e) => return Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e))),
                };

                let func_name_clone = func_name.clone();
                let correlation_id = correlation::current_or_new();
                let context_id_arg = args.get(2).and_then(|value| {
                    if value.is_string() {
                        ContextId::parse_temporal(value.get_str())
                    } else {
                        None
                    }
                });
                let message_id_arg = args.get(3).and_then(|value| {
                    if value.is_string() {
                        Some(MessageId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                let task_id_arg = args.get(4).and_then(|value| {
                    if value.is_string() {
                        Some(TaskId::from_external(ExternalId::new(value.get_str())))
                    } else {
                        None
                    }
                });
                let context_id = context_id_arg.unwrap_or_else(context::current_or_new);
                // agent_id is REQUIRED and captured from bridge - never optional
                let scope = context::RuntimeScope::new(context_id, agent_id.clone(), message_id_arg, task_id_arg);

                // Create a promise that will execute the streaming BAML call
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
                return await __baml_invoke("{}", JSON.stringify(argObj), globalThis.__baml_context_id, globalThis.__baml_message_id, globalThis.__baml_task_id);
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
                const results = await __baml_stream("{}", JSON.stringify(argObj), globalThis.__baml_context_id, globalThis.__baml_message_id, globalThis.__baml_task_id);
                
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

    /// Execute JavaScript code in the QuickJS context
    /// 
    /// The code should return a JSON string or a promise that resolves to a JSON string.
    /// If code returns a promise, we wait for it to resolve.
    pub async fn evaluate(&mut self, code: &str) -> Result<Value> {
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
        let direct_result = self.runtime.eval(None, direct_script).await;
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
        let js_result = self.runtime
            .eval(None, script)
            .await
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
                // Wait for the promise to resolve by running pending jobs in a loop
                // and checking if __eval_result has been set
                let poll_span = tracing::trace_span!("baml_rt.poll_promise_resolution");
                let _poll_guard = poll_span.enter();
                let mut attempts = 0;
                const MAX_ATTEMPTS: u32 = 60000;
                
                loop {
                    // Check if result is available (trace level - happens many times per resolution)
                    let check_code = r#"
                        (function() {
                            if (typeof globalThis.__eval_result !== 'undefined') {
                                return globalThis.__eval_result;
                            }
                            return null;
                        })()
                    "#;
                    let check_script = Script::new("check_result.js", check_code);
                    let check_result = self.runtime
                        .eval(None, check_script)
                        .await
                        .map_err(|e| BamlRtError::QuickJsWithSource {
                            context: "Failed to check result".to_string(),
                            source: Box::new(e),
                        })?;
                    
                    if check_result.is_string() {
                        let result_str = check_result.get_str();
                        // Clean up the global
                        if let Err(e) = self.runtime.eval(None, Script::new("cleanup.js", "delete globalThis.__eval_result")).await {
                            tracing::warn!(error = ?e, "Failed to clean up eval result");
                        }
                        tracing::trace!(attempts = attempts, "Promise resolved");
                        return serde_json::from_str(result_str).map_err(BamlRtError::Json);
                    }
                    
                    // Run pending jobs - this is how quickjs_runtime processes promises
                    // The runtime automatically polls Rust futures backing promises
                    self.runtime.exe_rt_task_in_event_loop(|rt| {
                        rt.run_pending_jobs_if_any();
                    });
                    
                    // Yield to Tokio to allow futures to progress
                    tokio::task::yield_now().await;
                    
                    // Small delay to allow promise resolution
                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    
                    attempts += 1;
                    if attempts >= MAX_ATTEMPTS {
                        // Clean up the global
                        if let Err(e) = self.runtime.eval(None, Script::new("cleanup.js", "delete globalThis.__eval_result")).await {
                            tracing::warn!(error = ?e, "Failed to clean up eval result");
                        }
                        return Err(BamlRtError::QuickJs(format!(
                            "Promise did not resolve after {} attempts ({}ms)",
                            MAX_ATTEMPTS,
                            MAX_ATTEMPTS
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
                    const promise = __baml_invoke("{}", JSON.stringify(args), globalThis.__baml_context_id, globalThis.__baml_message_id, globalThis.__baml_task_id);
                    return __awaitAndStringify(promise);
                }} catch (error) {{
                    return JSON.stringify({{ error: error.message || String(error) }});
                }}
            }})()
            "#,
            scope_prelude, args_json, function_name
        );

        if correlation::current_correlation_id().is_some() {
            self.evaluate(&js_code).await
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(&js_code).await
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
            self.evaluate(&js_code).await
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(&js_code).await
            })
            .await
        }
    }

    pub async fn invoke_js_function(&mut self, function_name: &str, args: Value) -> Result<Value> {
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
            self.evaluate(&js_code).await?
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(&js_code).await
            })
            .await?
        };

        match &result {
            Value::Object(map) if map.get("error").is_some() => Err(BamlRtError::QuickJs(format!(
                "JS function invocation error ({}): {}",
                function_name,
                map.get("error").and_then(Value::as_str).unwrap_or("unknown")
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
            self.evaluate(&js_code).await?
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(&js_code).await
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
                        promise = __baml_stream("{}", JSON.stringify(args), globalThis.__baml_context_id, globalThis.__baml_message_id, globalThis.__baml_task_id);
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
            self.evaluate(&js_code).await?
        } else {
            let correlation_id = correlation::generate_correlation_id();
            correlation::with_correlation_id(correlation_id, async {
                self.evaluate(&js_code).await
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

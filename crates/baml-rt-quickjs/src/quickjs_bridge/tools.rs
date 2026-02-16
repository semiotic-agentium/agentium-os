use super::wrappers;
use super::{QuickJSBridge, empty_open_input, tool_step_to_value};
use crate::js_value_converter::value_to_js_value_facade;
use baml_rt_core::context;
use baml_rt_core::correlation;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::ToolSessionId;
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde_json::Value;
use tokio::time::{Duration, timeout};

impl QuickJSBridge {
    /// Register all tool functions with QuickJS
    pub(crate) async fn register_tool_functions(&mut self) -> Result<()> {
        tracing::info!("Registering tool functions with QuickJS");

        // Register helper function to execute tools
        tracing::debug!("register_tool_functions: register_tool_invoke_helper start");
        self.register_tool_invoke_helper().await?;
        tracing::debug!("register_tool_functions: register_tool_invoke_helper done");

        tracing::debug!("register_tool_functions: register_tool_session_helpers start");
        self.register_tool_session_helpers().await?;
        tracing::debug!("register_tool_functions: register_tool_session_helpers done");

        tracing::debug!("register_tool_functions: register_tool_session_wrapper start");
        self.register_tool_session_wrapper().await?;
        tracing::debug!("register_tool_functions: register_tool_session_wrapper done");

        // Register a JS-callable wrapper per tool so each tool name is available as a function
        tracing::debug!("register_tool_functions: baml_manager lock start");
        let tool_names = timeout(Duration::from_secs(10), async {
            let manager = self.baml_manager.lock().await;
            manager.list_tools().await
        })
        .await
        .map_err(|_| {
            BamlRtError::QuickJs(
                "register_tool_functions timed out while waiting for baml_manager/list_tools"
                    .to_string(),
            )
        })?;
        tracing::debug!(
            tool_count = tool_names.len(),
            "register_tool_functions: discovered tool names"
        );

        for tool_name in tool_names {
            tracing::debug!(tool = %tool_name, "register_tool_functions: register_single_tool start");
            self.register_single_tool(&tool_name).await?;
            tracing::debug!(tool = %tool_name, "register_tool_functions: register_single_tool done");
        }

        tracing::debug!("Registering tool functions with QuickJS complete");
        Ok(())
    }

    /// Register a single tool function with QuickJS (per-tool JS wrapper).
    pub(crate) async fn register_single_tool(&mut self, tool_name: &str) -> Result<()> {
        // Register a JavaScript wrapper function for the tool
        let js_code = wrappers::build_token_args_wrapper(
            tool_name,
            &format!(
                "__tool_invoke(\"{}\", JSON.stringify(argObj))",
                tool_name.replace('\\', "\\\\").replace('"', "\\\"")
            ),
        );

        let script = Script::new("register_tool.js", &js_code);
        timeout(Duration::from_secs(5), self.runtime.eval(None, script))
            .await
            .map_err(|_| {
                BamlRtError::QuickJs(
                    "register_single_tool timed out while evaluating JS wrapper".to_string(),
                )
            })?
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register tool function".to_string(),
                source: Box::new(e),
            })?;

        tracing::debug!(tool = tool_name, "Registered tool function with QuickJS");
        Ok(())
    }

    /// Register helper function for tool invocation.
    /// Tokenless: host resolves scope from active context stack. JS calls __tool_invoke(toolName, argsJson).
    pub(crate) async fn register_tool_invoke_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();

        self.runtime.set_function(
            &[],
            "__tool_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = super::resolve_scope_from_active_context(&registry)?;
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (tool_name, args)"));
                }

                let tool_name_js = &args[0];
                let tool_name = if tool_name_js.is_string() {
                    tool_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Tool name must be a string"));
                };

                let args_js = &args[1];
                let args_json_str = if args_js.is_string() {
                    args_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Args must be a JSON string"));
                };

                let args_json: Value = serde_json::from_str(&args_json_str)
                    .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse JSON args: {}", e)))?;

                let tool_name_clone = tool_name.clone();
                let manager_for_promise = manager_clone.clone();
                let correlation_id = registry
                    .lock()
                    .ok()
                    .and_then(|g| g.current_frame().ok())
                    .and_then(|f| f.correlation_id);

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let run = async move {
                        let tool_scope = scope.clone();
                        context::with_scope(scope, async move {
                            let execution_handle = {
                                let manager = manager_for_promise.lock().await;
                                manager.tool_execution_handle()
                            };
                            let result = execution_handle
                                .execute_tool(&tool_scope, &tool_name_clone, args_json)
                                .await;
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
                    };
                    if let Some(correlation_id) = correlation_id {
                        correlation::with_correlation_id(correlation_id, run).await
                    } else {
                        run.await
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register tool helper function".to_string(),
            source: Box::new(e),
        })?;

        // Register __tool_from_baml_result: tokenless; host resolves from active context. JS calls (baml_result_json).
        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();
        self.runtime.set_function(
            &[],
            "__tool_from_baml_result",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = super::resolve_scope_from_active_context(&registry)?;
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (baml_result_json)"));
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
                let correlation_id = registry
                    .lock()
                    .ok()
                    .and_then(|g| g.current_frame().ok())
                    .and_then(|f| f.correlation_id);

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let run = async move {
                        let tool_scope = scope.clone();
                        context::with_scope(scope, async move {
                            let execution_handle = {
                                let manager = manager_for_promise.lock().await;
                                manager.tool_execution_handle()
                            };
                            let result = execution_handle
                                .execute_tool_from_baml_result(&tool_scope, baml_result)
                                .await;
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
                    };
                    if let Some(correlation_id) = correlation_id {
                        correlation::with_correlation_id(correlation_id, run).await
                    } else {
                        run.await
                    }
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

        tracing::debug!(
            "Registered __tool_invoke, __tool_from_baml_result, and invokeTool helper functions"
        );
        Ok(())
    }

    pub(crate) async fn register_tool_session_helpers(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();

        self.runtime.set_function(
            &[],
            "__tool_session_open",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = super::resolve_scope_from_active_context(&registry)?;
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (tool_name, openInputJson?)"));
                }
                let tool_name_js = &args[0];
                let tool_name = if tool_name_js.is_string() {
                    tool_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Tool name must be a string"));
                };
                let open_input = if args.len() > 1 && args[1].is_string() {
                    serde_json::from_str(args[1].get_str()).unwrap_or_else(|_| empty_open_input())
                } else {
                    empty_open_input()
                };
                let manager_for_promise = manager_clone.clone();
                let correlation_id = registry
                    .lock()
                    .ok()
                    .and_then(|g| g.current_frame().ok())
                    .and_then(|f| f.correlation_id);

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let run = async move {
                        let session_handle = {
                            let manager = manager_for_promise.lock().await;
                            manager.tool_session_handle()
                        };
                        let session_id = session_handle
                            .open_tool_session(&scope, &tool_name, open_input)
                            .await;
                        match session_id {
                            Ok(id) => Ok(JsValueFacade::new_string(id.as_str().into_owned())),
                            Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session open error: {}", e))),
                        }
                    };
                    if let Some(correlation_id) = correlation_id {
                        correlation::with_correlation_id(correlation_id, run).await
                    } else {
                        run.await
                    }
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_open".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_send",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = super::resolve_scope_from_active_context(&registry)?;
                if args.len() < 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (session_id, args)"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Session id must be a string"));
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
                    context::with_scope(scope, async move {
                        let session_handle = {
                            let manager = manager_for_promise.lock().await;
                            manager.tool_session_handle()
                        };
                        let result = session_handle.tool_session_send(&session_id, args_json).await;
                        match result {
                            Ok(_) => Ok(value_to_js_value_facade(Value::Null)),
                            Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session send error: {}", e))),
                        }
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_send".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_next",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = super::resolve_scope_from_active_context(&registry)?;
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (session_id)"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Session id must be a string"));
                };
                let manager_for_promise = manager_clone.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    context::with_scope(scope, async move {
                        let session_handle = {
                            let manager = manager_for_promise.lock().await;
                            manager.tool_session_handle()
                        };
                        let result = session_handle.tool_session_next(&session_id).await;
                        match result {
                            Ok(step) => {
                                let value = tool_step_to_value(step);
                                Ok(value_to_js_value_facade(value))
                            }
                            Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session next error: {}", e))),
                        }
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_next".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_finish",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = super::resolve_scope_from_active_context(&registry)?;
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (session_id)"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Session id must be a string"));
                };
                let manager_for_promise = manager_clone.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    context::with_scope(scope, async move {
                        let session_handle = {
                            let manager = manager_for_promise.lock().await;
                            manager.tool_session_handle()
                        };
                        let result = session_handle.tool_session_finish(&session_id).await;
                        match result {
                            Ok(_) => Ok(value_to_js_value_facade(Value::Null)),
                            Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session finish error: {}", e))),
                        }
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_finish".to_string(),
            source: Box::new(e),
        })?;

        let manager_clone = self.baml_manager.clone();
        let registry = self.invocation_context_registry.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_abort",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let scope = super::resolve_scope_from_active_context(&registry)?;
                if args.is_empty() {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (session_id, reason?)"));
                }
                let session_id = if args[0].is_string() {
                    ToolSessionId::parse(args[0].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Session id must be a string"));
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
                    context::with_scope(scope, async move {
                        let session_handle = {
                            let manager = manager_for_promise.lock().await;
                            manager.tool_session_handle()
                        };
                        let result = session_handle.tool_session_abort(&session_id, reason).await;
                        match result {
                            Ok(_) => Ok(value_to_js_value_facade(Value::Null)),
                            Err(e) => Err(quickjs_runtime::jsutils::JsError::new_str(&format!("Tool session abort error: {}", e))),
                        }
                    })
                    .await
                }))
            },
        ).map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __tool_session_abort".to_string(),
            source: Box::new(e),
        })?;

        tracing::debug!("Registered tool session helper functions");
        Ok(())
    }

    pub(crate) async fn register_tool_session_wrapper(&mut self) -> Result<()> {
        // Tokenless: host resolves invocation context from active context stack.
        let js_code = r#"
        globalThis.openToolSession = async function(toolName, openInput) {
            const sessionId = await __tool_session_open(toolName, JSON.stringify(openInput ?? {}));
            let phase = "Open";
            const isTerminal = () => phase === "Finish" || phase === "Abort";
            const assertNotTerminal = (op) => {
                if (isTerminal()) {
                    throw new Error(`Tool session ${sessionId} cannot ${op} after terminal phase ${phase}`);
                }
            };
            return {
                sessionId,
                phase: function() {
                    return phase;
                },
                send: async function(args) {
                    assertNotTerminal("send");
                    const argObj = args ?? {};
                    const out = await __tool_session_send(sessionId, JSON.stringify(argObj));
                    phase = "Send";
                    return out;
                },
                continue: async function() {
                    assertNotTerminal("continue");
                    const out = await __tool_session_next(sessionId);
                    phase = "Next";
                    return out;
                },
                finish: async function() {
                    assertNotTerminal("finish");
                    const out = await __tool_session_finish(sessionId);
                    phase = "Finish";
                    return out;
                },
                abort: async function(reason) {
                    assertNotTerminal("abort");
                    const out = await __tool_session_abort(sessionId, reason);
                    phase = "Abort";
                    return out;
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

    /// Register a JavaScript tool implementation.
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
}

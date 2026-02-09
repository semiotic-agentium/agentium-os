use super::{empty_open_input, tool_step_to_value, QuickJSBridge};
use super::wrappers;
use crate::js_value_converter::value_to_js_value_facade;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_core::correlation;
use baml_rt_core::context;
use baml_rt_tools::ToolSessionId;
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde_json::Value;

impl QuickJSBridge {
    /// Register all tool functions with QuickJS
    pub(crate) async fn register_tool_functions(&mut self) -> Result<()> {
        tracing::info!("Registering tool functions with QuickJS");

        // Register helper function to execute tools
        self.register_tool_invoke_helper().await?;
        self.register_tool_session_helpers().await?;
        self.register_tool_session_wrapper().await?;

        Ok(())
    }

    /// Register a single tool function with QuickJS
    #[allow(dead_code)]
    pub(crate) async fn register_single_tool(&mut self, tool_name: &str) -> Result<()> {
        let _manager_clone = self.baml_manager.clone();
        let _tool_name_clone = tool_name.to_string();

        // Register a JavaScript wrapper function for the tool
        let js_code = wrappers::build_token_args_wrapper(
            tool_name,
            &format!("__tool_invoke(token, \"{}\", JSON.stringify(argObj))", tool_name),
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
    pub(crate) async fn register_tool_invoke_helper(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();

        // Register __tool_invoke for Rust tools (low-level helper). Accepts (token, tool_name, args) or legacy (tool_name, args).
        self.runtime.set_function(
            &[],
            "__tool_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
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
                            let execution_handle = {
                                let manager = manager_for_promise.lock().await;
                                manager.tool_execution_handle()
                            };
                            let result = execution_handle
                                .execute_tool(&tool_name_clone, args_json)
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
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
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
                            let execution_handle = {
                                let manager = manager_for_promise.lock().await;
                                manager.tool_execution_handle()
                            };
                            let result = execution_handle.execute_tool_from_baml_result(baml_result).await;
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

    pub(crate) async fn register_tool_session_helpers(&mut self) -> Result<()> {
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();

        self.runtime.set_function(
            &[],
            "__tool_session_open",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let args_len = args.len();
                let first_arg_str = args.first().and_then(|a| if a.is_string() { Some(a.get_str().to_string()) } else { None });
                let (scope, skip) = match super::resolve_scope_from_token_arg(&scope_map, &args) {
                    Ok((s, sk)) => {
                        tracing::debug!(
                            __tool_session_open_args = args_len,
                            first_arg = ?first_arg_str,
                            scope_via = "token",
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
                            let session_handle = {
                                let manager = manager_for_promise.lock().await;
                                manager.tool_session_handle()
                            };
                            let open_input = empty_open_input();
                            let session_id = session_handle.open_tool_session(&tool_name, open_input).await;
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
                    let session_handle = {
                        let manager = manager_for_promise.lock().await;
                        manager.tool_session_handle()
                    };
                    let result = session_handle.tool_session_send(&session_id, args_json).await;
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
                    let session_handle = {
                        let manager = manager_for_promise.lock().await;
                        manager.tool_session_handle()
                    };
                    let result = session_handle.tool_session_finish(&session_id).await;
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
                    let session_handle = {
                        let manager = manager_for_promise.lock().await;
                        manager.tool_session_handle()
                    };
                    let result = session_handle.tool_session_abort(&session_id, reason).await;
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

    pub(crate) async fn register_tool_session_wrapper(&mut self) -> Result<()> {
        let js_code = r#"
        globalThis.openToolSession = async function(toolName, token) {
            if (!token) {
                throw new Error("Missing invocation token for openToolSession.");
            }
            const sessionId = await __tool_session_open(
                token,
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

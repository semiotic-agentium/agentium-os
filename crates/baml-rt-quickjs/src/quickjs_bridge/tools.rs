use super::wrappers;
use super::{QuickJSBridge, empty_open_input, tool_step_to_value};
use crate::baml::{extract_tool_call, extract_tool_session_plan};
use crate::js_value_converter::value_to_js_value_facade;
use baml_rt_core::context;
use baml_rt_core::correlation;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::ToolSessionId;
use quickjs_runtime::jsutils::Script;
use quickjs_runtime::quickjsrealmadapter::QuickJsRealmAdapter;
use quickjs_runtime::values::JsValueFacade;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

impl QuickJSBridge {
    /// Register all tool functions with QuickJS
    pub(crate) async fn register_tool_functions(&mut self) -> Result<()> {
        tracing::info!("Registering tool functions with QuickJS");

        // Register helper function to execute tools
        self.register_tool_invoke_helper().await?;
        self.register_react_loop_host().await?;
        self.register_tool_session_helpers().await?;
        self.register_tool_session_wrapper().await?;

        // Register a JS-callable wrapper per tool so each tool name is available as a function
        let tool_names = {
            let manager = self.baml_manager.lock().await;
            manager.list_tools().await
        };
        for tool_name in tool_names {
            self.register_single_tool(&tool_name).await?;
        }

        Ok(())
    }

    pub(crate) async fn register_react_loop_host(&mut self) -> Result<()> {
        #[derive(Deserialize)]
        struct ReActLoopHostOptions {
            #[serde(rename = "planFunction", alias = "plan_function")]
            plan_function: String,
            #[serde(rename = "userMessage", alias = "user_message")]
            user_message: String,
            #[serde(rename = "maxSteps", alias = "max_steps")]
            max_steps: Option<u32>,
            #[serde(default)]
            dedupe: Option<bool>,
        }

        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();
        let correlation_map = self.correlation_id_by_token.clone();

        self.runtime
            .set_function(
                &[],
                "__run_react_loop_host",
                move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                    let token = super::scope::token_from_args(&args).ok_or_else(|| {
                        quickjs_runtime::jsutils::JsError::new_str(
                            "Missing or invalid invocation token (first arg must be token string)",
                        )
                    })?;
                    let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
                    if args.len() < skip + 1 {
                        return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, opts_json)"));
                    }
                    let opts_json_str = if args[skip].is_string() {
                        args[skip].get_str().to_string()
                    } else {
                        return Err(quickjs_runtime::jsutils::JsError::new_str("Options must be a JSON string"));
                    };
                    let opts: ReActLoopHostOptions = serde_json::from_str(&opts_json_str)
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Failed to parse options JSON: {}", e)))?;
                    let manager_for_promise = manager_clone.clone();
                    let correlation_id = correlation_map
                        .lock()
                        .ok()
                        .and_then(|map| map.get(&token).cloned());

                    Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                        let run = async move {
                            context::with_scope(scope.clone(), async move {
                                let max_steps = opts.max_steps.unwrap_or(5);
                                let mut seen: HashSet<String> = HashSet::new();
                                for _ in 0..max_steps {
                                    let args = serde_json::json!({ "user_message": &opts.user_message });
                                    let plan_value = {
                                        let manager = manager_for_promise.lock().await;
                                        manager.invoke_function(&scope, &opts.plan_function, args).await
                                    };
                                    let plan_value = match plan_value {
                                        Ok(value) => value,
                                        Err(e) => {
                                            return Err(quickjs_runtime::jsutils::JsError::new_str(
                                                &format!("ReAct plan function error: {}", e),
                                            ));
                                        }
                                    };

                                    let is_plan = extract_tool_session_plan(&plan_value)
                                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Plan extraction error: {}", e)))?
                                        .is_some()
                                        || extract_tool_call(&plan_value)
                                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&format!("Plan extraction error: {}", e)))?
                                            .is_some();

                                    if !is_plan {
                                        if let Some(message) = match &plan_value {
                                            Value::String(s) => Some(s.clone()),
                                            Value::Object(map) => map.get("message").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                            _ => None,
                                        } {
                                            return Ok(JsValueFacade::new_string(message));
                                        }
                                        return Err(quickjs_runtime::jsutils::JsError::new_str(
                                            "ReAct plan did not return a tool call or final message",
                                        ));
                                    }

                                    if opts.dedupe.unwrap_or(true) {
                                        let key = serde_json::to_string(&plan_value).unwrap_or_default();
                                        if seen.contains(&key) {
                                            return Err(quickjs_runtime::jsutils::JsError::new_str(
                                                "runReActLoopHost detected repeated tool call",
                                            ));
                                        }
                                        seen.insert(key);
                                    }

                                    let observation = {
                                        let manager = manager_for_promise.lock().await;
                                        manager.execute_tool_from_baml_result_or_value(&scope, plan_value).await
                                    };
                                    if let Err(e) = observation {
                                        return Err(quickjs_runtime::jsutils::JsError::new_str(
                                            &format!("ReAct tool execution error: {}", e),
                                        ));
                                    }
                                }
                                Err(quickjs_runtime::jsutils::JsError::new_str(
                                    "runReActLoopHost exceeded maxSteps",
                                ))
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
            )
            .map_err(|e| BamlRtError::QuickJsWithSource {
                context: "Failed to register runReActLoopHost".to_string(),
                source: Box::new(e),
            })?;

        Ok(())
    }

    /// Register a single tool function with QuickJS (per-tool JS wrapper).
    pub(crate) async fn register_single_tool(&mut self, tool_name: &str) -> Result<()> {
        // Register a JavaScript wrapper function for the tool
        let js_code = wrappers::build_token_args_wrapper(
            tool_name,
            &format!(
                "__tool_invoke(token, \"{}\", JSON.stringify(argObj))",
                tool_name
            ),
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
        let correlation_map = self.correlation_id_by_token.clone();

        // Register __tool_invoke for Rust tools (low-level helper). Accepts (token, tool_name, args) or legacy (tool_name, args).
        self.runtime.set_function(
            &[],
            "__tool_invoke",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let token = super::scope::token_from_args(&args).ok_or_else(|| {
                    quickjs_runtime::jsutils::JsError::new_str(
                        "Missing or invalid invocation token (first arg must be token string)",
                    )
                })?;
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
                if args.len() < skip + 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, tool_name, args)"));
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
                let correlation_id = correlation_map
                    .lock()
                    .ok()
                    .and_then(|map| map.get(&token).cloned());

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

        // Register __tool_from_baml_result for executing tools based on BAML union output. Accepts (token, baml_result).
        let manager_clone = self.baml_manager.clone();
        let scope_map = self.invocation_scope_by_token.clone();
        let correlation_map = self.correlation_id_by_token.clone();
        self.runtime.set_function(
            &[],
            "__tool_from_baml_result",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let token = super::scope::token_from_args(&args).ok_or_else(|| {
                    quickjs_runtime::jsutils::JsError::new_str(
                        "Missing or invalid invocation token (first arg must be token string)",
                    )
                })?;
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
                if args.len() < skip + 1 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, baml_result_json)"));
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
                let correlation_id = correlation_map
                    .lock()
                    .ok()
                    .and_then(|map| map.get(&token).cloned());

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
        let scope_map = self.invocation_scope_by_token.clone();
        let correlation_map = self.correlation_id_by_token.clone();

        self.runtime.set_function(
            &[],
            "__tool_session_open",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let token = super::scope::token_from_args(&args).ok_or_else(|| {
                    quickjs_runtime::jsutils::JsError::new_str(
                        "Missing or invalid invocation token (first arg must be token string)",
                    )
                })?;
                let args_len = args.len();
                let first_arg_str = args.first().and_then(|a| if a.is_string() { Some(a.get_str().to_string()) } else { None });
                let (scope, skip) = match super::resolve_scope_from_token_arg(&scope_map, &args) {
                    Ok((s, sk)) => {
                        tracing::debug!(
                            __tool_session_open_args = args_len,
                            first_arg = ?first_arg_str,
                            scope_via = "token",
                            context_id = %s.context_id(),
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
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, tool_name)"));
                }
                let tool_name_js = &args[skip];
                let tool_name = if tool_name_js.is_string() {
                    tool_name_js.get_str().to_string()
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Tool name must be a string"));
                };
                let manager_for_promise = manager_clone.clone();
                let correlation_id = correlation_map
                    .lock()
                    .ok()
                    .and_then(|map| map.get(&token).cloned());

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let run = async move {
                        let session_handle = {
                            let manager = manager_for_promise.lock().await;
                            manager.tool_session_handle()
                        };
                        let open_input = if tool_name == "a2a/session" {
                            serde_json::json!({ "scope": scope.clone() })
                        } else {
                            empty_open_input()
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
        let scope_map = self.invocation_scope_by_token.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_send",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
                if args.len() < skip + 2 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, session_id, args)"));
                }
                let session_id = if args[skip].is_string() {
                    ToolSessionId::parse(args[skip].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Session id must be a string"));
                };
                let args_json_str = if args[skip + 1].is_string() {
                    args[skip + 1].get_str().to_string()
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
        let scope_map = self.invocation_scope_by_token.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_next",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
                if args.len() < skip + 1 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, session_id)"));
                }
                let session_id = if args[skip].is_string() {
                    ToolSessionId::parse(args[skip].get_str())
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
        let scope_map = self.invocation_scope_by_token.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_finish",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
                if args.len() < skip + 1 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, session_id)"));
                }
                let session_id = if args[skip].is_string() {
                    ToolSessionId::parse(args[skip].get_str())
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
        let scope_map = self.invocation_scope_by_token.clone();
        self.runtime.set_function(
            &[],
            "__tool_session_abort",
            move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let (scope, skip) = super::resolve_scope_from_token_arg(&scope_map, &args)?;
                if args.len() < skip + 1 {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Expected (token, session_id, reason?)"));
                }
                let session_id = if args[skip].is_string() {
                    ToolSessionId::parse(args[skip].get_str())
                        .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                } else {
                    return Err(quickjs_runtime::jsutils::JsError::new_str("Session id must be a string"));
                };
                let reason = args.get(skip + 1).and_then(|value| {
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
        // JS FSM: API-level misuse prevention; host (Rust) remains source-of-truth for terminal transitions.
        let js_code = r#"
        globalThis.openToolSession = async function(toolName, token) {
            if (!token) {
                throw new Error("Missing invocation token for openToolSession.");
            }
            const emitToolEvent = (type, payload) => {
                if (typeof __chat_yield !== "function") return;
                __chat_yield({
                    event: {
                        type,
                        source: "runtime",
                        tool: payload
                    }
                });
            };
            const sessionId = await __tool_session_open(
                token,
                toolName
            );
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
                    emitToolEvent("tool_execution_start", { name: toolName, sessionId, args: argObj });
                    try {
                        const out = await __tool_session_send(token, sessionId, JSON.stringify(argObj));
                        phase = "Send";
                        return out;
                    } catch (e) {
                        emitToolEvent("tool_execution_end", { name: toolName, sessionId, isError: true, error: String(e) });
                        throw e;
                    }
                },
                continue: async function() {
                    assertNotTerminal("continue");
                    const out = await __tool_session_next(token, sessionId);
                    if (out && out.status === "streaming") {
                        emitToolEvent("tool_execution_update", { name: toolName, sessionId, output: out.output });
                    } else if (out && out.status === "done") {
                        emitToolEvent("tool_execution_end", { name: toolName, sessionId, isError: false, result: out.output });
                    } else if (out && out.status === "error") {
                        emitToolEvent("tool_execution_end", { name: toolName, sessionId, isError: true, error: out.error });
                    }
                    phase = "Next";
                    return out;
                },
                finish: async function() {
                    assertNotTerminal("finish");
                    const out = await __tool_session_finish(token, sessionId);
                    phase = "Finish";
                    return out;
                },
                abort: async function(reason) {
                    assertNotTerminal("abort");
                    const out = await __tool_session_abort(token, sessionId, reason);
                    emitToolEvent("tool_execution_end", { name: toolName, sessionId, isError: true, error: reason ?? "aborted" });
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

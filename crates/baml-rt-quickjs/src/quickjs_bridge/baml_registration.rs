//! BAML and await/stream registration with QuickJS.
//!
//! All __baml_invoke, __baml_stream, __set_eval_result, await helpers, and
//! per-function registration live here so the main bridge focuses on coordination.

use std::sync::atomic::Ordering;

use baml_rt_core::{
    BamlRtError, Result,
    context::{self, InvocationScope},
    correlation,
};
use quickjs_runtime::{
    jsutils::Script, quickjsrealmadapter::QuickJsRealmAdapter, values::JsValueFacade,
};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{
    QuickJSBridge,
    scope::{InvocationToken, resolve_scope_from_active_context, resolve_scope_from_session},
    stream_yield, tools,
    types::InFlightGuard,
    wrappers,
};
use crate::js_value_converter::value_to_js_value_facade;

/// Register __baml_invoke. Tokenless: host resolves scope from active context.
pub(super) async fn register_baml_invoke_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let registry = bridge.invocation_context_registry().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();

    bridge.runtime().set_function(
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
                        let result = manager
                            .execute_tool_from_baml_result_or_value(
                                &scope_for_tools,
                                value,
                                Some(&func_name_clone),
                            )
                            .await
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
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register helper function".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_invoke helper function with async promise support");
    Ok(())
}

/// Register __awaitAndStringify and __set_eval_result.
pub(super) async fn register_await_helper(bridge: &QuickJSBridge) -> Result<()> {
    let js_code = r#"
            globalThis.__awaitAndStringify = async function(promise) {
                try {
                    const result = await promise;
                    return JSON.stringify(result);
                } catch (e) {
                    return JSON.stringify({ error: e.toString() });
                }
            };

            globalThis.__isPromise = function(value) {
                return value && typeof value.then === 'function';
            };
        "#;

    let script = Script::new("await_helper.js", js_code);
    bridge
        .runtime()
        .eval(None, script)
        .await
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register await helper".to_string(),
            source: Box::new(e),
        })?;

    let eval_results = bridge.eval_results_by_token().clone();
    let eval_notify_by_token = bridge.eval_notify_by_token().clone();
    bridge.runtime().set_function(
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
            let key = InvocationToken(token);
            {
                let mut guard = eval_results
                    .lock()
                    .map_err(|_| quickjs_runtime::jsutils::JsError::new_str("eval_results lock poisoned"))?;
                if !guard.contains_key(&key) {
                    // Late promise resolution after host cleanup (e.g. bounded resume poll timeout).
                    // Ignore stale writes so we do not surface an unhandled rejection in JS.
                    tracing::debug!(
                        token = %key.0,
                        "stale eval result: token slot already removed"
                    );
                    return Ok(JsValueFacade::Undefined);
                }
                guard.insert(key.clone(), Some(json_str));
            }
            if let Ok(mut notify_guard) = eval_notify_by_token.lock()
                && let Some(notify) = notify_guard.remove(&key)
            {
                tracing::debug!(token = %key.0, "eval result set, notifying poll loop");
                notify.notify_one();
            }
            Ok(JsValueFacade::Undefined)
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __set_eval_result".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __awaitAndStringify helper function");
    Ok(())
}

/// Register a single BAML function with QuickJS (tokenless wrapper).
pub(super) async fn register_single_function(
    bridge: &QuickJSBridge,
    function_name: &str,
) -> Result<()> {
    let js_code = wrappers::build_token_args_wrapper(
        function_name,
        &format!(
            "__baml_invoke(\"{}\", JSON.stringify(argObj))",
            function_name.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    );

    let script = Script::new("register_function.js", &js_code);
    bridge
        .runtime()
        .eval(None, script)
        .await
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register function".to_string(),
            source: Box::new(e),
        })?;

    tracing::debug!(function = function_name, "Registered function with QuickJS");
    Ok(())
}

/// Register a streaming version of a single BAML function with QuickJS.
pub(super) async fn register_single_stream_function(
    bridge: &QuickJSBridge,
    function_name: &str,
) -> Result<()> {
    let stream_function_name = format!("{}Stream", function_name);
    let js_code = wrappers::build_token_args_wrapper(
        &stream_function_name,
        &format!(
            "__baml_stream(\"{}\", JSON.stringify(argObj))",
            function_name.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    );

    let script = Script::new("register_stream_function.js", &js_code);
    bridge
        .runtime()
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

/// Register __baml_stream. Tokenless: host resolves scope from active context. JS calls (function_name, args).
pub(super) async fn register_baml_stream_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let registry = bridge.invocation_context_registry().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();
    let stream_sessions = bridge.stream_sessions().clone();

    bridge.runtime().set_function(
        &[],
        "__baml_stream",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            let scope = resolve_scope_from_active_context(&registry)?;
            if args.len() < 2 {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Expected (function_name, args)",
                ));
            }

            let func_name_js = &args[0];
            let func_name = if func_name_js.is_string() {
                func_name_js.get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Function name must be a string",
                ));
            };

            let args_js = &args[1];
            let args_json_str = if args_js.is_string() {
                args_js.get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Args must be a JSON string - use JSON.stringify in JavaScript",
                ));
            };

            let args_json: Value = serde_json::from_str(&args_json_str).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse JSON args: {}",
                    e
                ))
            })?;

            let (context_tags, stream_session_id_for_chunks) = stream_sessions
                .lock()
                .ok()
                .and_then(|g| {
                    g.iter()
                        .find(|(_sid, session)| !session.is_terminated() && session.scope == scope)
                        .map(|(sid, session)| (session.context_tags.clone(), Some(sid.0)))
                })
                .unwrap_or((None, None));

            let func_name_clone = func_name.clone();
            let manager_for_promise = manager_clone.clone();
            let scope_for_scope = scope.clone();
            let correlation_id = registry
                .lock()
                .ok()
                .and_then(|g| g.current_frame().ok())
                .and_then(|f| f.correlation_id);

            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(scope_for_scope.clone(), async move {
                        let inv = {
                            let manager = manager_for_promise.lock().await;
                            manager.invoke_function_stream(
                                &scope_for_scope,
                                &func_name_clone,
                                args_json.clone(),
                                context_tags,
                            )
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?
                        };

                        let (tx, mut rx) = mpsc::channel::<Value>(64);
                        let tx_closure = tx.clone();

                        let crate::baml_execution::BamlStreamInvocation {
                            mut stream,
                            ctx_manager,
                            client_registry_opt,
                            env_vars,
                        } = inv;

                        stream_yield::scope_stream_yield(Some(tx), async move {
                            let (_result, _call_id) = stream
                                .run(
                                    None::<fn()>,
                                    Some(move |fr: baml_runtime::FunctionResult| {
                                        if let Some(Ok(parsed)) = fr.parsed().as_ref()
                                            && let Ok(mut v) =
                                                serde_json::to_value(parsed.serialize_partial())
                                        {
                                            if let Some(session_id) = stream_session_id_for_chunks
                                                && let Some(obj) = v.as_object_mut()
                                            {
                                                obj.insert(
                                                    "__session".to_string(),
                                                    serde_json::Value::from(session_id),
                                                );
                                            }
                                            if let Err(e) = tx_closure.try_send(v) {
                                                tracing::warn!(error = ?e, "Stream chunk dropped: channel full");
                                            }
                                        }
                                    }),
                                    &ctx_manager,
                                    None,
                                    client_registry_opt.as_ref(),
                                    env_vars,
                                )
                                .await;
                        })
                        .await;

                        let mut chunks = Vec::new();
                        while let Ok(v) = rx.try_recv() {
                            chunks.push(v);
                        }
                        while let Some(v) = rx.recv().await {
                            chunks.push(v);
                        }
                        Ok(value_to_js_value_facade(Value::Array(chunks)))
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
        context: "Failed to register __baml_stream helper".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_stream helper function");
    Ok(())
}

/// Register `__baml_invoke_session(session_id, function_name, args_json)`.
pub(super) async fn register_baml_invoke_session_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let stream_sessions = bridge.stream_sessions().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();

    bridge.runtime().set_function(
        &[],
        "__baml_invoke_session",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            let session_id = tools::parse_session_id_arg(&args)?;
            let (scope, session) = match resolve_scope_from_session(&stream_sessions, session_id) {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = e.to_string();
                    return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                        Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                    }));
                }
            };
            if args.len() < 3 {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Expected (session_id, function_name, args_json)",
                ));
            }
            let func_name = if args[1].is_string() {
                args[1].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Function name must be a string",
                ));
            };
            let args_json_str = if args[2].is_string() {
                args[2].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Args must be a JSON string",
                ));
            };
            let args_json: Value = serde_json::from_str(&args_json_str).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse JSON args: {}",
                    e
                ))
            })?;

            let correlation_id = session.correlation_id.clone();
            let scope_for_tools = scope.clone();
            let manager_for_promise = manager_clone.clone();
            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(scope, async move {
                        let manager = manager_for_promise.lock().await;
                        let invocation_scope = InvocationScope::new(scope_for_tools.clone());
                        let value = manager
                            .invoke_function(&invocation_scope, &func_name, args_json)
                            .await
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                        let result = manager
                            .execute_tool_from_baml_result_or_value(
                                &scope_for_tools,
                                value,
                                Some(&func_name),
                            )
                            .await
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;
                        Ok(value_to_js_value_facade(result))
                    })
                    .await
                };
                if let Some(cid) = correlation_id {
                    correlation::with_correlation_id(cid, run).await
                } else {
                    run.await
                }
            }))
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __baml_invoke_session helper".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_invoke_session helper");
    Ok(())
}

/// Register `__baml_stream_session(session_id, function_name, args_json)`.
pub(super) async fn register_baml_stream_session_helper(bridge: &QuickJSBridge) -> Result<()> {
    let manager_clone = bridge.baml_manager().clone();
    let stream_sessions = bridge.stream_sessions().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();

    bridge.runtime().set_function(
        &[],
        "__baml_stream_session",
        move |_realm: &QuickJsRealmAdapter, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
            let session_id = tools::parse_session_id_arg(&args)?;
            let (scope, session) = match resolve_scope_from_session(&stream_sessions, session_id) {
                Ok(pair) => pair,
                Err(e) => {
                    let msg = e.to_string();
                    return Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                        Err(quickjs_runtime::jsutils::JsError::new_str(&msg))
                    }));
                }
            };
            if args.len() < 3 {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Expected (session_id, function_name, args_json)",
                ));
            }
            let func_name = if args[1].is_string() {
                args[1].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Function name must be a string",
                ));
            };
            let args_json_str = if args[2].is_string() {
                args[2].get_str().to_string()
            } else {
                return Err(quickjs_runtime::jsutils::JsError::new_str(
                    "Args must be a JSON string",
                ));
            };
            let args_json: Value = serde_json::from_str(&args_json_str).map_err(|e| {
                quickjs_runtime::jsutils::JsError::new_str(&format!(
                    "Failed to parse JSON args: {}",
                    e
                ))
            })?;

            let context_tags = session.context_tags.clone();
            let stream_session_id_for_chunks = session_id.0;
            let correlation_id = session.correlation_id.clone();
            let scope_for_run = scope.clone();
            let manager_for_promise = manager_clone.clone();
            in_flight.fetch_add(1, Ordering::Release);
            let guard_counter = in_flight.clone();

            Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                let _in_flight_guard = InFlightGuard(guard_counter);
                let run = async move {
                    context::with_scope(scope, async move {
                        let inv = manager_for_promise
                            .lock()
                            .await
                            .invoke_function_stream(
                                &scope_for_run,
                                &func_name,
                                args_json.clone(),
                                context_tags,
                            )
                            .map_err(|e| quickjs_runtime::jsutils::JsError::new_str(&e.to_string()))?;

                        let (tx, mut rx) = mpsc::channel::<Value>(64);
                        let tx_closure = tx.clone();

                        let crate::baml_execution::BamlStreamInvocation {
                            mut stream,
                            ctx_manager,
                            client_registry_opt,
                            env_vars,
                        } = inv;

                        stream_yield::scope_stream_yield(Some(tx), async move {
                            let (_result, _call_id) = stream
                                .run(
                                    None::<fn()>,
                                    Some(move |fr: baml_runtime::FunctionResult| {
                                        if let Some(Ok(parsed)) = fr.parsed().as_ref()
                                            && let Ok(mut v) =
                                                serde_json::to_value(parsed.serialize_partial())
                                        {
                                            if let Some(obj) = v.as_object_mut() {
                                                obj.insert(
                                                    "__session".to_string(),
                                                    serde_json::Value::from(stream_session_id_for_chunks),
                                                );
                                            }
                                            if let Err(e) = tx_closure.try_send(v) {
                                                tracing::warn!(error = ?e, "Stream chunk dropped: channel full");
                                            }
                                        }
                                    }),
                                    &ctx_manager,
                                    None,
                                    client_registry_opt.as_ref(),
                                    env_vars,
                                )
                                .await;
                        })
                        .await;

                        let mut chunks = Vec::new();
                        while let Ok(v) = rx.try_recv() {
                            chunks.push(v);
                        }
                        while let Some(v) = rx.recv().await {
                            chunks.push(v);
                        }
                        Ok(value_to_js_value_facade(Value::Array(chunks)))
                    })
                    .await
                };
                if let Some(cid) = correlation_id {
                    correlation::with_correlation_id(cid, run).await
                } else {
                    run.await
                }
            }))
        },
    )
    .map_err(|e| BamlRtError::QuickJsWithSource {
        context: "Failed to register __baml_stream_session helper".to_string(),
        source: Box::new(e),
    })?;

    tracing::debug!("Registered __baml_stream_session helper");
    Ok(())
}

//! Host dispatch ingress natives: unit task scope push/pop around `withTask` preludes.

use std::sync::atomic::{AtomicU64, Ordering};

use baml_rt_core::{BamlRtError, Result, dispatch_ingress::DispatchWorkUnit};
use quickjs_runtime::values::JsValueFacade;
use serde_json::{Value, json};

use super::{
    QuickJSBridge,
    scope::{InvocationContextId, resolve_scope_from_active_context},
    types::InFlightGuard,
};

static DISPATCH_UNIT_FRAME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_dispatch_unit_frame_id() -> InvocationContextId {
    let n = DISPATCH_UNIT_FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    InvocationContextId(format!("dispatch-unit-frame-{n}"))
}

fn js_err(message: impl std::fmt::Display) -> quickjs_runtime::jsutils::JsError {
    quickjs_runtime::jsutils::JsError::new_str(&message.to_string())
}

fn rt_err(err: BamlRtError) -> quickjs_runtime::jsutils::JsError {
    js_err(err)
}

fn unit_ctx_json(prelude: &baml_rt_core::WithTaskPrelude) -> Value {
    let scope = &prelude.scope;
    json!({
        "unitKey": prelude.unit_key,
        "contextId": scope.context_id().as_str(),
        "taskId": scope.task_id_opt().map(|t| t.as_str()),
        "messageId": scope.message_id().as_str(),
        "unitHistoryRef": format!("#{}", prelude.unit_history_ref),
    })
}

fn value_to_js(value: &Value) -> JsValueFacade {
    JsValueFacade::new_string(serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()))
}

fn parse_records_arg(
    args: &[JsValueFacade],
) -> std::result::Result<Vec<Value>, quickjs_runtime::jsutils::JsError> {
    if args.len() < 2 {
        return Err(js_err("expected (unitKey, recordsJson)"));
    }
    if !args[1].is_string() {
        return Err(js_err("records must be a JSON string"));
    }
    let records_json = args[1].get_str().to_string();
    serde_json::from_str(&records_json).map_err(|e| js_err(format!("invalid records JSON: {e}")))
}

fn parse_unit_key(
    args: &[JsValueFacade],
) -> std::result::Result<String, quickjs_runtime::jsutils::JsError> {
    if args.is_empty() || !args[0].is_string() {
        return Err(js_err("expected unitKey string"));
    }
    let key = args[0].get_str().trim().to_string();
    if key.is_empty() {
        return Err(js_err("unitKey must be non-empty"));
    }
    Ok(key)
}

/// Register `__dispatch_enter_unit_task` / `__dispatch_exit_unit_task` for `withTask` scope nesting.
pub(super) async fn register_dispatch_ingress_helpers(bridge: &QuickJSBridge) -> Result<()> {
    let recorder = bridge.host_ingress_recorder().clone();
    let agent_id = bridge.agent_id().clone();
    let registry = bridge.invocation_context_registry().clone();
    let unit_frames = bridge.dispatch_unit_frames().clone();
    let in_flight = bridge.in_flight_invoke_count_arc().clone();

    bridge
        .runtime()
        .set_function(
            &[],
            "__dispatch_enter_unit_task",
            move |_realm, args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let recorder = recorder.clone();
                let agent_id = agent_id.clone();
                let registry = registry.clone();
                let unit_frames = unit_frames.clone();
                let unit_key = parse_unit_key(&args)?;
                let records = parse_records_arg(&args)?;
                if records.is_empty() {
                    return Err(js_err("withTask records must be non-empty"));
                }

                in_flight.fetch_add(1, Ordering::Release);
                let guard_counter = in_flight.clone();

                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let _guard = InFlightGuard(guard_counter);
                    let Some(recorder) = recorder.as_ref() else {
                        return Err(js_err(
                            "dispatch ingress recorder not configured on this bridge",
                        ));
                    };
                    let unit = DispatchWorkUnit::new(unit_key.clone(), records).map_err(rt_err)?;
                    let parent_scope = resolve_scope_from_active_context(&registry)?;
                    let prelude = recorder
                        .with_task_prelude(&parent_scope, agent_id, unit)
                        .await
                        .map_err(rt_err)?;
                    let frame_id = next_dispatch_unit_frame_id();
                    {
                        let mut guard = registry.lock().map_err(|_| {
                            js_err("invocation context registry lock poisoned")
                        })?;
                        guard.enter(prelude.scope.clone(), None);
                        let mut frames = unit_frames.lock().map_err(|_| {
                            js_err("dispatch unit frame stack lock poisoned")
                        })?;
                        frames.push(frame_id);
                    }
                    Ok(value_to_js(&unit_ctx_json(&prelude)))
                }))
            },
        )
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __dispatch_enter_unit_task".to_string(),
            source: Box::new(e),
        })?;

    let registry_exit = bridge.invocation_context_registry().clone();
    let unit_frames_exit = bridge.dispatch_unit_frames().clone();
    let in_flight_exit = bridge.in_flight_invoke_count_arc().clone();

    bridge
        .runtime()
        .set_function(
            &[],
            "__dispatch_exit_unit_task",
            move |_realm, _args: Vec<JsValueFacade>| -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
                let registry = registry_exit.clone();
                let unit_frames = unit_frames_exit.clone();
                in_flight_exit.fetch_add(1, Ordering::Release);
                let guard_counter = in_flight_exit.clone();
                Ok(JsValueFacade::new_promise::<JsValueFacade, _, ()>(async move {
                    let _guard = InFlightGuard(guard_counter);
                    let frame_id = {
                        let mut frames = unit_frames.lock().map_err(|_| {
                            js_err("dispatch unit frame stack lock poisoned")
                        })?;
                        frames.pop().ok_or_else(|| js_err("no active dispatch unit frame"))?
                    };
                    let mut guard = registry.lock().map_err(|_| {
                        js_err("invocation context registry lock poisoned")
                    })?;
                    guard.exit(&frame_id);
                    Ok(JsValueFacade::Null)
                }))
            },
        )
        .map_err(|e| BamlRtError::QuickJsWithSource {
            context: "Failed to register __dispatch_exit_unit_task".to_string(),
            source: Box::new(e),
        })?;

    Ok(())
}

use baml_rt_core::context::RuntimeScope;
use quickjs_runtime::values::JsValueFacade;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

/// Opaque token issued by the host for the duration of an invocation. JS receives only this
/// string; natives look up scope by token so JS cannot forge attribution. See
/// docs/QUICKJS_THREADING_AND_SCOPE.md.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct InvocationToken(pub(crate) String);

static INVOCATION_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_invocation_token() -> InvocationToken {
    let n = INVOCATION_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    InvocationToken(format!("inv-{}", n))
}

// Worker-thread invocation scope: set by the bridge when running eval via `loop_realm` with
// an `InvocationScope`. Native callbacks run on the QuickJS worker thread and read this
// instead of task-local context (see docs/QUICKJS_THREADING_AND_SCOPE.md).
thread_local! {
    static WORKER_INVOCATION_SCOPE: RefCell<Option<RuntimeScope>> = const { RefCell::new(None) };
}

/// Resolve invocation scope from native args: first arg must be a non-empty token string;
/// look up scope in map. Returns (scope, skip_count) where skip_count is 1 (token consumed).
/// No worker-thread fallback; token is required so native callbacks have explicit provenance.
pub(crate) fn resolve_scope_from_token_arg(
    map: &Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>,
    args: &[JsValueFacade],
) -> std::result::Result<(RuntimeScope, usize), quickjs_runtime::jsutils::JsError> {
    if !args.is_empty()
        && let Some(token_js) = args.first()
            && token_js.is_string()
    {
        let s = token_js.get_str().to_string();
        if !s.is_empty() {
            if let Ok(guard) = map.lock()
                && let Some(scope) = guard.get(&InvocationToken(s)) 
            {
                return Ok((scope.clone(), 1));
            }
            return Err(quickjs_runtime::jsutils::JsError::new_str(
                "Invalid or expired invocation token",
            ));
        }
    }
    Err(quickjs_runtime::jsutils::JsError::new_str(
        "Missing or invalid invocation token (bind a token in the eval scope or pass it explicitly to __tool_invoke/__baml_invoke/__baml_stream)",
    ))
}

#[allow(dead_code)]
pub(crate) fn worker_thread_scope() -> Option<RuntimeScope> {
    WORKER_INVOCATION_SCOPE.with(|cell| cell.borrow().clone())
}

/// Clear the worker-thread invocation scope. Call when a stream invocation is done (e.g. in
/// [`get_a2a_yield_buffer`](crate::quickjs_bridge::QuickJSBridge::get_a2a_yield_buffer)) so the next operation doesn't see the old scope.
pub(crate) fn clear_worker_thread_scope() {
    WORKER_INVOCATION_SCOPE.with(|cell| {
        let _ = cell.replace(None);
    });
}

pub(crate) async fn run_eval_with_scope(
    runtime: &quickjs_runtime::facades::QuickJsRuntimeFacade,
    scope: &baml_rt_core::context::InvocationScope,
    script: quickjs_runtime::jsutils::Script,
    clear_after: bool,
) -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
    let scope_runtime = scope.as_scope().clone();
    runtime
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

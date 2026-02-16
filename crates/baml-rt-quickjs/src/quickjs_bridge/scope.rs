//! Host-only invocation context: no token material in JS.
//!
//! The host maintains an active-context stack per bridge. When we enter an invocation
//! (e.g. evaluate(Some(scope), ...) or invoke_js_function_stream), we push the scope;
//! when we exit we pop. Native callbacks resolve scope from the current top of stack
//! **at the moment the native is invoked** (synchronously), then capture that scope
//! and use it for the entire async operation (e.g. tool run, BAML invoke). Completions
//! can happen in arbitrary order—we never re-read "current" at completion time, so
//! invocation order does not need to be serialised. Re-entrant safe (nested invocations
//! push; resolution is LIFO). No globals; no JS token args.

use baml_rt_core::context::RuntimeScope;
use quickjs_runtime::values::JsValueFacade;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

/// Opaque host-only context id for the duration of an invocation. Never exposed to JS.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct InvocationContextId(pub(crate) String);

static INVOCATION_CONTEXT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_invocation_context_id() -> InvocationContextId {
    let n = INVOCATION_CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed);
    InvocationContextId(format!("inv-ctx-{}", n))
}

/// Frame stored per active invocation: scope and optional correlation id.
#[derive(Debug, Clone)]
pub(crate) struct InvocationContextFrame {
    pub(crate) scope: RuntimeScope,
    pub(crate) correlation_id: Option<baml_rt_core::ids::CorrelationId>,
}

/// Per-bridge registry: stack of active context ids and map id -> frame.
/// Native callbacks resolve current scope from the top of the stack (LIFO).
pub(crate) struct InvocationContextRegistry {
    stack: Vec<InvocationContextId>,
    by_id: HashMap<InvocationContextId, InvocationContextFrame>,
}

impl InvocationContextRegistry {
    pub(crate) fn new() -> Self {
        Self {
            stack: Vec::new(),
            by_id: HashMap::new(),
        }
    }

    /// Enter an invocation: push scope (and optional correlation_id), return the context id.
    /// Call [`exit`](Self::exit) with this id when the invocation ends.
    pub(crate) fn enter(
        &mut self,
        scope: RuntimeScope,
        correlation_id: Option<baml_rt_core::ids::CorrelationId>,
    ) -> InvocationContextId {
        let id = next_invocation_context_id();
        self.by_id.insert(
            id.clone(),
            InvocationContextFrame {
                scope,
                correlation_id,
            },
        );
        self.stack.push(id.clone());
        id
    }

    /// Exit the invocation for this id. Must match the id returned from [`enter`](Self::enter).
    pub(crate) fn exit(&mut self, id: &InvocationContextId) {
        if self.stack.last() == Some(id) {
            self.stack.pop();
        }
        self.by_id.remove(id);
    }

    /// Resolve the current (top of stack) scope. Errors if no active invocation.
    pub(crate) fn current_scope(
        &self,
    ) -> std::result::Result<RuntimeScope, quickjs_runtime::jsutils::JsError> {
        let id = self.stack.last().ok_or_else(|| {
            quickjs_runtime::jsutils::JsError::new_str(
                "No invocation context (native called without active host invocation)",
            )
        })?;
        self.by_id.get(id).map(|f| f.scope.clone()).ok_or_else(|| {
            quickjs_runtime::jsutils::JsError::new_str("Invalid or expired invocation context")
        })
    }

    /// Current frame (scope + correlation_id) for the active invocation.
    pub(crate) fn current_frame(
        &self,
    ) -> std::result::Result<InvocationContextFrame, quickjs_runtime::jsutils::JsError> {
        let id = self.stack.last().ok_or_else(|| {
            quickjs_runtime::jsutils::JsError::new_str(
                "No invocation context (native called without active host invocation)",
            )
        })?;
        self.by_id.get(id).cloned().ok_or_else(|| {
            quickjs_runtime::jsutils::JsError::new_str("Invalid or expired invocation context")
        })
    }
}

/// Legacy: opaque token issued by the host (used only for eval result tracking and backward compat).
/// Natives resolve scope from active context stack, not from this token.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct InvocationToken(pub(crate) String);

static INVOCATION_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_invocation_token() -> InvocationToken {
    let n = INVOCATION_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    InvocationToken(format!("inv-{}", n))
}

pub(crate) fn token_from_args(args: &[JsValueFacade]) -> Option<InvocationToken> {
    args.first().and_then(|first| {
        if !first.is_string() {
            return None;
        }
        let token = first.get_str().to_string();
        if token.is_empty() {
            None
        } else {
            Some(InvocationToken(token))
        }
    })
}

/// Resolve invocation scope from the host's active context stack (tokenless).
/// Optional: if args start with a valid token and registry has no current context, resolve from token map (legacy).
pub(crate) fn resolve_scope_from_active_context(
    registry: &Arc<StdMutex<InvocationContextRegistry>>,
    args: &[JsValueFacade],
    token_scope_map: &Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>,
) -> std::result::Result<(RuntimeScope, usize), quickjs_runtime::jsutils::JsError> {
    if let Ok(guard) = registry.lock()
        && let Ok(scope) = guard.current_scope()
    {
        return Ok((scope, 0));
    }
    // Legacy: first arg can be token string for backward compat
    if !args.is_empty()
        && let Some(token_js) = args.first()
        && token_js.is_string()
    {
        let s = token_js.get_str().to_string();
        if !s.is_empty() {
            if let Ok(guard) = token_scope_map.lock()
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
        "No invocation context (run inside a host invocation or pass a valid token)",
    ))
}

/// Resolve scope from active context only (strict tokenless). No legacy token fallback.
#[allow(dead_code)]
pub(crate) fn resolve_scope_from_active_context_only(
    registry: &Arc<StdMutex<InvocationContextRegistry>>,
) -> std::result::Result<RuntimeScope, quickjs_runtime::jsutils::JsError> {
    let guard = registry.lock().map_err(|_| {
        quickjs_runtime::jsutils::JsError::new_str("context registry lock poisoned")
    })?;
    guard.current_scope()
}

/// Legacy: resolve from token arg (used in tests and as fallback in resolve_scope_from_active_context).
#[allow(dead_code)]
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

pub(crate) async fn run_eval_with_scope(
    runtime: &quickjs_runtime::facades::QuickJsRuntimeFacade,
    scope: &baml_rt_core::context::InvocationScope,
    script: quickjs_runtime::jsutils::Script,
    clear_after: bool,
) -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
    let _ = scope;
    let _ = clear_after;
    runtime.eval(None, script).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
    use proptest::prelude::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex as StdMutex};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(8))]

        #[test]
        fn prop_invocation_tokens_unique(n in 1u32..64u32) {
            let tokens: HashSet<_> = (0..n).map(|_| next_invocation_token()).collect();
            assert_eq!(
                tokens.len(),
                n as usize,
                "next_invocation_token must yield distinct tokens"
            );
        }
    }

    fn test_scope(seed: u64) -> RuntimeScope {
        RuntimeScope::task_scope(
            ContextId::new(1700000000000 + seed, seed),
            AgentId::from_uuid(
                UuidId::parse_str("00000000-0000-0000-0000-000000000777").expect("valid test uuid"),
            ),
            MessageId::from_external(ExternalId::new(format!("msg-{seed}"))),
            TaskId::from_external(ExternalId::new(format!("task-{seed}"))),
        )
    }

    #[test]
    fn token_resolution_rejects_expired_token_without_worker_fallback() {
        let map: Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let args = vec![JsValueFacade::new_string("inv-expired".to_string())];

        let err = resolve_scope_from_token_arg(&map, &args).expect_err("expired token must fail");
        assert!(
            err.to_string()
                .contains("Invalid or expired invocation token"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn token_resolution_accepts_valid_token_map_entry() {
        let token_scope = test_scope(1);
        let token = InvocationToken("inv-123".to_string());
        let map: Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        map.lock()
            .expect("map lock")
            .insert(token.clone(), token_scope.clone());

        let args = vec![JsValueFacade::new_string(token.0.clone())];
        let (resolved, skip) =
            resolve_scope_from_token_arg(&map, &args).expect("valid token resolves");

        assert_eq!(skip, 1, "token arg should be consumed");
        assert_eq!(resolved.context_id(), token_scope.context_id());
        assert_eq!(resolved.message_id(), token_scope.message_id());
        assert_eq!(resolved.task_id_opt(), token_scope.task_id_opt());
    }

    #[test]
    fn token_resolution_rejects_missing_token() {
        let map: Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>> =
            Arc::new(StdMutex::new(HashMap::new()));

        let err = resolve_scope_from_token_arg(&map, &[]).expect_err("missing token must fail");
        assert!(
            err.to_string()
                .contains("Missing or invalid invocation token"),
            "unexpected error: {err:?}"
        );
    }
}

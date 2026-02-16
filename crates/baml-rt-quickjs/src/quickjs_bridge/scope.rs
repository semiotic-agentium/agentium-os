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

/// Opaque host token used for eval result tracking.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct InvocationToken(pub(crate) String);

static INVOCATION_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_invocation_token() -> InvocationToken {
    let n = INVOCATION_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    InvocationToken(format!("inv-{}", n))
}

/// Resolve invocation scope from the host's active context stack.
pub(crate) fn resolve_scope_from_active_context(
    registry: &Arc<StdMutex<InvocationContextRegistry>>,
) -> std::result::Result<RuntimeScope, quickjs_runtime::jsutils::JsError> {
    if let Ok(guard) = registry.lock()
        && let Ok(scope) = guard.current_scope()
    {
        return Ok(scope);
    }
    Err(quickjs_runtime::jsutils::JsError::new_str(
        "No invocation context (run inside an active host invocation)",
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
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn proptest_cfg(cases: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(cases);
        cfg.failure_persistence = None;
        cfg
    }

    proptest! {
        #![proptest_config(proptest_cfg(8))]

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
}

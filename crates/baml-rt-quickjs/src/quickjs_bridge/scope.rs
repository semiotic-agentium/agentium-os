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

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
};

use baml_rt_core::context::RuntimeScope;
use quickjs_runtime::values::JsValueFacade;

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

    /// Return the runtime scope's context_id for this invocation id, if present.
    /// Superseded by [`get_scope`] for task-scoped teardown but kept for potential future use.
    #[allow(dead_code)]
    pub(crate) fn get_context_id(
        &self,
        id: &InvocationContextId,
    ) -> Option<baml_rt_core::ids::ContextId> {
        self.by_id.get(id).map(|f| f.scope.context_id().clone())
    }

    /// Return the full runtime scope for this invocation id, if present.
    /// Used at teardown to extract both context_id and task_id for task-scoped session cleanup.
    pub(crate) fn get_scope(&self, id: &InvocationContextId) -> Option<RuntimeScope> {
        self.by_id.get(id).map(|f| f.scope.clone())
    }

    /// Exit the invocation for this id. Must match the id returned from [`enter`](Self::enter).
    pub(crate) fn exit(&mut self, id: &InvocationContextId) {
        // Remove from stack even if exits arrive out-of-order. This prevents
        // dangling ids in `stack` that point to missing frames in `by_id`.
        if let Some(pos) = self.stack.iter().rposition(|current| current == id) {
            if pos + 1 != self.stack.len() {
                tracing::warn!(
                    exited_id = %id.0,
                    depth = self.stack.len(),
                    pos = pos,
                    "Invocation context exited out of order; removing non-top frame"
                );
            }
            self.stack.remove(pos);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClearPolicy {
    Clear,
    Keep,
}

/// Resolve invocation scope from the host's active context stack.
pub(crate) fn resolve_scope_from_active_context(
    registry: &Arc<StdMutex<InvocationContextRegistry>>,
) -> std::result::Result<RuntimeScope, quickjs_runtime::jsutils::JsError> {
    let guard = registry.lock().map_err(|_| {
        quickjs_runtime::jsutils::JsError::new_str("No invocation context (registry lock poisoned)")
    })?;
    guard.current_scope().map_err(|e| {
        quickjs_runtime::jsutils::JsError::new_str(&format!(
            "No invocation context (run inside an active host invocation): {}",
            e
        ))
    })
}

/// Resolve invocation scope from the stream session map by session id.
///
/// Returns the session's `RuntimeScope` if the session exists and has not been
/// terminated (closed or cancelled). Returns a `JsError` otherwise so that
/// post-finalization callbacks get a clean rejected-promise instead of a crash.
pub(crate) fn resolve_scope_from_session(
    sessions: &super::StreamSessionMap,
    session_id: super::StreamSessionId,
) -> std::result::Result<
    (RuntimeScope, std::sync::Arc<super::StreamInvocationSession>),
    quickjs_runtime::jsutils::JsError,
> {
    let guard = sessions.lock().map_err(|_| {
        quickjs_runtime::jsutils::JsError::new_str("stream session map lock poisoned")
    })?;
    let session = guard.get(&session_id).cloned().ok_or_else(|| {
        quickjs_runtime::jsutils::JsError::new_str(&format!(
            "Stream session {} not found (already finalized or never created)",
            session_id
        ))
    })?;
    if session.is_terminated() {
        return Err(quickjs_runtime::jsutils::JsError::new_str(&format!(
            "Stream session {} has been cancelled or closed",
            session_id
        )));
    }
    let scope = session.scope.clone();
    Ok((scope, session))
}

pub(crate) async fn run_eval_with_scope(
    runtime: &quickjs_runtime::facades::QuickJsRuntimeFacade,
    scope: &baml_rt_core::context::InvocationScope,
    script: quickjs_runtime::jsutils::Script,
    clear_policy: ClearPolicy,
) -> std::result::Result<JsValueFacade, quickjs_runtime::jsutils::JsError> {
    let _ = scope;
    let _ = clear_policy;
    runtime.eval(None, script).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use baml_rt_core::{
        context::InvocationScope,
        ids::{AgentId, UuidId},
    };
    use proptest::prelude::*;

    use super::*;

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

    #[test]
    fn exit_out_of_order_does_not_leave_dangling_stack_ids() {
        let agent_a = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
        let agent_b = AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()));
        let scope_a = InvocationScope::synthetic_message(agent_a)
            .as_scope()
            .clone();
        let scope_b = InvocationScope::synthetic_message(agent_b)
            .as_scope()
            .clone();

        let mut registry = InvocationContextRegistry::new();
        let id_a = registry.enter(scope_a, None);
        let id_b = registry.enter(scope_b.clone(), None);

        // Simulate out-of-order exit (parent exits before child).
        registry.exit(&id_a);
        assert_eq!(
            registry
                .current_scope()
                .expect("top scope should remain valid"),
            scope_b
        );

        // After child exits, stack must be empty (no dangling removed ids).
        registry.exit(&id_b);
        let err = registry
            .current_scope()
            .expect_err("stack should be empty after both exits")
            .to_string();
        assert!(
            err.contains("No invocation context"),
            "expected empty-stack error, got: {}",
            err
        );
    }
}

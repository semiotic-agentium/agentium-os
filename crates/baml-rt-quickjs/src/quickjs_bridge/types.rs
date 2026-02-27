//! Shared types for the QuickJS bridge: eval lifecycle, stream sessions, and polling.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU32, Ordering},
    },
};

use baml_rt_core::context::RuntimeScope;
use baml_rt_tools::ToolStep;
use quickjs_runtime::facades::QuickJsRuntimeFacade;
use serde_json::Value;
use tokio::sync::{Mutex, Semaphore};

use super::scope::{InvocationContextId, InvocationContextRegistry, InvocationToken};
use crate::baml::BamlRuntimeManager;

// ---------- Type aliases ----------

pub(crate) type InvocationScopeMap = Arc<StdMutex<HashMap<InvocationToken, RuntimeScope>>>;
pub(crate) type CorrelationMap =
    Arc<StdMutex<HashMap<InvocationToken, baml_rt_core::ids::CorrelationId>>>;
pub(crate) type InvocationContextRegistrySlot = Arc<StdMutex<InvocationContextRegistry>>;
pub(crate) type EvalResultMap = Arc<StdMutex<HashMap<InvocationToken, Option<String>>>>;
/// When set for a token, __set_eval_result notifies so the poll loop can wait instead of relying on event-loop ordering.
pub(crate) type EvalNotifyMap = Arc<StdMutex<HashMap<InvocationToken, Arc<tokio::sync::Notify>>>>;
pub(crate) type StreamSemaphore = Arc<Semaphore>;
pub(crate) type StreamPermit = tokio::sync::OwnedSemaphorePermit;
pub(crate) type InFlightCounter = Arc<AtomicU32>;

// ---------- Stream session ----------

/// Host-only stream session identifier. Never exposed to JS; used for yield routing and finalization.
/// Each `invoke_js_function_stream` call allocates a unique id; session is looked up in `stream_sessions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamSessionId(pub(crate) u64);

impl std::fmt::Display for StreamSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stream-session-{}", self.0)
    }
}

/// Active stream invocation session.
///
/// Held in `QuickJSBridge::stream_sessions` for the lifetime of a single stream
/// invocation. Native callbacks clone the `Arc` to capture the session and resolve
/// scope, correlation id, and cancellation state. The permit is released when
/// the session is removed from the map (drop). The context is exited in
/// `finalize_a2a_stream_invocation` before removal.
pub(crate) struct StreamInvocationSession {
    #[allow(dead_code)]
    pub(crate) id: StreamSessionId,
    pub(crate) scope: RuntimeScope,
    pub(crate) correlation_id: Option<baml_rt_core::ids::CorrelationId>,
    pub(crate) cancel: tokio_util::sync::CancellationToken,
    pub(crate) closed: std::sync::atomic::AtomicBool,
    #[allow(dead_code)]
    pub(crate) permit: Option<StreamPermit>,
    pub(crate) context_id: Option<InvocationContextId>,
    pub(crate) context_tags: Option<HashMap<String, baml_types::BamlValue>>,
}

impl StreamInvocationSession {
    /// Check whether this session has been finalized or cancelled.
    pub(crate) fn is_terminated(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.cancel.is_cancelled()
    }
}

pub(crate) type StreamSessionMap =
    Arc<StdMutex<HashMap<StreamSessionId, Arc<StreamInvocationSession>>>>;

// ---------- In-flight guard ----------

/// RAII guard that decrements the in-flight counter when dropped.
///
/// Used inside `JsValueFacade::new_promise` async bodies so the counter is
/// decremented even on panic/cancellation. Caller must increment (fetch_add) before creating the guard.
pub(crate) struct InFlightGuard(pub(crate) InFlightCounter);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

// ---------- Eval lifecycle ----------

/// Ensures evaluate() bookkeeping is cleaned up on all exits, including cancellation/drop.
/// Also triggers tool-session teardown for this context (spawned task) so sessions are not leaked.
pub(crate) struct EvalLifecycleGuard {
    pub(crate) eval_results_by_token: EvalResultMap,
    pub(crate) invocation_context_registry: InvocationContextRegistrySlot,
    pub(crate) eval_token: InvocationToken,
    pub(crate) context_id_to_exit: Option<InvocationContextId>,
    pub(crate) eval_slot_registered: bool,
    pub(crate) baml_manager: Arc<Mutex<BamlRuntimeManager>>,
}

impl EvalLifecycleGuard {
    pub(crate) fn new(
        eval_results_by_token: EvalResultMap,
        invocation_context_registry: InvocationContextRegistrySlot,
        eval_token: InvocationToken,
        context_id_to_exit: Option<InvocationContextId>,
        baml_manager: Arc<Mutex<BamlRuntimeManager>>,
    ) -> Self {
        Self {
            eval_results_by_token,
            invocation_context_registry,
            eval_token,
            context_id_to_exit,
            eval_slot_registered: false,
            baml_manager,
        }
    }

    pub(crate) fn mark_eval_slot_registered(&mut self) {
        self.eval_slot_registered = true;
    }
}

impl Drop for EvalLifecycleGuard {
    fn drop(&mut self) {
        if self.eval_slot_registered
            && let Ok(mut guard) = self.eval_results_by_token.lock()
        {
            guard.remove(&self.eval_token);
        }

        if let Some(ref id) = self.context_id_to_exit {
            let runtime_cid = self
                .invocation_context_registry
                .lock()
                .ok()
                .and_then(|reg| reg.get_context_id(id));
            if let Some(cid) = runtime_cid {
                let baml = self.baml_manager.clone();
                tokio::spawn(async move {
                    let mgr = baml.lock().await;
                    let _ = mgr.close_sessions_for_context(&cid).await;
                });
            }
        }

        if let Some(id) = self.context_id_to_exit.as_ref()
            && let Ok(mut guard) = self.invocation_context_registry.lock()
        {
            guard.exit(id);
        }
    }
}

// ---------- Brief poll (resume path) ----------

/// Params for the promise-poll loop when using runtime without bridge lock (deadlock-free resume path).
/// Caller holds `lifecycle_guard` until after poll completes so context is exited on drop.
/// When `result_notify` is set, the poll loop waits on it so the result is observed only after
/// __set_eval_result has run (strict ordering; no event-loop race).
pub(crate) struct BriefPollParams {
    pub(crate) runtime: Arc<QuickJsRuntimeFacade>,
    pub(crate) eval_results_by_token: EvalResultMap,
    pub(crate) eval_token: InvocationToken,
    pub(crate) result_notify: Arc<tokio::sync::Notify>,
    pub(crate) eval_notify_by_token: EvalNotifyMap,
    pub(crate) invocation_scope_by_token: InvocationScopeMap,
    pub(crate) scope: baml_rt_core::context::InvocationScope,
    pub(crate) effect_liveness: Option<Arc<dyn baml_rt_core::bus::EffectLiveness>>,
    pub(crate) idle_timeout_ms: u64,
    pub(crate) max_attempts_ms: u64,
    #[allow(dead_code)]
    pub(crate) lifecycle_guard: EvalLifecycleGuard,
}

impl Drop for BriefPollParams {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.eval_notify_by_token.lock() {
            guard.remove(&self.eval_token);
        }
    }
}

/// Result of running eval once: either sync value or params to poll with brief locks.
pub(crate) enum EvalOnceResult {
    Sync(Value),
    PromisePending(BriefPollParams),
}

/// Prepared eval for brief-poll path; run without holding the bridge lock so the worker
/// and TaskManager can make progress (fixes resume deadlock).
pub(crate) struct PreparedBriefPollEval {
    pub(crate) direct_script: quickjs_runtime::jsutils::Script,
    pub(crate) scope: baml_rt_core::context::InvocationScope,
    pub(crate) runtime: Arc<QuickJsRuntimeFacade>,
    pub(crate) eval_token: InvocationToken,
    pub(crate) lifecycle_guard: EvalLifecycleGuard,
    pub(crate) result_notify: Arc<tokio::sync::Notify>,
    pub(crate) eval_results_by_token: EvalResultMap,
    pub(crate) eval_notify_by_token: EvalNotifyMap,
    pub(crate) invocation_scope_by_token: InvocationScopeMap,
    pub(crate) effect_liveness: Option<Arc<dyn baml_rt_core::bus::EffectLiveness>>,
    pub(crate) idle_timeout_ms: u64,
    pub(crate) max_attempts_ms: u64,
}

// ---------- Helpers used from bridge ----------

/// Helper for creating an empty open_input value.
pub(crate) fn empty_open_input() -> Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Convert a tool step to a JSON value for JS.
pub(crate) fn tool_step_to_value(step: ToolStep) -> Value {
    match step {
        ToolStep::Streaming { output } => {
            serde_json::json!({ "status": "streaming", "output": output })
        }
        ToolStep::Suspended { output } => {
            serde_json::json!({ "status": "suspended", "output": output })
        }
        ToolStep::Done { output } => serde_json::json!({ "status": "done", "output": output }),
        ToolStep::Error { error } => serde_json::json!({
            "status": "error",
            "error": {
                "kind": format!("{:?}", error.kind),
                "message": error.message,
                "retryable": bool::from(error.retryability)
            }
        }),
    }
}

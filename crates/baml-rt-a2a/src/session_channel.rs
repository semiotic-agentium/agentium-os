//! Channel-based A2A session dispatcher and runtime worker.
//!
//! All coordination is explicit message passing. Context (scope) is carried in messages only.
//! See `docs/INVARIANTS_AND_LIVENESS.md` for properties and `tests/session_channel_property_test.rs` for tests.
//!
//! ## Invariants (summary)
//!
//! - **Dispatcher:** ∀ session_id: at most one (scope, response_tx) in map; Register before Cmd(Send); Finish/Abort removes session.
//! - **Worker:** Every HandleA2a runs with_scope(msg.scope, ...); exactly one response (stream of values or error) per message.

use crate::A2aRequestHandler;
use baml_rt_core::context::RuntimeScope;
use baml_rt_observability::{metrics, spans};
use baml_rt_tools::ToolSessionId;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Commands for a single session (send request, finish, abort).
#[derive(Debug)]
pub enum SessionCmd {
    Send(Value),
    Finish,
    Abort(Option<String>),
}

/// Message to the session dispatcher. Single ordered channel: Register before Cmd for that session.
#[derive(Debug)]
pub enum DispatcherMsg {
    /// INVARIANT: Sent exactly once per session at open. Dispatcher stores (scope, response_tx) by session_id.
    Register {
        session_id: ToolSessionId,
        scope: RuntimeScope,
        response_tx: mpsc::UnboundedSender<Value>,
    },
    Cmd {
        session_id: ToolSessionId,
        cmd: SessionCmd,
    },
}

/// Message to the runtime worker (bridge owner). Context is in the message; no task-local.
#[derive(Debug)]
pub enum RuntimeWorkerMsg {
    /// Worker runs with_scope(scope, handler.handle_a2a(request)) and sends results on response_tx.
    HandleA2a {
        scope: RuntimeScope,
        request: Value,
        response_tx: mpsc::UnboundedSender<Value>,
    },
}

/// Runtime worker loop: a tokio task. Receives HandleA2a and delegates to
/// `handler.run_handle_a2a(...)`, which enqueues work and posts a fire-and-forget
/// bridge drain on the QuickJS worker event loop.
pub async fn run_runtime_worker(
    handler: Arc<dyn A2aRequestHandler>,
    mut rx: mpsc::UnboundedReceiver<RuntimeWorkerMsg>,
) {
    let span = spans::session_runtime_worker();
    let _guard = span.enter();
    while let Some(msg) = rx.recv().await {
        match msg {
            RuntimeWorkerMsg::HandleA2a {
                scope,
                request,
                response_tx,
            } => {
                let start = Instant::now();
                let outcome = handler.run_handle_a2a(scope, request).await;
                let duration = start.elapsed();
                let result_str = if outcome.is_ok() { "success" } else { "error" };
                metrics::record_a2a_worker_handle(result_str, duration);
                match outcome {
                    Ok(responses) => {
                        for v in responses {
                            if response_tx.send(v).is_err() {
                                tracing::warn!(
                                    "A2A session response channel closed before send; client likely dropped"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let err_value = serde_json::json!({ "error": e.to_string() });
                        if response_tx.send(err_value).is_err() {
                            tracing::warn!(
                                error = ?e,
                                "A2A session error response channel closed; client likely dropped"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Runs the session dispatcher loop. Spawn with `tokio::spawn(run_dispatcher(rx, worker_tx))`.
/// INVARIANT: For each session_id, at most one entry in map; Register before Cmd(Send); Finish/Abort removes.
pub async fn run_dispatcher(
    mut rx: mpsc::UnboundedReceiver<DispatcherMsg>,
    worker_tx: mpsc::UnboundedSender<RuntimeWorkerMsg>,
) {
    let span = spans::session_dispatcher();
    let _guard = span.enter();
    let mut map: HashMap<ToolSessionId, (RuntimeScope, mpsc::UnboundedSender<Value>)> =
        HashMap::new();

    while let Some(msg) = rx.recv().await {
        match msg {
            DispatcherMsg::Register {
                session_id,
                scope,
                response_tx,
            } => {
                map.insert(session_id, (scope, response_tx));
            }
            DispatcherMsg::Cmd { session_id, cmd } => match cmd {
                SessionCmd::Send(request) => {
                    if let Some((scope, response_tx)) = map.get(&session_id) {
                        let scope = scope.clone();
                        let response_tx = response_tx.clone();
                        if worker_tx
                            .send(RuntimeWorkerMsg::HandleA2a {
                                scope,
                                request,
                                response_tx,
                            })
                            .is_err()
                        {
                            tracing::warn!(
                                session_id = %session_id,
                                "A2A runtime worker channel closed; dispatcher shutting down"
                            );
                        }
                    }
                }
                SessionCmd::Finish => {
                    map.remove(&session_id);
                }
                SessionCmd::Abort(reason) => {
                    tracing::debug!(session_id = %session_id, reason = ?reason, "A2A session aborted");
                    map.remove(&session_id);
                }
            },
        }
    }
}

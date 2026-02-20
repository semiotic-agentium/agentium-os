//! Type-safe A2A stream protocol: yield-buffer setup → invoke → collect.
//!
//! This module encodes the three-phase protocol so that the host cannot call
//! collect before invoke, or invoke before setup. The session is a linear type:
//! each step consumes the previous state and produces the next.
//!
//! ## Semi-formal liveness properties
//!
//! Let □ = "always", ◇ = "eventually", and "→" denote ordering.
//!
//! - **L1 (Session progress)**
//!   □(begin_a2a_yield_session returns Ok(s) → ◇(invoke(s) is called))
//!   If setup succeeds, the caller must eventually call invoke on the session.
//!
//! - **L2 (Invoke progress)**
//!   □(invoke(s) returns Ok(s′) → ◇(collect(s′) is called))
//!   If invoke succeeds, the caller must eventually call collect.
//!
//! - **L3 (Collect terminates)**
//!   □(collect(s) is called → ◇(collect(s) returns))
//!   Collect always returns in finite time (bounded by JSON parse and buffer read).
//!
//! - **L4 (Promise resolution for non-stream)**
//!   For non-stream requests, the JS promise eventually resolves or the evaluate loop hits MAX_ATTEMPTS.
//!
//! - **L6 (Stream Promise Non-Termination)**
//!   For stream requests, the promise from `onChatMessage()` is DESIGNED to never resolve.
//!   It yields chunks via `__baml_chat_yield()` and only completes on agent exit or crash.
//!   `invoke_js_function_stream()` starts the function but does NOT wait for promise resolution.
//!
//! **CG4 (Stream Promise Non-Resolution):** The type parameter `P` encodes promise semantics;
//! `NonResolvingPromise` ensures only stream invocation (no wait for resolution) is used.

use std::time::{Duration, Instant};

use baml_rt_core::{
    Result,
    context::InvocationScope,
    stream_completion::{StreamCompletion, StreamResult},
};
use serde_json::Value;
use tokio::{sync::mpsc::UnboundedReceiver, time::sleep};

use crate::quickjs_bridge::{BufferDrain, QuickJSBridge, StreamSessionId};

/// State marker: yield buffer is installed and ready for one stream invocation.
pub struct YieldBufferReady;

/// State marker: onChatMessage has been invoked (promise intentionally not awaited).
pub struct InvocationComplete;

/// **INVARIANT L6 / CG4:** Marker type encoding that the JS promise never resolves for this session.
/// Stream handlers yield via `__baml_chat_yield()`; the host must never wait on promise resolution.
#[derive(Debug, Clone, Copy)]
pub struct NonResolvingPromise;

/// Session in ready phase: yield buffer installed; invoke not yet called.
///
/// **Invariant:** The same `&mut QuickJSBridge` is held for the entire session (CG1: single writer).
pub struct A2aYieldSessionReady<'a> {
    pub(super) bridge: &'a mut QuickJSBridge,
}

/// Session in complete phase: stream invoked; collect may be called.
///
/// Holds `session_id` and `yield_rx` so invalid state (collect without invoke) is unrepresentable.
pub struct A2aYieldSessionComplete<'a> {
    pub(super) bridge: &'a mut QuickJSBridge,
    pub(super) session_id: StreamSessionId,
    pub(super) yield_rx: UnboundedReceiver<Value>,
}

/// Begins a stream session: installs the yield buffer and returns a session in ready state.
///
/// **Liveness (L1):** Caller must eventually call [`A2aYieldSessionReady::invoke`] on the returned session.
#[inline]
pub async fn begin_a2a_yield_session(
    bridge: &mut QuickJSBridge,
) -> Result<A2aYieldSessionReady<'_>> {
    bridge.setup_a2a_yield_buffer().await?;
    Ok(A2aYieldSessionReady { bridge })
}

impl<'a> A2aYieldSessionReady<'a> {
    /// Invokes `onChatMessage` with the given chat message payload. The JS handler must use
    /// `__baml_chat_yield(chunk)`; the return value is ignored.
    ///
    /// **Liveness (L2):** Caller must eventually call [`A2aYieldSessionComplete::collect`] on the returned session.
    ///
    /// **INVARIANT L6 / CG4:** The promise never resolves; this method uses `invoke_js_function_stream()`
    /// and does NOT wait for promise resolution. Chunks are collected via the yield buffer.
    pub async fn invoke(
        self,
        scope: &InvocationScope,
        request: Value,
    ) -> Result<A2aYieldSessionComplete<'a>> {
        let (session_id, yield_rx) = self
            .bridge
            .invoke_js_function_stream(scope, "onChatMessage", request)
            .await?;
        Ok(A2aYieldSessionComplete {
            bridge: self.bridge,
            session_id,
            yield_rx,
        })
    }
}

/// Reads task state from a stream chunk. Supports both object shape (yield buffer) and
/// stringified JSON (tool/serialization boundary); treats parse failure as missing state.
fn chunk_state(chunk: &Value) -> Option<String> {
    fn from_val(val: &Value) -> Option<String> {
        val.get("status")
            .and_then(|s| s.get("state"))
            .and_then(Value::as_str)
            .map(String::from)
    }
    fn from_maybe_string(val: &Value) -> Option<String> {
        from_val(val).or_else(|| {
            val.as_str().and_then(|s| {
                match serde_json::from_str::<Value>(s) {
                    Ok(parsed) => from_val(&parsed),
                    Err(e) => {
                        tracing::trace!(error = %e, "chunk_state: stringified task/statusUpdate parse failed");
                        None
                    }
                }
            })
        })
    }
    chunk
        .get("task")
        .and_then(from_maybe_string)
        .or_else(|| chunk.get("statusUpdate").and_then(from_maybe_string))
}

fn chunk_has_final_state(chunk: &Value) -> bool {
    if chunk.get("final").and_then(Value::as_bool).unwrap_or(false) {
        return true;
    }
    matches!(
        chunk_state(chunk).as_deref(),
        Some("TASK_STATE_COMPLETED") | Some("TASK_STATE_FAILED")
    )
}

fn chunk_has_input_required_state(chunk: &Value) -> bool {
    matches!(
        chunk_state(chunk).as_deref(),
        Some("TASK_STATE_INPUT_REQUIRED")
    )
}

impl<'a> A2aYieldSessionComplete<'a> {
    /// Reads and clears the yield buffer. Returns chunks and an explicit completion reason.
    ///
    /// Terminates only on: (1) semantic final chunk (TASK_STATE_COMPLETED/FAILED),
    /// (2) channel closed (sender dropped), or (3) safety timeout. No quiescence heuristic.
    ///
    /// **Liveness (L3):** This method returns in finite time.
    pub async fn collect(mut self) -> Result<StreamResult> {
        let start = Instant::now();
        let idle_timeout = Duration::from_secs(60);
        let active_timeout = Duration::from_secs(300);
        let interval = Duration::from_millis(50);
        let read_timeout = Duration::from_secs(2);
        let mut all = Vec::new();

        loop {
            let drain: BufferDrain = match tokio::time::timeout(
                read_timeout,
                self.bridge.drain_yield_buffer(&mut self.yield_rx),
            )
            .await
            {
                Ok(Ok(d)) => d,
                Ok(Err(e)) => {
                    tracing::error!(
                        error = ?e,
                        "a2a stream buffer read failed; finalizing stream invocation state"
                    );
                    self.bridge
                        .finalize_a2a_stream_invocation(self.session_id)
                        .await;
                    return Err(e);
                }
                Err(_) => BufferDrain {
                    chunks: vec![],
                    channel_closed: false,
                },
            };
            if !drain.chunks.is_empty() {
                all.extend(drain.chunks);
                let has_input_req = all.iter().any(chunk_has_input_required_state);
                let has_final = all.iter().any(chunk_has_final_state);
                tracing::trace!(
                    chunk_count = all.len(),
                    has_input_required = has_input_req,
                    has_final = has_final,
                    "a2a stream collect: drain extended"
                );
                // Prefer suspension over final: if both INPUT_REQUIRED and COMPLETED appear
                // (e.g. same batch or ordering), stop at INPUT_REQUIRED so the client can resume.
                if has_input_req {
                    self.bridge
                        .finalize_a2a_stream_invocation(self.session_id)
                        .await;
                    return Ok(StreamResult {
                        chunks: all,
                        completion: StreamCompletion::InputRequired,
                    });
                }
                if has_final {
                    self.bridge
                        .finalize_a2a_stream_invocation(self.session_id)
                        .await;
                    return Ok(StreamResult {
                        chunks: all,
                        completion: StreamCompletion::SemanticFinal,
                    });
                }
            }
            if drain.channel_closed {
                self.bridge
                    .finalize_a2a_stream_invocation(self.session_id)
                    .await;
                return Ok(StreamResult {
                    chunks: all,
                    completion: StreamCompletion::ChannelClosed,
                });
            }
            let elapsed = start.elapsed();
            let in_flight = self.bridge.in_flight_invoke_count();
            let timeout_budget = if in_flight > 0 {
                active_timeout
            } else {
                idle_timeout
            };
            if elapsed >= timeout_budget {
                tracing::warn!(
                    elapsed_ms = elapsed.as_millis() as u64,
                    timeout_ms = timeout_budget.as_millis() as u64,
                    in_flight,
                    chunk_count = all.len(),
                    "a2a stream collector timeout reached"
                );
                self.bridge
                    .finalize_a2a_stream_invocation(self.session_id)
                    .await;
                return Ok(StreamResult {
                    chunks: all,
                    completion: StreamCompletion::Timeout,
                });
            }
            sleep(interval).await;
        }
    }
}

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

use crate::quickjs_bridge::QuickJSBridge;
use baml_rt_core::Result;
use baml_rt_core::context::InvocationScope;
use serde_json::Value;
use std::marker::PhantomData;
use std::time::{Duration, Instant};
use tokio::time::sleep;

fn is_tool_event_chunk(value: &Value) -> bool {
    let Some(event) = value.get("event").and_then(|v| v.as_object()) else {
        return false;
    };
    let Some(source) = event.get("source").and_then(|v| v.as_str()) else {
        return false;
    };
    if source != "runtime" {
        return false;
    }
    let Some(event_type) = event.get("type").and_then(|v| v.as_str()) else {
        return false;
    };
    event_type.starts_with("tool_execution")
}

fn is_output_chunk(value: &Value) -> bool {
    if value.get("message").is_some() {
        return true;
    }
    if value.get("artifactUpdate").is_some() {
        return true;
    }
    if let Some(status) = value
        .get("statusUpdate")
        .and_then(|v| v.get("status"))
        .and_then(|v| v.get("state"))
        .and_then(|v| v.as_str())
        && matches!(
            status,
            "TASK_STATE_COMPLETED"
                | "TASK_STATE_FAILED"
                | "TASK_STATE_REJECTED"
                | "TASK_STATE_CANCELED"
        )
    {
        return true;
    }
    if let Some(task) = value.get("task").and_then(|v| v.as_object())
        && let Some(status) = task.get("status").and_then(|v| v.as_object())
    {
        if let Some(state) = status.get("state").and_then(|v| v.as_str())
            && matches!(
                state,
                "TASK_STATE_COMPLETED"
                    | "TASK_STATE_FAILED"
                    | "TASK_STATE_REJECTED"
                    | "TASK_STATE_CANCELED"
            )
        {
            return true;
        }
        if status.get("message").is_some() {
            return true;
        }
    }
    false
}

/// State marker: yield buffer is installed and ready for one stream invocation.
pub struct YieldBufferReady;

/// State marker: onChatMessage has been invoked (promise intentionally not awaited).
pub struct InvocationComplete;

/// **INVARIANT L6 / CG4:** Marker type encoding that the JS promise never resolves for this session.
/// Stream handlers yield via `__baml_chat_yield()`; the host must never wait on promise resolution.
#[derive(Debug, Clone, Copy)]
pub struct NonResolvingPromise;

/// Type-safe session for a single A2A stream request.
///
/// Type parameters:
/// - `S`: phase ([`YieldBufferReady`] or [`InvocationComplete`]).
/// - `P`: promise semantics; only [`NonResolvingPromise`] is used (stream promise never resolves).
///
/// The session is linear: each method consumes `self` and returns the next state (or the result).
///
/// **Invariant:** The same `&mut QuickJSBridge` is held for the entire session (CG1: single writer).
pub struct A2aYieldSession<'a, S, P = NonResolvingPromise> {
    pub(super) bridge: &'a mut QuickJSBridge,
    _state: PhantomData<(S, P)>,
}

/// Begins a stream session: installs the yield buffer and returns a session in [`YieldBufferReady`] state.
/// The session is typed with [`NonResolvingPromise`] so only stream invocation (no promise wait) is used.
///
/// **Liveness (L1):** Caller must eventually call [`A2aYieldSession::invoke`] on the returned session.
#[inline]
pub async fn begin_a2a_yield_session(
    bridge: &mut QuickJSBridge,
) -> Result<A2aYieldSession<'_, YieldBufferReady, NonResolvingPromise>> {
    bridge.setup_a2a_yield_buffer().await?;
    Ok(A2aYieldSession {
        bridge,
        _state: PhantomData,
    })
}

impl<'a> A2aYieldSession<'a, YieldBufferReady, NonResolvingPromise> {
    /// Invokes `onChatMessage` with the given chat message payload. The JS handler must use
    /// `__baml_chat_yield(chunk)`; the return value is ignored.
    ///
    /// **Liveness (L2):** Caller must eventually call [`collect`](A2aYieldSession::collect) on the returned session.
    ///
    /// **INVARIANT L6 / CG4:** The promise never resolves; this method uses `invoke_js_function_stream()`
    /// and does NOT wait for promise resolution. Chunks are collected via the yield buffer.
    pub async fn invoke(
        self,
        scope: &InvocationScope,
        request: Value,
    ) -> Result<A2aYieldSession<'a, InvocationComplete, NonResolvingPromise>> {
        self.bridge
            .invoke_js_function_stream(scope, "onChatMessage", request)
            .await?;
        Ok(A2aYieldSession {
            bridge: self.bridge,
            _state: PhantomData,
        })
    }
}

impl<'a, P> A2aYieldSession<'a, InvocationComplete, P> {
    /// Reads and clears the yield buffer. Returns the sequence of chunks yielded via
    /// `__baml_chat_yield` during the preceding invoke.
    ///
    /// **Liveness (L3):** This method returns in finite time. It polls briefly to allow
    /// async handlers to yield before the buffer is read.
    pub async fn collect(self) -> Result<Vec<Value>> {
        let start = Instant::now();
        let timeout = Duration::from_secs(120);
        let interval = Duration::from_millis(50);
        let read_timeout = Duration::from_secs(2);
        let settle_duration = Duration::from_millis(1000);
        let mut collected: Vec<Value> = Vec::new();
        let mut last_nonempty: Option<Instant> = None;
        let mut saw_output = false;

        loop {
            // Liveness guard: a single buffer-read must not stall collection forever.
            let responses = match tokio::time::timeout(
                read_timeout,
                self.bridge.get_a2a_yield_buffer(),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => Vec::new(),
            };
            if !responses.is_empty() {
                let has_signal = responses.iter().any(|v| !is_tool_event_chunk(v));
                let has_output = responses.iter().any(is_output_chunk);
                if has_output {
                    saw_output = true;
                }
                collected.extend(responses);
                if has_signal {
                    last_nonempty = Some(Instant::now());
                } else if last_nonempty.is_none() {
                    // Tool-heavy streams should still settle after terminal output.
                    last_nonempty = Some(Instant::now());
                }
            }
            if saw_output
                && let Some(last) = last_nonempty
                && last.elapsed() >= settle_duration
            {
                self.bridge.finalize_a2a_stream_invocation();
                return Ok(collected);
            }
            if start.elapsed() >= timeout {
                self.bridge.finalize_a2a_stream_invocation();
                return Ok(collected);
            }
            sleep(interval).await;
        }
    }
}

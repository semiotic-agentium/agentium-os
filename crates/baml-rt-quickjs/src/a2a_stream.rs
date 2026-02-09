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
//!   For stream requests, the promise from `handle_a2a_request()` is DESIGNED to never resolve.
//!   It yields chunks via `__baml_a2a_yield()` and only completes on agent exit or crash.
//!   `invoke_js_function_stream()` starts the function but does NOT wait for promise resolution.
//!
//! **CG4 (Stream Promise Non-Resolution):** The type parameter `P` encodes promise semantics;
//! `NonResolvingPromise` ensures only stream invocation (no wait for resolution) is used.

use crate::quickjs_bridge::QuickJSBridge;
use baml_rt_core::Result;
use baml_rt_core::context::InvocationScope;
use serde_json::Value;
use std::marker::PhantomData;

/// State marker: yield buffer is installed and ready for one stream invocation.
pub struct YieldBufferReady;

/// State marker: handle_a2a_request has been invoked (promise intentionally not awaited).
pub struct InvocationComplete;

/// **INVARIANT L6 / CG4:** Marker type encoding that the JS promise never resolves for this session.
/// Stream handlers yield via `__baml_a2a_yield()`; the host must never wait on promise resolution.
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
    /// Invokes `handle_a2a_request` with the given request. The JS handler must use
    /// `__baml_a2a_yield(chunk)`; the return value is ignored.
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
            .invoke_js_function_stream(scope, "handle_a2a_request", request)
            .await?;
        Ok(A2aYieldSession {
            bridge: self.bridge,
            _state: PhantomData,
        })
    }
}

impl<'a, P> A2aYieldSession<'a, InvocationComplete, P> {
    /// Reads and clears the yield buffer. Returns the sequence of chunks yielded via
    /// `__baml_a2a_yield` during the preceding invoke.
    ///
    /// **Liveness (L3):** This method returns in finite time.
    pub async fn collect(self) -> Result<Vec<Value>> {
        self.bridge.get_a2a_yield_buffer().await
    }
}

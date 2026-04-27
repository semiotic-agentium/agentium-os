//! `SessionToolInvoker` — transport-abstract interface for the session-mode
//! sandbox protocol from `plans/sandbox_streaming.md` §5.2.
//!
//! Counterpart to [`super::ExternalInvoker`]. Where `ExternalInvoker` is the
//! single-shot `tool/invoke` surface, `SessionToolInvoker` exposes the
//! `tool/session_*` methods with strongly-typed request/response shapes. The
//! concrete pool-backed implementation lives in
//! [`super::sandbox::session_invoker::SandboxSessionInvoker`]; the trait is
//! stand-alone so test fixtures can supply mocks without depending on it.

use std::time::Duration;

use async_trait::async_trait;
use baml_rt_core::Result;
use serde_json::Value;

use crate::{ToolName, tool_fsm::ToolStep};

/// Request for `tool/session_open`.
#[derive(Debug, Clone)]
pub struct SessionOpenRequest {
    pub tool_name: ToolName,
    pub invocation_id: String,
    pub open_input: Value,
    /// Provided when the metadata declares `secret_scope=session`.
    pub secrets: serde_json::Map<String, Value>,
    pub capabilities: Value,
    pub timeout: Duration,
}

/// Response for `tool/session_open`.
#[derive(Debug, Clone)]
pub struct SessionOpenResponse {
    /// Adapter-supplied session id; opaque to the host.
    pub session_id: String,
    /// Optional first step the adapter produced synchronously at open time.
    pub initial_step: Option<ToolStep>,
}

/// Request for `tool/session_send`. `resume_token` is required iff the last
/// observed step for the session was [`ToolStep::Suspended`].
#[derive(Debug, Clone)]
pub struct SessionSendRequest {
    pub session_id: String,
    pub input: Value,
    pub resume_token: Option<String>,
    /// Provided when the metadata declares `secret_scope=send` (the default).
    pub secrets: serde_json::Map<String, Value>,
    pub timeout: Duration,
}

/// Request for `tool/session_read`. Wire is parameterless beyond the session id;
/// `chunk_timeout` controls the read deadline.
#[derive(Debug, Clone)]
pub struct SessionReadRequest {
    pub session_id: String,
    pub chunk_timeout: Duration,
}

/// Request for `tool/session_finish`.
#[derive(Debug, Clone)]
pub struct SessionFinishRequest {
    pub session_id: String,
    pub timeout: Duration,
}

/// Request for `tool/session_abort`.
#[derive(Debug, Clone)]
pub struct SessionAbortRequest {
    pub session_id: String,
    pub reason: Option<String>,
    pub timeout: Duration,
}

/// Transport-abstract interface for the session-mode external tool protocol.
///
/// The pool-backed implementation lives in
/// [`super::sandbox::session_invoker::SandboxSessionInvoker`]. Test fixtures
/// can supply mock implementations without taking that dependency.
///
/// The reuse-after-finish reset hook (`tool/session_reset`) is *not* exposed
/// here — it is an implementation detail of the pool-backed invoker, fired
/// internally between `session_finish` and the entry's return to `Idle`.
#[async_trait]
pub trait SessionToolInvoker: Send + Sync {
    async fn session_open(&self, req: SessionOpenRequest) -> Result<SessionOpenResponse>;
    async fn session_send(&self, req: SessionSendRequest) -> Result<()>;
    async fn session_read(&self, req: SessionReadRequest) -> Result<ToolStep>;
    async fn session_finish(&self, req: SessionFinishRequest) -> Result<()>;
    async fn session_abort(&self, req: SessionAbortRequest) -> Result<()>;
}

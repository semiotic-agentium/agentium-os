//! The runtime's only entry point into sandbox infrastructure.
//!
//! Per `tool_sandbox.md` §17 decision #10: "`SandboxProvider` trait is the
//! runtime's only entrypoint to sandbox infrastructure." Concrete impls
//! (microsandbox, mock, future docker) plug in here so the runtime can treat
//! them interchangeably — important because microsandbox is a beta crate and
//! the trait boundary isolates the runtime from its churn (§7.3 caveat).

use async_trait::async_trait;
use baml_rt_core::Result;
use futures_util::stream::BoxStream;

use super::{
    channel::TsrpcChannel,
    spec::{SandboxEvent, SandboxHandle, SandboxSpec},
};

/// Sandbox lifecycle + transport surface. See `tool_sandbox.md` §7.3.
///
/// **Contract notes:**
/// - `create` is idempotent by name-replace: re-creating with an existing
///   name should either return the live handle or tear down + recreate.
///   Providers document their choice; runtime callers should not rely on
///   "not found" errors.
/// - `teardown` is best-effort. Providers must not raise on "already gone"
///   states — return `Ok(())`.
/// - `events` is a long-lived stream; the runtime drops it on teardown.
/// - `list_owned` / `reattach` are scoped to the **current process**
///   (§9.4 decision: in-process reattach only in v1).
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Create a new sandbox. Returns a handle the runtime caches and passes
    /// to the other methods.
    async fn create(&self, spec: SandboxSpec) -> Result<SandboxHandle>;

    /// Open a TSRPC channel (length-prefixed JSON over `exec_stream` stdio
    /// in the microsandbox case — §5.2) to the tool-adapter inside the
    /// sandbox identified by `handle`.
    async fn rpc_channel(&self, handle: &SandboxHandle) -> Result<TsrpcChannel>;

    /// Tear down. Best-effort; safe to call on an already-gone sandbox.
    async fn teardown(&self, handle: &SandboxHandle) -> Result<()>;

    /// Lifecycle event stream for observability. Non-blocking: drop the
    /// stream to unsubscribe.
    fn events(&self, handle: &SandboxHandle) -> BoxStream<'_, SandboxEvent>;

    /// Enumerate sandboxes the current process owns, filtered by the
    /// runner-id-prefixed naming convention (§9.2). Used for hot reload,
    /// liveness checks, and test cache rebuilds.
    async fn list_owned(&self, runner_id: &str) -> Result<Vec<SandboxHandle>>;

    /// Reattach to a named sandbox and return its handle. Callers MUST run
    /// the §9.4 reattach validation checklist (status / context / digest /
    /// policy / age) before trusting it.
    async fn reattach(&self, name: &str) -> Result<SandboxHandle>;
}

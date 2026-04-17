//! Sandbox backend (`tool_sandbox.md` Workstream B).
//!
//! Layout:
//! - [`spec`] — value types (`SandboxSpec`, `SandboxHandle`, events, policy).
//! - [`provider`] — the `SandboxProvider` trait.
//! - [`channel`] — length-prefixed JSON codec over `exec_stream` stdio (§5.2).
//! - [`microsandbox_provider`] — thin wrapper over the `microsandbox` crate
//!   (behind the `sandbox-provider` feature).
//! - [`mock`] — in-memory provider for tests + fixture-driven runs.
//! - [`invoker`] — `SandboxInvoker: ToolInvoker` + `SandboxCache`.
//!
//! Nothing in this module dispatches a tool on its own — the runner wires
//! [`SandboxInvoker`] into the resolver/handler in Workstream B step 6
//! (`runtime.kind == "sandbox"` branch).

pub mod channel;
pub mod handler;
pub mod invoker;
pub mod microsandbox_provider;
pub mod mock;
pub mod provider;
pub mod spec;
pub mod wiring;

#[cfg(test)]
mod tests;

pub use channel::{MAX_FRAME_BYTES, TsrpcChannel};
pub use handler::{SandboxToolHandler, SandboxToolSession};
pub use invoker::{SandboxCache, SandboxCacheKey, SandboxInvoker, SandboxSpecBuilder};
pub use microsandbox_provider::MicrosandboxProvider;
pub use mock::{MockSandboxProvider, ScriptedAdapter, test_durations};
pub use provider::SandboxProvider;
pub use spec::{
    Destination, DestinationGroup, ImageDigest, NetworkPolicy, NetworkRule, PortMapping, Protocol,
    PullPolicy, SandboxEvent, SandboxHandle, SandboxSpec, SecretBinding, SecretBindingMode,
    VolumeMount,
};
pub use wiring::{
    DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_MAX_DURATION_SECS, default_spec_factory, fresh_runner_id,
    stock_wiring,
};

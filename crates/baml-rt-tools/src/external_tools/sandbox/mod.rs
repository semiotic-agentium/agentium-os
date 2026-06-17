// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
pub mod digest;
#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
pub mod exec_adapter;
pub mod handler;
pub mod invoker;
pub mod microsandbox_provider;
pub mod mock;
pub mod path_guard;
pub mod provider;
pub mod session_invoker;
pub mod session_pool;
pub mod spec;
pub mod wiring;

#[cfg(test)]
mod tests;

pub use channel::{MAX_FRAME_BYTES, TsrpcChannel};
pub use digest::{canonical_bind_digest, file_sha256};
pub use handler::{SandboxToolHandler, SandboxToolSession};
pub use invoker::{SandboxCache, SandboxCacheKey, SandboxInvoker, SandboxSpecBuilder};
pub use microsandbox_provider::MicrosandboxProvider;
pub use mock::{MockSandboxProvider, ScriptedAdapter, test_durations};
pub use provider::SandboxProvider;
pub use session_invoker::{SandboxSessionInvoker, SandboxSessionInvokerConfig};
pub use session_pool::{
    DEFAULT_POOL_CHECKOUT_TIMEOUT, DEFAULT_POOL_MAX, PoolError, PooledSandbox, PooledSessionId,
    SessionPool, SessionPoolConfig,
};
pub use spec::{
    Destination, DestinationGroup, NetworkPolicy, NetworkRule, PortMapping, Protocol, PullPolicy,
    SandboxEvent, SandboxHandle, SandboxImageSource, SandboxSpec, SecretBinding, SecretBindingMode,
    VolumeMount,
};
pub use wiring::{
    DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_MAX_DURATION_SECS, default_spec_factory,
    default_spec_factory_with_bind_roots, fresh_runner_id, stock_wiring,
    stock_wiring_with_bind_roots,
};

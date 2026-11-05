// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! VM backend for the v2 Sandbox tier.
//!
//! - [`provider`]: the `SandboxProvider` trait, the host's only entry point
//!   into VM infrastructure.
//! - [`microsandbox_provider`]: the microsandbox implementation (behind the
//!   `sandbox-provider` feature, Linux only).
//! - [`runner_id`]: the persisted identity that owns this runner's VMs.
//!
//! The pool, grants and supervisor channel live in
//! [`super::host`]; this module only boots, sweeps and tears down VMs.

pub mod channel;
pub mod microsandbox_provider;
pub mod provider;
pub mod runner_id;
pub mod spec;

pub use channel::{MAX_FRAME_BYTES, TsrpcChannel};
pub use microsandbox_provider::MicrosandboxProvider;
#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
pub use microsandbox_provider::TOOL_VM_OWNER_LABEL;
pub use provider::SandboxProvider;
pub use runner_id::{fresh_runner_id, load_or_create_runner_id};
pub use spec::SandboxHandle;

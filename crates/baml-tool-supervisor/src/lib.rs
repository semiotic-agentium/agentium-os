// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Guest supervisor for external tools v2 (P4).
//!
//! The supervisor is the only process on the vsock. It fetches and re-verifies
//! `.ktool` bundles, unpacks them behind path/limit guards, and runs each tool
//! as an isolated worker child (distinct uid in the VM, bwrap on a dev box),
//! enforcing the wall-clock deadline, output cap, cancel, and reveal-fd
//! delivery. It holds no `ToolGrant`: network and egress-bound secrets are
//! enforced by the host *outside* the VM.
//!
//! The codec is shared with the host through `baml-sandbox-protocol`; bundle
//! parsing/digests are shared through `baml-ktool`, so host and guest cannot
//! drift.

pub mod error;
pub mod limits;
pub mod pinned_hosts;
pub mod sandbox;
pub mod secrets;
pub mod supervisor;
pub mod tool;
pub mod transport;
pub mod usage;
pub mod worker;

pub use error::SupervisorError;
pub use limits::SupervisorLimits;
#[cfg(target_os = "linux")]
pub use sandbox::root::RootCloneBackend;
pub use sandbox::{WorkerBackend, bwrap::BwrapBackend, direct::DirectBackend};
pub use supervisor::{SUPERVISOR_NAME, Supervisor};
pub use usage::Usage;

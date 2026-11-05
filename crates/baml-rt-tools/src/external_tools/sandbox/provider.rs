// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! The host's only entry point into VM infrastructure. Concrete providers
//! (microsandbox, test fakes) plug in here so the pool never depends on a
//! specific VM crate.

use async_trait::async_trait;
use baml_rt_core::Result;

use super::{channel::TsrpcChannel, spec::SandboxHandle};

/// VM lifecycle for the v2 Sandbox tier.
///
/// - `teardown` is best effort: "already gone" is `Ok(())`.
/// - `sweep_vms` never touches VMs of another owner.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Start a shared VM and accept its persistent supervisor connection.
    /// There is no fallback to exec stdin: a supervisor that does not connect
    /// is a deployment error.
    async fn create_vm(
        &self,
        spec: super::super::host::VmSpec,
    ) -> Result<(SandboxHandle, TsrpcChannel)>;

    /// Stop and remove a VM. Safe to call on a VM that is already gone.
    async fn teardown(&self, handle: &SandboxHandle) -> Result<()>;

    /// Remove provider-owned warm templates at runner shutdown.
    async fn clear_vm_templates(&self) -> Result<()> {
        Ok(())
    }

    /// Stop and remove every VM labelled as owned by `owner` that this
    /// process does not hold. Run once at pool start: a supervisor dials its
    /// vsock exactly once, so a VM left by an earlier run of the same runner
    /// (crash, SIGKILL, missed shutdown) can never be reattached and only
    /// holds disk. Returns how many VMs were removed.
    async fn sweep_vms(&self, _owner: &str) -> Result<usize> {
        Ok(0)
    }
}

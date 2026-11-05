// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! [`SandboxProvider`] over the `microsandbox` crate.
//!
//! Behind the `sandbox-provider` Cargo feature *and* `target_os = "linux"`.
//! [`MicrosandboxProvider::new`] has three behaviors:
//!   - Linux + feature: real provider
//!   - non-Linux + feature: `Ok` stub, so the runner starts; VM operations
//!     fail at runtime (microsandbox is never linked off Linux)
//!   - feature off: `Err` explaining sandbox support wasn't compiled in

#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
use dashmap::DashMap;
#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
use tracing::{debug, warn};

#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
#[path = "microsandbox_v2.rs"]
mod v2;
#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
pub use v2::OWNER_LABEL as TOOL_VM_OWNER_LABEL;

use super::{channel::TsrpcChannel, provider::SandboxProvider, spec::SandboxHandle};

/// How long to wait for `stop_and_wait` before falling back to `kill`.
#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Microsandbox-backed provider. Construction is fallible because sandbox
/// support may be compiled out (`sandbox-provider` feature).
pub struct MicrosandboxProvider {
    #[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
    live: Arc<DashMap<String, microsandbox::Sandbox>>,
    #[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
    v2: v2::VmBackend,
    #[cfg(not(all(feature = "sandbox-provider", target_os = "linux")))]
    _unused: std::marker::PhantomData<()>,
}

impl MicrosandboxProvider {
    pub fn new() -> Result<Self> {
        #[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
        {
            Ok(Self {
                live: Arc::new(DashMap::new()),
                v2: v2::VmBackend::new()?,
            })
        }
        #[cfg(all(feature = "sandbox-provider", not(target_os = "linux")))]
        {
            Ok(Self {
                _unused: std::marker::PhantomData,
            })
        }
        #[cfg(not(feature = "sandbox-provider"))]
        {
            Err(BamlRtError::InvalidArgument(
                "MicrosandboxProvider requires the 'sandbox-provider' feature to be enabled"
                    .to_string(),
            ))
        }
    }
}

#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
fn to_rt_err<E: std::fmt::Display>(context: &str, err: E) -> BamlRtError {
    BamlRtError::InvalidArgument(format!("{context}: {err}"))
}

#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
#[async_trait]
impl SandboxProvider for MicrosandboxProvider {
    async fn create_vm(
        &self,
        spec: super::super::host::VmSpec,
    ) -> Result<(SandboxHandle, TsrpcChannel)> {
        self.create_supervisor_vm(spec).await
    }

    async fn clear_vm_templates(&self) -> Result<()> {
        self.clear_supervisor_templates().await
    }

    async fn sweep_vms(&self, owner: &str) -> Result<usize> {
        self.sweep_owned_vms(owner).await
    }

    async fn teardown(&self, handle: &SandboxHandle) -> Result<()> {
        let Some(sandbox) = self.live.get(&handle.name).map(|s| s.clone()) else {
            // "Already gone" is fine per the SandboxProvider contract.
            return Ok(());
        };
        // Mirror the in-VM supervisor console to host tracing before the VM is
        // stopped, so in-guest failures survive teardown and show up in the
        // runner log instead of only microsandbox's on-disk exec.log.
        v2::dump_vm_console(&handle.name, "teardown");

        match tokio::time::timeout(STOP_TIMEOUT, sandbox.stop_and_wait()).await {
            Ok(Ok(_status)) => debug!(sandbox = %handle.name, "tool VM stopped cleanly"),
            Ok(Err(err)) => {
                warn!(sandbox = %handle.name, ?err, "stop_and_wait failed; falling back to kill");
                sandbox
                    .kill()
                    .await
                    .map_err(|e| to_rt_err("kill tool VM", e))?;
            }
            Err(_timeout) => {
                warn!(
                    sandbox = %handle.name,
                    timeout_secs = STOP_TIMEOUT.as_secs(),
                    "stop_and_wait timed out; falling back to kill"
                );
                sandbox
                    .kill()
                    .await
                    .map_err(|e| to_rt_err("kill tool VM after timeout", e))?;
            }
        }
        self.live.remove(&handle.name);
        remove_stopped(&handle.name).await;
        Ok(())
    }
}

/// Delete a stopped VM's persisted state (rootfs, config). Teardown means
/// gone: a stopped-but-present VM only holds disk and its name. Best effort,
/// per the `teardown` contract.
#[cfg(all(feature = "sandbox-provider", target_os = "linux"))]
async fn remove_stopped(name: &str) {
    if let Err(error) = microsandbox::Sandbox::remove(name).await {
        warn!(sandbox = %name, %error, "cannot remove stopped sandbox");
    }
}

/// Why the stub provider refuses VM operations: the `sandbox-provider`
/// feature is off, or the target isn't Linux.
#[cfg(not(all(feature = "sandbox-provider", target_os = "linux")))]
const SANDBOX_UNAVAILABLE: &str =
    "sandbox provider unavailable: requires the 'sandbox-provider' feature on a Linux target";

#[cfg(not(all(feature = "sandbox-provider", target_os = "linux")))]
#[async_trait]
impl SandboxProvider for MicrosandboxProvider {
    async fn create_vm(
        &self,
        _spec: super::super::host::VmSpec,
    ) -> Result<(SandboxHandle, TsrpcChannel)> {
        Err(BamlRtError::InvalidArgument(
            SANDBOX_UNAVAILABLE.to_string(),
        ))
    }

    async fn teardown(&self, _handle: &SandboxHandle) -> Result<()> {
        Ok(())
    }
}

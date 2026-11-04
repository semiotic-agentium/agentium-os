// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External tools v2 host authority and shared VM execution.

mod channel;
mod grants;
mod handler;
mod pool;
mod service;
mod trusted;

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use baml_rt_core::{BamlRtError, Result};
use baml_sandbox_protocol::protocol_v2::{CallContext, GuestMessage};
pub use grants::{
    CapabilityDiff, CidrGrant, DnsResolver, EgressSecret, HostPolicy, HostResolver, RevealSecret,
    ToolGrant, ToolPolicy, TrustDomain, VmKey, public_ip,
};
pub use pool::{PoolPolicy, VmBudget, VmPool, VmSpec};
pub use service::{ServiceClient, ServiceHost};
pub use trusted::{TrustGrant, TrustedHost, TrustedHostConfig, TrustedPolicy};

use super::{AuditedKtoolBundle, ExternalToolSnapshot, audit_ktool_bundle, verify_ktool_snapshot};
use crate::ToolSessionContext;

/// How long the host keeps waiting past `deadline_ms` for the guest's own
/// deadline result (vsock round trip + worker kill/reap).
const GUEST_DEADLINE_GRACE: Duration = Duration::from_secs(2);

/// Shared authorization prelude for every v2 tier that runs approved `.ktool`
/// bytes (Sandbox and Trusted). It fails closed and pins the *exact* approved
/// bytes; the tier-specific authority (capability grants vs operator trust) is
/// layered on top by the caller. Keeping this in one place stops the two tiers'
/// approval/expiry/identity checks from drifting apart.
pub(super) fn audit_approved_bundle(
    snapshot: &ExternalToolSnapshot,
    bytes: &[u8],
    ctx: &ToolSessionContext,
) -> Result<AuditedKtoolBundle> {
    if !snapshot.approval.state.is_approved() {
        return Err(invalid("external tool is not approved"));
    }
    if let Some(expiry) = &snapshot.approval.expires_at {
        let expiry = chrono::DateTime::parse_from_rfc3339(expiry)
            .map_err(|_| invalid("invalid tool approval expiry"))?;
        if expiry <= chrono::Utc::now() {
            return Err(invalid("external tool approval expired"));
        }
    }
    let audited = audit_ktool_bundle(bytes)?;
    verify_ktool_snapshot(&audited, snapshot)?;
    let tool_name = ctx.tool_name.to_string();
    if !audited
        .descriptors
        .iter()
        .any(|descriptor| descriptor.name == tool_name)
    {
        return Err(invalid("tool session identity does not match bundle"));
    }
    Ok(audited)
}

pub struct ToolHost {
    pool: Arc<VmPool>,
    policy: HostPolicy,
    resolver: Arc<dyn HostResolver>,
    fingerprint_key: [u8; 32],
}

impl ToolHost {
    pub fn new(
        pool: Arc<VmPool>,
        policy: HostPolicy,
        resolver: Arc<dyn HostResolver>,
    ) -> Result<Self> {
        let mut fingerprint_key = [0; 32];
        getrandom::fill(&mut fingerprint_key)
            .map_err(|_| invalid("cannot initialize tool secret fingerprint key"))?;
        if policy.memory_mib == 0
            || policy.call_timeout.is_zero()
            || policy.control_timeout.is_zero()
        {
            return Err(invalid("tool host limits must be nonzero"));
        }
        Ok(Self {
            pool,
            policy,
            resolver,
            fingerprint_key,
        })
    }

    /// Re-audit executable bytes and intersect the approval with agent policy.
    pub async fn authorize(
        &self,
        snapshot: &ExternalToolSnapshot,
        bytes: &[u8],
        ctx: &ToolSessionContext,
    ) -> Result<ToolGrant> {
        let audited = audit_approved_bundle(snapshot, bytes, ctx)?;
        let policy = self
            .policy
            .tools
            .get(&(ctx.agent_id.to_string(), audited.tool_digest))
            .ok_or_else(|| {
                invalid(format!(
                    "agent '{}' is not authorized for tool '{}' ({})",
                    ctx.agent_id, ctx.tool_name, audited.tool_digest
                ))
            })?;
        tokio::time::timeout(
            self.policy.control_timeout,
            grants::grant(
                ctx,
                audited.tool_digest,
                audited.manifest.runtime_image.digest,
                // R1 §R1.1: capabilities are toolset-level, pinned by the exact
                // bytes the tool_digest covers (the manifest, not the interface
                // descriptor). Equivalent to the approved snapshot's requested.
                &audited.manifest.requested,
                policy,
                self.resolver.as_ref(),
                &self.fingerprint_key,
            ),
        )
        .await
        .map_err(|_| invalid("tool grant resolution timed out"))?
    }

    /// Invoke only after a fresh approval and policy check. Identity comes from the session.
    pub async fn invoke(
        &self,
        snapshot: &ExternalToolSnapshot,
        bytes: &[u8],
        ctx: &ToolSessionContext,
        mut context: CallContext,
        input: serde_json::Value,
    ) -> Result<GuestMessage> {
        let grant = Arc::new(self.authorize(snapshot, bytes, ctx).await?);
        context.agent_id = grant.agent_id.clone();
        context.context_id = ctx.context_id.to_string();
        context.task_id = ctx
            .task_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("host clock precedes Unix epoch"))?;
        let host_deadline = now
            .saturating_add(self.policy.call_timeout)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        context.deadline_ms = if context.deadline_ms == 0 {
            host_deadline
        } else {
            context.deadline_ms.min(host_deadline)
        };
        let deadline = context.deadline_ms;
        let remaining = || -> Result<Duration> {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| invalid("invalid host clock"))?
                .as_millis() as u64;
            if deadline <= now {
                return Err(invalid("tool invocation deadline elapsed"));
            }
            Ok(Duration::from_millis(deadline - now))
        };
        remaining()?;
        let v2 = snapshot.v2.as_ref().expect("authorized v2 snapshot");
        let runtime_image = match &v2.delivery {
            super::ToolDelivery::Bundled { runtime_image, .. } => runtime_image.clone(),
            super::ToolDelivery::Reference { .. } => {
                return Err(invalid(
                    "Service snapshots must use ServiceHost, not ToolHost",
                ));
            }
        };
        let vm = self
            .pool
            .acquire(
                runtime_image,
                grant.clone(),
                self.policy.memory_mib,
                self.policy.control_timeout,
            )
            .await?;
        let outcome = async {
            vm.channel()
                .load(grant.tool_digest, grant.key.runtime_image_digest, bytes)
                .await?;
            vm.channel()
                .invoke(
                    grant.tool_digest,
                    ctx.tool_name.to_string(),
                    context,
                    input,
                    // The supervisor enforces `deadline_ms` in the guest and
                    // answers with a classified (Transient) result. Wait a
                    // short grace past the deadline for that verdict instead
                    // of racing it with an unclassified host-side timeout.
                    remaining()? + GUEST_DEADLINE_GRACE,
                    grant.reveal_secrets(),
                )
                .await
        }
        .await;
        drop(vm);
        if outcome.is_err() {
            // The channel owns per-call cancellation. Keep a healthy shared VM
            // alive after a deadline or caller cancellation; prune only a
            // channel that retired itself after an I/O or protocol failure.
            self.pool.prune().await?;
        }
        outcome
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.pool.shutdown().await
    }
}

fn invalid(message: impl Into<String>) -> BamlRtError {
    BamlRtError::InvalidArgument(message.into())
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Value types the runtime hands to a [`super::provider::SandboxProvider`].
//!
//! Shapes match `tool_sandbox.md` §7.3 `SandboxSpec` 1:1 so the first
//! implementation ([`super::microsandbox_provider::MicrosandboxProvider`]) is
//! a thin passthrough onto `microsandbox::SandboxBuilder`.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

/// Sandbox rootfs source used by providers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SandboxImageSource {
    Oci(String),
    Bind(PathBuf),
}

impl SandboxImageSource {
    pub fn is_oci(&self) -> bool {
        matches!(self, Self::Oci(_))
    }
}

/// Filesystem mount forwarded to the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMount {
    /// Host path. Tmpfs / named volumes may leave empty; provider-specific.
    pub host: String,
    /// Guest mount point.
    pub guest: String,
    /// Default true — mounts are readonly unless explicitly opted out.
    #[serde(default = "default_true")]
    pub readonly: bool,
}

fn default_true() -> bool {
    true
}

/// Host↔guest port mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMapping {
    pub host: u16,
    pub guest: u16,
}

/// Image pull behavior. Matches microsandbox's PullPolicy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    #[default]
    IfMissing,
    Always,
    Never,
}

/// Outbound network policy compiled from tool-requested + runner-authorized
/// capabilities (§10.2). Default action must be `Deny`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Explicit allow rules applied on top of the default-deny.
    #[serde(default)]
    pub allow: Vec<NetworkRule>,
    /// Built-in blocks that can't be overridden by tool declarations
    /// (`DestinationGroup::Metadata`, `DestinationGroup::Private`).
    #[serde(default)]
    pub hard_deny: Vec<DestinationGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRule {
    pub destination: Destination,
    #[serde(default)]
    pub protocol: Option<Protocol>,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    Any,
    Domain(String),
    Cidr(String),
    Group(DestinationGroup),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationGroup {
    /// Cloud metadata endpoints (169.254.169.254 and friends). Hard-denied.
    Metadata,
    /// RFC1918 + link-local. Hard-denied.
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Tcp,
    Udp,
    Http,
    Https,
}

/// Secret binding injected into the sandbox (§10.1 / §10.1a). A binding is
/// exclusively one mode — never both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretBinding {
    pub env_var: String,
    pub value: String,
    pub binding: SecretBindingMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SecretBindingMode {
    /// Create-time egress-bound. Microsandbox substitutes the placeholder on
    /// outbound calls to any of `allow_hosts`. Plaintext never enters the VM.
    EgressBound { allow_hosts: Vec<String> },
    /// Per-invoke TSRPC-carried. Plaintext is attached to the `Invoke`
    /// payload (runner-resolved each call).
    PerInvoke,
}

/// Input to [`SandboxProvider::create`](super::provider::SandboxProvider::create).
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    /// Encoded sandbox name: `baml:<runner_id>:<agent_instance>:<ctx>:<tool>`
    /// (§9.2). Provider uses this for list/reattach.
    pub name: String,
    pub image: SandboxImageSource,
    /// Guest-side working directory used for sandbox creation, adapter exec,
    /// and default `PWD`. Must exist inside the guest filesystem.
    pub guest_workdir: String,
    pub cpus: u32,
    pub memory_mib: u32,
    pub env: BTreeMap<String, String>,
    pub volumes: Vec<VolumeMount>,
    pub port_mappings: Vec<PortMapping>,
    pub network_policy: NetworkPolicy,
    pub secrets: Vec<SecretBinding>,
    /// `/.msb/scripts/` entries the guest can exec. Optional.
    pub scripts: BTreeMap<String, String>,
    pub idle_timeout: Duration,
    pub max_duration: Duration,
    /// When true, sandbox survives runner exit (§9.4 default). The runner
    /// still tears down explicitly on context close.
    pub detached: bool,
    pub pull_policy: PullPolicy,
    /// Guest-side argv for the tool-adapter entrypoint. Empty = use image
    /// default. Provider passes this to `exec_stream` (§5.2).
    pub entrypoint: Vec<String>,
    /// Creation-time policy hash snapshot, used by the reattach
    /// checklist (policy hash match).
    pub policy_hash: Option<String>,
}

impl SandboxSpec {
    /// Minimal spec useful for tests. All policy fields default to "empty"
    /// (deny-all network, no secrets, no volumes).
    pub fn for_test(name: impl Into<String>, image: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            image: SandboxImageSource::Oci(image.into()),
            guest_workdir: "/".to_string(),
            cpus: 1,
            memory_mib: 512,
            env: BTreeMap::new(),
            volumes: Vec::new(),
            port_mappings: Vec::new(),
            network_policy: NetworkPolicy::default(),
            secrets: Vec::new(),
            scripts: BTreeMap::new(),
            idle_timeout: Duration::from_secs(300),
            max_duration: Duration::from_secs(3600),
            detached: true,
            pull_policy: PullPolicy::IfMissing,
            entrypoint: Vec::new(),
            policy_hash: None,
        }
    }
}

/// Opaque handle returned by [`SandboxProvider::create`]. Providers may stash
/// implementation-specific state inside; the runtime treats it as an identifier.
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub name: String,
    pub created_at: SystemTime,
    /// Last successful host-side use. Used with `idle_timeout` so the runtime
    /// does not hand out handles microsandbox may already have reaped.
    pub last_used_at: SystemTime,
    /// Guest-side working directory used when (re)launching `/tool-adapter`.
    pub guest_workdir: String,
    /// Snapshot of the policy hash at create time — used by the
    /// reattach checklist to detect drift (§9.4).
    pub policy_hash: Option<String>,
    /// Idle timeout configured for this sandbox. Microsandbox may stop the VM
    /// after this much inactivity even when `max_duration` has not elapsed.
    pub idle_timeout: Duration,
    /// The `max_duration` that was set on this sandbox — used for the reattach
    /// age check so the runtime can reason about remaining lifetime without
    /// a second trip to the provider.
    pub max_duration: Duration,
}

impl SandboxHandle {
    pub fn new(name: impl Into<String>, max_duration: Duration) -> Self {
        let now = SystemTime::now();
        Self {
            name: name.into(),
            created_at: now,
            last_used_at: now,
            guest_workdir: "/".to_string(),
            policy_hash: None,
            idle_timeout: max_duration,
            max_duration,
        }
    }

    pub fn touch(&mut self) {
        self.last_used_at = SystemTime::now();
    }

    /// True when either lifetime bound has elapsed:
    /// - `created_at + max_duration` absolute cap.
    /// - `last_used_at + idle_timeout` idle reap window.
    pub fn is_expired(&self) -> bool {
        let max_expired = self
            .created_at
            .elapsed()
            .map(|elapsed| elapsed >= self.max_duration)
            .unwrap_or(false);
        let idle_expired = self
            .last_used_at
            .elapsed()
            .map(|elapsed| elapsed >= self.idle_timeout)
            .unwrap_or(false);
        max_expired || idle_expired
    }
}

/// Provider-emitted lifecycle event. Streamed via
/// [`SandboxProvider::events`](super::provider::SandboxProvider::events); the
/// runtime translates these into [`super::super::ExternalLifecycleEvent`] and
/// observability spans.
#[derive(Debug, Clone)]
pub enum SandboxEvent {
    Created {
        name: String,
    },
    Started {
        name: String,
    },
    Stopped {
        name: String,
    },
    /// Sandbox died outside of an explicit teardown — runtime evicts the
    /// cache entry (§9.3 `SandboxTerminatedUnexpectedly`).
    TerminatedUnexpectedly {
        name: String,
        reason: String,
    },
    PolicyDenied {
        name: String,
        rule: String,
    },
}

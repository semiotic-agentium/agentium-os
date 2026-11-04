// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host decisions. None of these grants can be deserialized from a tool manifest.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::IpAddr,
    time::Duration,
};

use async_trait::async_trait;
use baml_rt_core::Result;
use serde::Serialize;

use super::invalid;
use crate::{
    ToolSecretValue, ToolSessionContext,
    external_tools::{CapabilityRequest, HostPort, SecretBinding, SecretRequest},
    mcp_snapshot::{Digest, canonical_digest},
    metrics::{self, GrantDenial},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustDomain {
    FirstParty,
    #[default]
    ThirdParty,
}

impl TrustDomain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FirstParty => "first-party",
            Self::ThirdParty => "third-party",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct VmKey {
    pub trust_domain: TrustDomain,
    pub runtime_image_digest: Digest,
    pub grant_class: Digest,
}

/// Operator policy for one agent and exact tool digest. Empty capabilities deny all access.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    pub trust_domain: TrustDomain,
    pub capabilities: CapabilityRequest,
    /// Exact HTTPS destinations for each egress-bound secret. No wildcards.
    pub secret_hosts: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct HostPolicy {
    pub tools: HashMap<(String, Digest), ToolPolicy>,
    pub memory_mib: u32,
    pub call_timeout: Duration,
    pub control_timeout: Duration,
}

impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            tools: HashMap::new(),
            memory_mib: 512,
            call_timeout: Duration::from_secs(30),
            control_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CapabilityDiff {
    pub added: CapabilityRequest,
    pub removed: CapabilityRequest,
}

impl CapabilityDiff {
    pub fn between(approved: &CapabilityRequest, requested: &CapabilityRequest) -> Self {
        fn difference(a: &CapabilityRequest, b: &CapabilityRequest) -> CapabilityRequest {
            CapabilityRequest {
                net: a
                    .net
                    .iter()
                    .filter(|v| !b.net.contains(v))
                    .cloned()
                    .collect(),
                secrets: a
                    .secrets
                    .iter()
                    .filter(|v| !b.secrets.contains(v))
                    .cloned()
                    .collect(),
            }
        }
        Self {
            added: difference(requested, approved),
            removed: difference(approved, requested),
        }
    }

    pub fn widens(&self) -> bool {
        !self.added.net.is_empty() || !self.added.secrets.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CidrGrant {
    pub cidr: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct EgressSecret {
    pub name: String,
    pub hosts: Vec<String>,
    pub value: ToolSecretValue,
}

/// A reveal-tier secret bound to the exact grant. Delivered to one worker over
/// a guest-internal fd, never on the wire in the invoke payload and never in
/// env/argv.
#[derive(Debug)]
pub struct RevealSecret {
    pub name: String,
    pub value: ToolSecretValue,
}

#[derive(Debug)]
pub struct ToolGrant {
    pub(super) agent_id: String,
    pub(super) tool_digest: Digest,
    pub(super) key: VmKey,
    pub(super) network: Vec<CidrGrant>,
    /// Declared hostname -> the exact addresses `network` allows for it. The
    /// VM serves these from `/etc/hosts`; guest DNS egress stays closed.
    pub(super) hosts: BTreeMap<String, BTreeSet<IpAddr>>,
    pub(super) secrets: Vec<EgressSecret>,
    pub(super) reveal: Vec<RevealSecret>,
}

impl ToolGrant {
    pub fn key(&self) -> &VmKey {
        &self.key
    }
    pub fn network(&self) -> &[CidrGrant] {
        &self.network
    }
    pub fn hosts(&self) -> &BTreeMap<String, BTreeSet<IpAddr>> {
        &self.hosts
    }
    pub fn egress_secrets(&self) -> &[EgressSecret] {
        &self.secrets
    }
    pub fn reveal_secrets(&self) -> &[RevealSecret] {
        &self.reveal
    }
    pub fn tool_digest(&self) -> Digest {
        self.tool_digest
    }
}

#[async_trait]
pub trait HostResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>>;
}

pub struct DnsResolver;

#[async_trait]
impl HostResolver for DnsResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>> {
        tokio::net::lookup_host((host, port))
            .await
            .map(|addresses| addresses.map(|a| a.ip()).collect())
            .map_err(|_| invalid(format!("cannot resolve tool destination '{host}:{port}'")))
    }
}

type GrantedNetwork = (BTreeSet<CidrGrant>, BTreeMap<String, BTreeSet<IpAddr>>);
type GrantedSecrets = (
    BTreeMap<String, EgressSecret>,
    BTreeMap<String, RevealSecret>,
);

/// Resolve every requested destination to exact public addresses.
async fn grant_network(
    requested: &CapabilityRequest,
    resolver: &dyn HostResolver,
) -> Result<GrantedNetwork> {
    let mut network = BTreeSet::new();
    let mut hosts: BTreeMap<String, BTreeSet<IpAddr>> = BTreeMap::new();
    for HostPort { host, port } in &requested.net {
        validate_host(host)?;
        if *port == 0 {
            return Err(invalid("tool network port must be nonzero"));
        }
        let addresses = resolver.resolve(host, *port).await?;
        if addresses.is_empty() {
            return Err(invalid(format!("no addresses for '{host}'")));
        }
        for ip in addresses {
            if !public_ip(ip) {
                return Err(invalid(format!(
                    "tool destination '{host}' resolves to a forbidden address"
                )));
            }
            network.insert(CidrGrant {
                cidr: format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 }),
                port: *port,
            });
            if host.parse::<IpAddr>().is_err() {
                hosts.entry(host.clone()).or_default().insert(ip);
            }
        }
    }
    Ok((network, hosts))
}

/// Bind every requested secret: egress-bound to approved HTTPS hosts, or reveal.
fn grant_secrets(
    ctx: &ToolSessionContext,
    requested: &CapabilityRequest,
    policy: &ToolPolicy,
) -> Result<GrantedSecrets> {
    let mut secrets = BTreeMap::new();
    let mut reveal = BTreeMap::new();
    for SecretRequest { name, binding } in &requested.secrets {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(invalid("invalid secret environment name"));
        }
        if *binding == SecretBinding::Reveal {
            // P4 supervisor delivers the value over a sealed memfd to exactly
            // one worker. It does not need an egress host binding, but it is
            // still part of the approved capability set and the grant class.
            let value = ctx
                .secrets
                .resolve(name)
                .map_err(|e| invalid(e.to_string()))?
                .ok_or_else(|| invalid(format!("missing agent-scoped secret '{name}'")))?;
            reveal.insert(
                name.clone(),
                RevealSecret {
                    name: name.clone(),
                    value,
                },
            );
            continue;
        }
        let mut hosts = policy
            .secret_hosts
            .get(name)
            .cloned()
            .ok_or_else(|| invalid(format!("missing egress binding for secret '{name}'")))?;
        hosts.sort();
        hosts.dedup();
        if hosts.is_empty() {
            return Err(invalid(format!("empty egress binding for secret '{name}'")));
        }
        for host in &hosts {
            validate_host(host)?;
            if !requested.net.contains(&HostPort {
                host: host.clone(),
                port: 443,
            }) {
                return Err(invalid(format!(
                    "secret '{name}' host '{host}' requires an approved HTTPS destination"
                )));
            }
        }
        let value = ctx
            .secrets
            .resolve(name)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid(format!("missing agent-scoped secret '{name}'")))?;
        secrets.insert(
            name.clone(),
            EgressSecret {
                name: name.clone(),
                hosts,
                value,
            },
        );
    }
    Ok((secrets, reveal))
}

pub(super) async fn grant(
    ctx: &ToolSessionContext,
    tool_digest: Digest,
    image_digest: Digest,
    requested: &CapabilityRequest,
    policy: &ToolPolicy,
    resolver: &dyn HostResolver,
    fingerprint_key: &[u8; 32],
) -> Result<ToolGrant> {
    let diff = CapabilityDiff::between(&policy.capabilities, requested);
    if diff.widens() {
        metrics::record_tool_grant_denied(GrantDenial::CapabilityOutsidePolicy);
        return Err(invalid(format!(
            "tool {tool_digest} requests capabilities outside agent policy: {:?}",
            diff.added
        )));
    }
    // Each stage maps its refusal to one bounded denial reason.
    let (network, hosts) = grant_network(requested, resolver)
        .await
        .inspect_err(|_| metrics::record_tool_grant_denied(GrantDenial::Network))?;
    let (secrets, reveal) = grant_secrets(ctx, requested, policy)
        .inspect_err(|_| metrics::record_tool_grant_denied(GrantDenial::Secret))?;
    let network: Vec<_> = network.into_iter().collect();
    let secrets: Vec<_> = secrets.into_values().collect();
    let reveal: Vec<_> = reveal.into_values().collect();
    // Keyed fingerprints prevent offline guessing and force fresh VMs on rotation.
    // The process-local key deliberately prevents reusing secret-bearing VMs after restart.
    // Reveal secrets are part of the grant class too: a rotated reveal value
    // must not reuse a VM that already staged the old one.
    let bindings: Vec<_> = secrets
        .iter()
        .map(|s| {
            (
                &s.name,
                &s.hosts,
                blake3::keyed_hash(fingerprint_key, s.value.as_str().as_bytes())
                    .to_hex()
                    .to_string(),
            )
        })
        .collect();
    let reveal_bindings: Vec<_> = reveal
        .iter()
        .map(|s| {
            (
                &s.name,
                blake3::keyed_hash(fingerprint_key, s.value.as_str().as_bytes())
                    .to_hex()
                    .to_string(),
            )
        })
        .collect();
    // The hosts map is VM state (`/etc/hosts`), so it is part of VM identity.
    let grant_class = canonical_digest(&(&network, &hosts, bindings, reveal_bindings));
    Ok(ToolGrant {
        agent_id: ctx.agent_id.to_string(),
        tool_digest,
        key: VmKey {
            trust_domain: policy.trust_domain,
            runtime_image_digest: image_digest,
            grant_class,
        },
        network,
        hosts,
        secrets,
        reveal,
    })
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host != host.to_ascii_lowercase()
        || host.ends_with('.')
        || host.contains('*')
        || host.contains('/')
        || host.contains('@')
        || host.bytes().any(|b| b.is_ascii_whitespace())
        || (host.parse::<IpAddr>().is_err()
            && host.split('.').any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
    {
        return Err(invalid(format!("invalid exact tool hostname '{host}'")));
    }
    Ok(())
}

/// Reject non-global ranges before generating exact CIDRs, including mapped IPv4.
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            // Only native global unicast. No mapped addresses, NAT64 or 6to4 tunnels.
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

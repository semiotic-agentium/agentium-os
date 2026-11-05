// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Microsandbox 0.7 persistent vsock and clean, grant-keyed snapshot templates.

use std::{collections::HashMap, os::unix::fs::DirBuilderExt, path::PathBuf, sync::Mutex};

use baml_rt_core::Result;
use microsandbox::Sandbox;
use tokio::net::UnixListener;

use super::{MicrosandboxProvider, to_rt_err};
use crate::{
    external_tools::{
        KtoolRuntimeImage,
        host::{CidrGrant, VmKey, VmSpec},
        sandbox::{SandboxHandle, TsrpcChannel},
    },
    mcp_snapshot::Digest,
};

const SUPERVISOR_PORT: u32 = 5000;
const MAX_TEMPLATES: usize = 8;
/// Label naming the runner that owns a v2 tool VM ([`VmSpec::owner`]).
pub const OWNER_LABEL: &str = "karakara.tool-vm.owner";
const SWEEP_PAGE: u32 = 100;

pub(super) struct VmBackend {
    images: HashMap<Digest, (KtoolRuntimeImage, String)>,
    templates: Mutex<HashMap<VmKey, (String, u32)>>,
    socket_dir: PathBuf,
}

impl VmBackend {
    pub fn new() -> Result<Self> {
        let socket_dir = std::env::temp_dir().join(format!("karak-vsock-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&socket_dir)
            .map_err(|e| to_rt_err("create private vsock directory", e))?;
        Ok(Self {
            images: HashMap::new(),
            templates: Mutex::new(HashMap::new()),
            socket_dir,
        })
    }
}

impl Drop for VmBackend {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

impl MicrosandboxProvider {
    /// Bind a platform runtime contract to exact OCI bytes. Tool manifests cannot set this map.
    pub fn with_runtime_image(mut self, image: KtoolRuntimeImage, oci_ref: String) -> Result<Self> {
        let (_, digest) = oci_ref
            .rsplit_once('@')
            .ok_or_else(|| to_rt_err("runtime image", "OCI reference must be digest-pinned"))?;
        digest
            .parse::<Digest>()
            .map_err(|e| to_rt_err("runtime image", e))?;
        if let Some(existing) = self.v2.images.get(&image.digest)
            && existing != &(image.clone(), oci_ref.clone())
        {
            return Err(to_rt_err(
                "runtime image",
                "registry identity already bound to different bytes",
            ));
        }
        self.v2.images.insert(image.digest, (image, oci_ref));
        Ok(self)
    }

    pub(super) async fn clear_supervisor_templates(&self) -> Result<()> {
        let templates = self.v2.templates.lock().unwrap().clone();
        for (key, (snapshot, _)) in templates {
            microsandbox::Snapshot::remove(&snapshot, true)
                .await
                .map_err(|e| to_rt_err("remove tool VM template", e))?;
            self.v2.templates.lock().unwrap().remove(&key);
        }
        Ok(())
    }

    /// [`SandboxProvider::sweep_vms`] for microsandbox: list VMs carrying
    /// `OWNER_LABEL=owner`, kill any still running, and remove them. VMs this
    /// process holds live are skipped. Per-VM failures are logged and skipped
    /// so one stuck VM does not block reclaiming the rest.
    ///
    /// [`SandboxProvider::sweep_vms`]: crate::external_tools::sandbox::SandboxProvider::sweep_vms
    pub(super) async fn sweep_owned_vms(&self, owner: &str) -> Result<usize> {
        let mut owned = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = Sandbox::list_with(|query| {
                let query = query.limit(SWEEP_PAGE).label(OWNER_LABEL, owner);
                match cursor.take() {
                    Some(cursor) => query.cursor(cursor),
                    None => query,
                }
            })
            .await
            .map_err(|e| to_rt_err("list tool VMs", e))?;
            owned.extend(page.sandboxes);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let mut removed = 0;
        for vm in owned {
            let name = vm.name().to_string();
            if self.live.contains_key(&name) {
                continue;
            }
            use microsandbox::sandbox::SandboxStatus;
            let stopped = matches!(
                vm.status_snapshot(),
                SandboxStatus::Stopped | SandboxStatus::Crashed | SandboxStatus::Created
            );
            // An orphan's host listener is gone, so nothing can use it: kill, don't drain.
            if !stopped && let Err(error) = vm.kill().await {
                tracing::warn!(vm = %name, %error, "cannot stop leftover tool VM");
                continue;
            }
            match Sandbox::remove(&name).await {
                Ok(()) => removed += 1,
                Err(error) => tracing::warn!(vm = %name, %error, "cannot remove leftover tool VM"),
            }
        }
        Ok(removed)
    }

    pub(super) async fn create_supervisor_vm(
        &self,
        spec: VmSpec,
    ) -> Result<(SandboxHandle, TsrpcChannel)> {
        let (image, oci_ref) = self
            .v2
            .images
            .get(&spec.image.digest)
            .ok_or_else(|| to_rt_err("runtime image", "no pinned platform image registered"))?;
        if image != &spec.image || image.digest != spec.grant.key().runtime_image_digest {
            return Err(to_rt_err(
                "runtime image",
                "grant and registry identity differ",
            ));
        }
        let socket_path = self
            .v2
            .socket_dir
            .join(format!("{}.sock", uuid::Uuid::new_v4()));
        let socket = SocketPath(socket_path);
        let listener =
            UnixListener::bind(&socket.0).map_err(|e| to_rt_err("bind supervisor vsock", e))?;
        let policy = compile_network(spec.grant.network())?;
        let template = self
            .v2
            .templates
            .lock()
            .unwrap()
            .get(spec.grant.key())
            .cloned()
            .filter(|(_, memory)| *memory == spec.memory_mib);
        let sandbox = if let Some((snapshot, _)) = template {
            // Templates contain no tool code and no secret material. Never inherit host resources.
            Sandbox::restore(snapshot)
                .name(&spec.name)
                .cow_memory()
                .vsock(&socket.0, SUPERVISOR_PORT)
                .network_policy(policy)
                .restore()
                .await
                .map_err(|e| to_rt_err("restore tool VM template", e))?
        } else {
            let intercept_tls = !spec.grant.egress_secrets().is_empty();
            let mut builder = Sandbox::builder(&spec.name)
                .image(oci_ref.as_str())
                .cpus(1)
                .label(OWNER_LABEL, spec.owner.as_str());
            if oci_ref.split_once('/').is_some_and(|(registry, _)| {
                registry.starts_with("localhost:") || registry.starts_with("127.0.0.1:")
            }) {
                // Local development registries are commonly plain HTTP. Never
                // enable this for a normal production registry reference.
                builder = builder.registry(|registry| registry.insecure());
            }
            let mut builder = builder
                .memory(spec.memory_mib)
                // Name resolution for declared hosts is pinned to the exact
                // addresses the network policy allows; the guest gets no DNS
                // egress (plan §8: grants compile to CIDR, not Domain rules).
                // The supervisor writes these into /etc/hosts after boot:
                // microsandbox's guest init rewrites that file, so a rootfs
                // patch would not survive.
                .env(PINNED_HOSTS_ENV, encode_pinned_hosts(spec.grant.hosts()))
                .vsock(&socket.0, SUPERVISOR_PORT)
                // `create()` boots the VM but does not launch the image
                // workload unless a launch intent is set. The supervisor is
                // that workload; without this the VM stays healthy forever
                // without dialing the vsock listener.
                .background_command(["--vsock-port".to_string(), SUPERVISOR_PORT.to_string()])
                // TLS interception exists to inject egress secrets on the wire. With no
                // secret to inject, keep TLS end to end between the tool and its host.
                // When enabled, the VM installs its CA into the guest system bundle and
                // the supervisor points node at it (NODE_EXTRA_CA_CERTS).
                .network(|n| {
                    n.policy(policy)
                        .tls(|tls| tls.enabled(intercept_tls).verify_upstream(true))
                })
                .replace();
            for secret in spec.grant.egress_secrets() {
                builder = builder.secret(|mut s| {
                    s = s
                        .env(&secret.name)
                        .value(secret.value.as_str())
                        .require_tls_identity(true)
                        .violation_action(microsandbox::SecretViolationAction::Block);
                    for host in &secret.hosts {
                        s = s.allow(host);
                    }
                    s
                });
            }
            builder
                .create()
                .await
                .map_err(|e| to_rt_err("create tool VM", e))?
        };
        let handle = SandboxHandle::new(&spec.name);
        self.live.insert(spec.name.clone(), sandbox.clone());
        let connection = tokio::time::timeout(spec.control_timeout, listener.accept()).await;
        let stream = match connection {
            Ok(Ok((stream, _))) => stream,
            _ => {
                // No fallback to exec stdin. A missing supervisor is a deployment error.
                tracing::warn!(vm = %spec.name, "supervisor vsock did not connect; tearing down VM");
                dump_vm_console(&spec.name, "vsock connect failure");
                crate::external_tools::sandbox::SandboxProvider::teardown(self, &handle).await?;
                return Err(to_rt_err(
                    "supervisor vsock",
                    "connection failed or timed out",
                ));
            }
        };
        tracing::info!(vm = %spec.name, "supervisor vsock connected");
        // Warm-template capture is OPT-IN (`KARAKARA_VM_SNAPSHOT=1`).
        //
        // microsandbox's full checkpoint treats virtio-vsock as a `Reconnect`
        // resource (microsandbox-runtime checkpoint/coordinator.rs): quiescing
        // the device resets every live vsock connection on the *source* VM.
        // Capturing here therefore kills the stream we just accepted, and the
        // first Hello write fails with EPIPE. The supervisor dials exactly once
        // and never redials, and `listener` is dropped when this function
        // returns, so neither the source VM nor a VM restored from the template
        // can re-establish the control channel. Enable this only once the
        // supervisor redials after a transport reset and the host re-accepts
        // after capture/restore.
        // Secret-bearing VMs stay warm in the pool but are never persisted as templates.
        let snapshot_enabled = std::env::var("KARAKARA_VM_SNAPSHOT")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let capture = snapshot_enabled && spec.grant.egress_secrets().is_empty() && {
            let templates = self.v2.templates.lock().unwrap();
            templates.len() < MAX_TEMPLATES && !templates.contains_key(spec.grant.key())
        };
        if capture {
            tracing::info!(vm = %spec.name, "capturing warm snapshot template from live VM");
            let name = format!("karak-template-{}", uuid::Uuid::new_v4());
            match microsandbox::Snapshot::builder(&name)
                .from_sandbox(&spec.name)
                .full()
                .create()
                .await
            {
                Ok(snapshot) => {
                    self.v2.templates.lock().unwrap().insert(
                        spec.grant.key().clone(),
                        (snapshot.reference().value().to_string(), spec.memory_mib),
                    );
                }
                Err(_) => tracing::warn!(
                    "tool runtime does not support clean VM snapshots; retaining cold-boot path"
                ),
            }
        }
        tracing::info!(vm = %spec.name, "supervisor channel ready");
        let (reader, writer) = stream.into_split();
        Ok((handle, TsrpcChannel::new(reader, writer)))
    }
}

/// Emit the VM's captured console (`exec.log`) to host tracing.
///
/// The in-VM supervisor's stdout/stderr lands only in microsandbox's on-disk
/// `exec.log`, which is lost on teardown and easy to miss. Mirroring it into
/// the runner's tracing makes in-guest failures (panics, worker-sandbox mount
/// errors) visible in the normal log stream without digging in `~/.microsandbox`.
pub(super) fn dump_vm_console(name: &str, context: &str) {
    let path = microsandbox::logs::log_dir_for(name).join("exec.log");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        tracing::debug!(vm = %name, ?path, "no VM console log to dump ({context})");
        return;
    };
    let mut console = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line)
            && matches!(
                entry.get("s").and_then(|s| s.as_str()),
                Some("stderr" | "stdout" | "system")
            )
            && let Some(data) = entry.get("d").and_then(|d| d.as_str())
        {
            console.push_str(data);
        }
    }
    let console = console.trim();
    if console.is_empty() {
        tracing::debug!(vm = %name, "VM console empty ({context})");
    } else {
        tracing::warn!(vm = %name, context, "VM console:\n{console}");
    }
}

/// Must match `baml_tool_supervisor::pinned_hosts::PINNED_HOSTS_ENV`.
const PINNED_HOSTS_ENV: &str = "KARAKARA_PINNED_HOSTS";

/// `ip host;ip host` for the supervisor's `/etc/hosts` block (IPv4 first, so
/// clients without IPv6 egress connect on the first try).
pub(super) fn encode_pinned_hosts(
    hosts: &std::collections::BTreeMap<String, std::collections::BTreeSet<std::net::IpAddr>>,
) -> String {
    let mut entries = Vec::new();
    for (name, ips) in hosts {
        let (v4, v6): (Vec<&std::net::IpAddr>, Vec<_>) = ips.iter().partition(|ip| ip.is_ipv4());
        for ip in v4.into_iter().chain(v6) {
            entries.push(format!("{ip} {name}"));
        }
    }
    entries.join(";")
}

/// Hard denies precede exact TCP CIDRs; no domain rules or implicit public egress.
pub(super) fn compile_network(grants: &[CidrGrant]) -> Result<microsandbox::NetworkPolicy> {
    let mut policy = microsandbox::NetworkPolicy::builder()
        .default_deny()
        .egress(|r| {
            r.deny_meta()
                .deny_private()
                .deny_link_local()
                .deny_loopback()
                .deny_multicast()
        });
    for grant in grants {
        policy = policy.egress(|r| r.tcp().port(grant.port).allow().cidr(&grant.cidr));
    }
    policy
        .build()
        .map_err(|e| to_rt_err("compile tool network policy", e))
}

struct SocketPath(PathBuf);
impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn pinned_hosts_encode_each_granted_name_ipv4_first() {
        let hosts = BTreeMap::from([(
            "api.open-meteo.com".to_string(),
            BTreeSet::from([
                "2001:db8::1".parse().unwrap(),
                "203.0.113.7".parse().unwrap(),
            ]),
        )]);
        assert_eq!(
            super::encode_pinned_hosts(&hosts),
            "203.0.113.7 api.open-meteo.com;2001:db8::1 api.open-meteo.com"
        );
    }
}

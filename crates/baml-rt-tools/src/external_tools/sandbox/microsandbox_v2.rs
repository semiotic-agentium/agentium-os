// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Microsandbox 0.7 persistent vsock and clean, grant-keyed snapshot templates.

use std::{
    collections::HashMap, os::unix::fs::DirBuilderExt, path::PathBuf, sync::Mutex, time::Duration,
};

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
            let mut builder = Sandbox::builder(&spec.name)
                .image(oci_ref.as_str())
                .cpus(1)
                .memory(spec.memory_mib)
                .vsock(&socket.0, SUPERVISOR_PORT)
                .network(|n| {
                    n.policy(policy)
                        .tls(|tls| tls.enabled(true).verify_upstream(true))
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
        let mut handle = SandboxHandle::new(&spec.name, Duration::from_secs(3600));
        handle.policy_hash = Some(spec.grant.key().grant_class.to_string());
        self.live.insert(spec.name.clone(), sandbox.clone());
        let connection = tokio::time::timeout(spec.control_timeout, listener.accept()).await;
        let stream = match connection {
            Ok(Ok((stream, _))) => stream,
            _ => {
                // No fallback to exec stdin. A missing supervisor is a deployment error.
                crate::external_tools::sandbox::SandboxProvider::teardown(self, &handle).await?;
                return Err(to_rt_err(
                    "supervisor vsock",
                    "connection failed or timed out",
                ));
            }
        };
        // Capture before Hello or LoadTool. Runtime supervisors must reconnect after restore.
        // Secret-bearing VMs stay warm in the pool but are never persisted as templates.
        let capture = spec.grant.egress_secrets().is_empty() && {
            let templates = self.v2.templates.lock().unwrap();
            templates.len() < MAX_TEMPLATES && !templates.contains_key(spec.grant.key())
        };
        if capture {
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
        let (reader, writer) = stream.into_split();
        Ok((handle, TsrpcChannel::new(reader, writer)))
    }
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

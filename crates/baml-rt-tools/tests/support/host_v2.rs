// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared v2 host fixture: a fake supervisor over a duplex (never a VM).
//! Each test binary uses a different subset.
#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use baml_rt_core::{
    Result,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_tools::{
    ToolName, ToolSecretResolver, ToolSecretValue, ToolSecrets, ToolSessionContext,
    external_tools::{
        CapabilityRequest, ExternalToolSnapshot, HostPort, SecretBinding, SecretRequest,
        host::{
            HostPolicy, HostResolver, PoolPolicy, ToolHost, ToolPolicy, TrustDomain, VmPool, VmSpec,
        },
        sandbox::{SandboxHandle, SandboxProvider, TsrpcChannel},
    },
    tool_fsm::ToolSessionId,
    tools::SecretRequest as ToolSecretRequest,
};
use baml_sandbox_protocol::{
    codec::{FrameReader, FrameWriter},
    protocol_v2::{GuestMessage, HostMessage},
};
use serde_json::{Value, json};
use test_support::external_tools_v2::{bundle, descriptor};

pub const SECRET: &str = "METEO_KEY";
pub const TOOL: &str = "support/host_fixture";

/// Deterministic host resolution; tests never touch DNS.
pub struct StaticResolver(pub HashMap<&'static str, Vec<IpAddr>>);

#[async_trait]
impl HostResolver for StaticResolver {
    async fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>> {
        Ok(self.0.get(host).cloned().unwrap_or_default())
    }
}

pub fn resolver() -> Arc<StaticResolver> {
    Arc::new(StaticResolver(HashMap::from([
        ("api.example.com", vec!["1.1.1.1".parse().unwrap()]),
        (
            "metadata.example.com",
            vec!["169.254.169.254".parse().unwrap()],
        ),
    ])))
}

pub struct FixedSecret(pub String);

impl ToolSecretResolver for FixedSecret {
    fn resolve_tool_secret(&self, name: &str) -> Option<ToolSecretValue> {
        (name == SECRET).then(|| ToolSecretValue::new(self.0.clone()))
    }
}

pub fn session_ctx(value: &str) -> ToolSessionContext {
    ToolSessionContext {
        session_id: ToolSessionId::random(),
        tool_name: ToolName::parse(TOOL).unwrap(),
        context_id: ContextId::new(1, 1),
        agent_id: AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4())),
        config: None,
        config_version: None,
        secrets: ToolSecrets::scoped(
            Some(Arc::new(FixedSecret(value.to_string()))),
            &[ToolSecretRequest::api_key(SECRET, "fixture", "fixture")],
        ),
        task_id: None,
        execution_classifier: None,
    }
}

pub fn net(host: &str) -> CapabilityRequest {
    CapabilityRequest {
        net: vec![HostPort {
            host: host.into(),
            port: 443,
        }],
        secrets: vec![],
    }
}

pub fn full_request() -> CapabilityRequest {
    CapabilityRequest {
        net: vec![HostPort {
            host: "api.example.com".into(),
            port: 443,
        }],
        secrets: vec![SecretRequest {
            name: SECRET.into(),
            binding: SecretBinding::EgressBound,
        }],
    }
}

pub fn policy(capabilities: CapabilityRequest) -> ToolPolicy {
    ToolPolicy {
        trust_domain: TrustDomain::ThirdParty,
        capabilities,
        secret_hosts: BTreeMap::from([(SECRET.to_string(), vec!["api.example.com".to_string()])]),
    }
}

/// Fake supervisor over a real duplex: speaks the v2 enum, never a VM.
pub struct FakeVms {
    pub created: AtomicUsize,
    pub teardowns: AtomicUsize,
    pub cancels: Arc<AtomicUsize>,
    /// `grant.hosts()` of every VM spec, i.e. what lands in the VM's /etc/hosts.
    pub hosts: std::sync::Mutex<Vec<BTreeMap<String, std::collections::BTreeSet<IpAddr>>>>,
    /// `spec.owner` of every created VM.
    pub owners: std::sync::Mutex<Vec<String>>,
    /// Owner of every `sweep_vms` call.
    pub sweeps: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl SandboxProvider for FakeVms {
    async fn create_vm(&self, spec: VmSpec) -> Result<(SandboxHandle, TsrpcChannel)> {
        self.created.fetch_add(1, Ordering::SeqCst);
        self.hosts.lock().unwrap().push(spec.grant.hosts().clone());
        self.owners.lock().unwrap().push(spec.owner.clone());
        let (host, guest) = tokio::io::duplex(64 * 1024);
        tokio::spawn(supervisor(guest, self.cancels.clone()));
        let (reader, writer) = tokio::io::split(host);
        Ok((
            SandboxHandle::new(spec.name),
            TsrpcChannel::new(reader, writer),
        ))
    }

    async fn teardown(&self, _: &SandboxHandle) -> Result<()> {
        self.teardowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn sweep_vms(&self, owner: &str) -> Result<usize> {
        self.sweeps.lock().unwrap().push(owner.to_string());
        Ok(2)
    }
}

/// Replies per request_id, concurrently: proves the host multiplexes one stream.
pub async fn supervisor(guest: tokio::io::DuplexStream, cancels: Arc<AtomicUsize>) {
    let (reader, writer) = tokio::io::split(guest);
    let mut reader = FrameReader::new(reader);
    let writer = Arc::new(tokio::sync::Mutex::new(FrameWriter::new(writer)));
    let mut staged: HashMap<u64, Vec<String>> = HashMap::new();
    loop {
        let Ok(value) = reader.recv().await else {
            break;
        };
        let Ok(message) = serde_json::from_value::<HostMessage>(value) else {
            break;
        };
        let reply = match message {
            HostMessage::Hello { .. } => {
                Some(json!({"t": "hello_ack", "proto": 2, "supervisor": "fake"}))
            }
            HostMessage::LoadTool {
                tool_digest,
                bundle_len,
                ..
            } => {
                let mut remaining = bundle_len;
                while remaining > 0 {
                    let Ok(chunk) = reader.recv_bytes().await else {
                        return;
                    };
                    remaining = remaining.saturating_sub(chunk.len() as u64);
                }
                Some(json!({"t": "load_ack", "tool_digest": tool_digest, "ok": true}))
            }
            HostMessage::Invoke(request) => {
                let writer = writer.clone();
                let reveals = staged.remove(&request.request_id).unwrap_or_default();
                tokio::spawn(async move {
                    if let Some(delay) = request.input.get("delay").and_then(Value::as_u64) {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    let reply = if request.input.get("fail").is_some() {
                        json!({
                            "t": "result", "request_id": request.request_id, "ok": false,
                            "error": "boom", "error_class": "transient",
                        })
                    } else {
                        json!({
                            "t": "result", "request_id": request.request_id, "ok": true,
                            "output": {
                                "echo": request.input,
                                "agent": request.call_context.agent_id,
                                "reveals": reveals,
                            },
                        })
                    };
                    // Like the real supervisor: per-call usage precedes the result.
                    let usage = json!({
                        "t": "event", "request_id": request.request_id, "kind": "usage",
                        "payload": {"wall_ms": 1, "cpu_ms": 7, "peak_rss_kb": 2048, "worker_pid": 42},
                    });
                    let mut writer = writer.lock().await;
                    let _ = writer.send(&usage).await;
                    let _ = writer.send(&reply).await;
                });
                None
            }
            HostMessage::Secret {
                request_id, name, ..
            } => {
                staged.entry(request_id).or_default().push(name);
                None
            }
            HostMessage::Cancel { .. } => {
                cancels.fetch_add(1, Ordering::SeqCst);
                None
            }
        };
        if let Some(reply) = reply
            && writer.lock().await.send(&reply).await.is_err()
        {
            break;
        }
    }
}

pub struct Fixture {
    pub host: Arc<ToolHost>,
    pub snapshot: ExternalToolSnapshot,
    pub bytes: Vec<u8>,
    pub vm: Arc<FakeVms>,
}

pub fn build(
    descriptor: &baml_rt_tools::external_tools::UnifiedToolDescriptor,
    requested: CapabilityRequest,
    policy: ToolPolicy,
    ctx: &ToolSessionContext,
    pool_policy: PoolPolicy,
    memory_mib: u32,
) -> Fixture {
    let bytes = bundle(descriptor, requested.clone());
    let audited = baml_rt_tools::external_tools::audit_ktool_bundle(&bytes).unwrap();
    let snapshot = ExternalToolSnapshot::from_v2_bundle(
        descriptor.clone(),
        requested,
        audited.bundle_digest,
        audited.tool_digest,
        audited.manifest.runtime_image.clone(),
        "operator",
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    let vm = Arc::new(FakeVms {
        created: AtomicUsize::new(0),
        teardowns: AtomicUsize::new(0),
        cancels: Arc::new(AtomicUsize::new(0)),
        hosts: std::sync::Mutex::new(Vec::new()),
        owners: std::sync::Mutex::new(Vec::new()),
        sweeps: std::sync::Mutex::new(Vec::new()),
    });
    let pool = VmPool::new(vm.clone(), "test-runner", pool_policy);
    let mut host_policy = HostPolicy {
        memory_mib,
        ..HostPolicy::default()
    };
    host_policy
        .tools
        .insert((ctx.agent_id.to_string(), audited.tool_digest), policy);
    Fixture {
        host: Arc::new(ToolHost::new(pool, host_policy, resolver()).unwrap()),
        snapshot,
        bytes,
        vm,
    }
}

pub fn fixture(
    requested: CapabilityRequest,
    policy: ToolPolicy,
    secret: &str,
) -> (Fixture, ToolSessionContext) {
    let ctx = session_ctx(secret);
    let f = build(
        &descriptor(TOOL),
        requested.clone(),
        policy,
        &ctx,
        PoolPolicy::default(),
        512,
    );
    (f, ctx)
}

pub async fn invoke(f: &Fixture, ctx: &ToolSessionContext, input: Value) -> Value {
    match f
        .host
        .invoke(&f.snapshot, &f.bytes, ctx, Default::default(), input)
        .await
        .unwrap()
    {
        GuestMessage::Result {
            ok: true, output, ..
        } => output.unwrap_or(Value::Null),
        other => panic!("unexpected supervisor reply: {other:?}"),
    }
}

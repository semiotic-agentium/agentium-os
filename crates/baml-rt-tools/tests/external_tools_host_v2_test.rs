// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! P3 host authority: grant intersection, VM reuse, budgets, and v2 invocation.

use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use baml_rt_core::{
    BamlRtError, Result,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_tools::{
    ToolName, ToolSecretResolver, ToolSecretValue, ToolSecrets, ToolSessionContext,
    external_tools::{
        CapabilityRequest, ExternalToolSnapshot, HostPort, SecretBinding, SecretRequest,
        host::{
            HostPolicy, HostResolver, PoolPolicy, ToolHost, ToolPolicy, TrustDomain, VmBudget,
            VmPool, VmSpec,
        },
        sandbox::{SandboxEvent, SandboxHandle, SandboxProvider, SandboxSpec, TsrpcChannel},
    },
    tool_fsm::{ToolSessionId, ToolStep},
    tools::{SecretRequest as ToolSecretRequest, ToolHandler},
};
use baml_sandbox_protocol::{
    codec::{FrameReader, FrameWriter},
    protocol_v2::{GuestMessage, HostMessage},
};
use futures_util::stream::{self, BoxStream};
use serde_json::{Value, json};
use test_support::external_tools_v2::{bundle, descriptor};

const SECRET: &str = "METEO_KEY";
const TOOL: &str = "support/host_fixture";

/// Deterministic host resolution; tests never touch DNS.
struct StaticResolver(HashMap<&'static str, Vec<IpAddr>>);

#[async_trait]
impl HostResolver for StaticResolver {
    async fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>> {
        Ok(self.0.get(host).cloned().unwrap_or_default())
    }
}

fn resolver() -> Arc<StaticResolver> {
    Arc::new(StaticResolver(HashMap::from([
        ("api.example.com", vec!["1.1.1.1".parse().unwrap()]),
        (
            "metadata.example.com",
            vec!["169.254.169.254".parse().unwrap()],
        ),
    ])))
}

struct FixedSecret(String);

impl ToolSecretResolver for FixedSecret {
    fn resolve_tool_secret(&self, name: &str) -> Option<ToolSecretValue> {
        (name == SECRET).then(|| ToolSecretValue::new(self.0.clone()))
    }
}

fn session_ctx(value: &str) -> ToolSessionContext {
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

fn net(host: &str) -> CapabilityRequest {
    CapabilityRequest {
        net: vec![HostPort {
            host: host.into(),
            port: 443,
        }],
        secrets: vec![],
    }
}

fn full_request() -> CapabilityRequest {
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

fn policy(capabilities: CapabilityRequest) -> ToolPolicy {
    ToolPolicy {
        trust_domain: TrustDomain::ThirdParty,
        capabilities,
        secret_hosts: BTreeMap::from([(SECRET.to_string(), vec!["api.example.com".to_string()])]),
    }
}

/// Fake supervisor over a real duplex: speaks the v2 enum, never a VM.
struct FakeVms {
    created: AtomicUsize,
    teardowns: AtomicUsize,
    cancels: Arc<AtomicUsize>,
}

#[async_trait]
impl SandboxProvider for FakeVms {
    async fn create_vm(&self, spec: VmSpec) -> Result<(SandboxHandle, TsrpcChannel)> {
        self.created.fetch_add(1, Ordering::SeqCst);
        let (host, guest) = tokio::io::duplex(64 * 1024);
        tokio::spawn(supervisor(guest, self.cancels.clone()));
        let (reader, writer) = tokio::io::split(host);
        Ok((
            SandboxHandle::new(spec.name, Duration::from_secs(60)),
            TsrpcChannel::new(reader, writer),
        ))
    }

    async fn create(&self, _: SandboxSpec) -> Result<SandboxHandle> {
        Err(BamlRtError::InvalidArgument(
            "create is not a v2 path".into(),
        ))
    }

    async fn rpc_channel(&self, _: &SandboxHandle) -> Result<TsrpcChannel> {
        Err(BamlRtError::InvalidArgument(
            "rpc_channel is not a v2 path".into(),
        ))
    }

    async fn teardown(&self, _: &SandboxHandle) -> Result<()> {
        self.teardowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn events(&self, _: &SandboxHandle) -> BoxStream<'_, SandboxEvent> {
        Box::pin(stream::empty())
    }

    async fn list_owned(&self, _: &str) -> Result<Vec<SandboxHandle>> {
        Ok(Vec::new())
    }

    async fn reattach(&self, _: &str) -> Result<SandboxHandle> {
        Err(BamlRtError::InvalidArgument(
            "reattach is not a v2 path".into(),
        ))
    }
}

/// Replies per request_id, concurrently: proves the host multiplexes one stream.
async fn supervisor(guest: tokio::io::DuplexStream, cancels: Arc<AtomicUsize>) {
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
                    let _ = writer.lock().await.send(&reply).await;
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

struct Fixture {
    host: Arc<ToolHost>,
    snapshot: ExternalToolSnapshot,
    bytes: Vec<u8>,
    vm: Arc<FakeVms>,
}

fn build(
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

fn fixture(
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

async fn invoke(f: &Fixture, ctx: &ToolSessionContext, input: Value) -> Value {
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

/// One row per refusal reason. Each row is a distinct authority boundary.
#[tokio::test]
async fn tool_grant_denial_matrix() {
    struct Case {
        label: &'static str,
        requested: CapabilityRequest,
        policy: ToolPolicy,
        secret: &'static str,
        substituted: Option<CapabilityRequest>,
        want: &'static str,
    }

    let full = full_request();
    let mut widened = policy(full.clone());
    widened.capabilities.net.clear();
    let mut unbound = policy(full.clone());
    unbound.secret_hosts = BTreeMap::new();

    let cases = [
        Case {
            label: "network widening",
            requested: full.clone(),
            policy: widened,
            secret: "s3cret",
            substituted: None,
            want: "outside agent policy",
        },
        Case {
            label: "secret without host binding",
            requested: full.clone(),
            policy: unbound,
            secret: "s3cret",
            substituted: None,
            want: "missing egress binding",
        },
        Case {
            label: "secret host outside the network grant",
            requested: CapabilityRequest {
                net: vec![],
                secrets: full.secrets.clone(),
            },
            policy: policy(CapabilityRequest {
                net: vec![],
                secrets: full.secrets.clone(),
            }),
            secret: "s3cret",
            substituted: None,
            want: "approved HTTPS destination",
        },
        Case {
            label: "metadata address",
            requested: net("metadata.example.com"),
            policy: policy(net("metadata.example.com")),
            secret: "s3cret",
            substituted: None,
            want: "forbidden address",
        },
        Case {
            label: "no DNS answer",
            requested: net("unresolved.example.com"),
            policy: policy(net("unresolved.example.com")),
            secret: "s3cret",
            substituted: None,
            want: "no addresses",
        },
        Case {
            label: "missing agent secret",
            requested: full.clone(),
            policy: policy(full.clone()),
            secret: "",
            substituted: None,
            want: "missing agent-scoped secret",
        },
        Case {
            label: "bundle does not match the approved snapshot",
            requested: full.clone(),
            policy: policy(full.clone()),
            secret: "s3cret",
            substituted: Some(net("api.example.com")),
            want: "does not match",
        },
    ];

    for Case {
        label,
        requested,
        policy,
        secret,
        substituted,
        want,
    } in cases
    {
        let (mut f, ctx) = fixture(requested, policy, secret);
        if let Some(other_requested) = substituted {
            let mut other = descriptor("support/other_fixture");
            other.description = TOOL.into();
            f.bytes = bundle(&other, other_requested);
        }
        let err = f
            .host
            .authorize(&f.snapshot, &f.bytes, &ctx)
            .await
            .expect_err(label);
        assert!(err.to_string().contains(want), "{label}: {err}");
    }
}

#[tokio::test]
async fn expired_approval_is_refused() {
    let requested = full_request();
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");
    let mut snapshot = f.snapshot.clone();
    snapshot.approval.expires_at = Some("2025-01-01T00:00:00Z".into());
    let err = f
        .host
        .authorize(&snapshot, &f.bytes, &ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
}

#[tokio::test]
async fn unauthorized_agent_is_refused() {
    let requested = net("api.example.com");
    let (f, _owner) = fixture(requested.clone(), policy(requested), "s3cret");
    let err = f
        .host
        .authorize(&f.snapshot, &f.bytes, &session_ctx("s3cret"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not authorized"), "{err}");
}

#[tokio::test]
async fn vm_is_reused_within_a_grant_class_and_split_on_secret_rotation() {
    let requested = full_request();
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");

    assert_eq!(invoke(&f, &ctx, json!({"q": 1})).await["echo"]["q"], 1);
    assert_eq!(invoke(&f, &ctx, json!({"q": 2})).await["echo"]["q"], 2);
    assert_eq!(
        f.vm.created.load(Ordering::SeqCst),
        1,
        "warm VM must be reused"
    );

    let mut rotated = session_ctx("rotated");
    rotated.agent_id = ctx.agent_id.clone();
    invoke(&f, &rotated, json!({"q": 3})).await;
    assert_eq!(
        f.vm.created.load(Ordering::SeqCst),
        2,
        "rotation must not reuse a VM"
    );
}

#[tokio::test]
async fn idle_vms_are_evicted_before_the_budget_is_exceeded() {
    let requested = full_request();
    let ctx = session_ctx("first");
    let f = build(
        &descriptor(TOOL),
        requested.clone(),
        policy(requested),
        &ctx,
        PoolPolicy {
            total: VmBudget {
                count: 1,
                memory_mib: 512,
            },
            ..PoolPolicy::default()
        },
        512,
    );

    invoke(&f, &ctx, json!({"q": 1})).await;
    let mut second = session_ctx("second");
    second.agent_id = ctx.agent_id.clone();
    invoke(&f, &second, json!({"q": 2})).await;

    assert_eq!(
        f.vm.created.load(Ordering::SeqCst),
        2,
        "a new grant class needs its own VM"
    );
    assert_eq!(
        f.vm.teardowns.load(Ordering::SeqCst),
        1,
        "the idle VM is evicted, not orphaned"
    );
}

#[tokio::test]
async fn concurrent_calls_share_one_stream_without_crossing_requests() {
    let requested = net("api.example.com");
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");

    let slow = invoke(&f, &ctx, json!({"q": "slow", "delay": 100}));
    let fast = invoke(&f, &ctx, json!({"q": "fast"}));
    let (slow, fast) = tokio::join!(slow, fast);

    assert_eq!(slow["echo"]["q"], "slow");
    assert_eq!(fast["echo"]["q"], "fast");
    assert_eq!(f.vm.created.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn timed_out_call_does_not_retire_shared_channel() {
    let requested = net("api.example.com");
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");
    // Boot and load the shared VM before putting one call on a short deadline.
    invoke(&f, &ctx, json!({"q": "warm"})).await;
    let deadline_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 150;
    let slow = f.host.invoke(
        &f.snapshot,
        &f.bytes,
        &ctx,
        baml_sandbox_protocol::protocol_v2::CallContext {
            deadline_ms,
            ..Default::default()
        },
        json!({"q": "slow", "delay": 500}),
    );
    let fast = f.host.invoke(
        &f.snapshot,
        &f.bytes,
        &ctx,
        Default::default(),
        json!({"q": "fast"}),
    );
    let (slow, fast) = tokio::join!(slow, fast);
    assert!(slow.is_err(), "the slow call must hit its deadline");
    let fast = fast.unwrap();
    match fast {
        GuestMessage::Result {
            ok: true, output, ..
        } => assert_eq!(output.unwrap()["echo"]["q"], "fast"),
        other => panic!("unexpected fast result: {other:?}"),
    }

    // The fake supervisor still sends the timed-out reply. The reader must
    // ignore that stale request and keep routing later calls.
    tokio::time::sleep(Duration::from_millis(550)).await;
    assert!(f.vm.cancels.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        invoke(&f, &ctx, json!({"q": "later"})).await["echo"]["q"],
        "later"
    );
    assert_eq!(f.vm.created.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oversized_vm_request_is_refused_before_boot() {
    let requested = net("api.example.com");
    let ctx = session_ctx("s3cret");
    let f = build(
        &descriptor(TOOL),
        requested.clone(),
        policy(requested),
        &ctx,
        PoolPolicy {
            total: VmBudget {
                count: 1,
                memory_mib: 64,
            },
            ..PoolPolicy::default()
        },
        128,
    );
    let err = f
        .host
        .invoke(&f.snapshot, &f.bytes, &ctx, Default::default(), json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds pool budget"), "{err}");
    assert_eq!(f.vm.created.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn handler_round_trips_success_and_tool_errors() {
    let requested = net("api.example.com");
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");
    let handler = f
        .host
        .handler(f.snapshot.clone(), Arc::from(f.bytes.clone()))
        .unwrap();

    let mut session = handler.open_session(ctx.clone(), json!({})).await.unwrap();
    session.send(json!({"q": 1})).await.unwrap();
    match session.read(json!({})).await.unwrap() {
        ToolStep::Done { output } => assert_eq!(output.unwrap()["echo"]["q"], 1),
        other => panic!("expected Done, got {other:?}"),
    }

    session.send(json!({"fail": true})).await.unwrap();
    let err = session.read(json!({})).await.unwrap_err();
    assert!(format!("{err:?}").contains("boom"), "{err:?}");
}

#[tokio::test]
async fn shutdown_tears_down_every_vm() {
    let requested = net("api.example.com");
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");
    invoke(&f, &ctx, json!({})).await;
    f.host.shutdown().await.unwrap();
    assert_eq!(f.vm.teardowns.load(Ordering::SeqCst), 1);
}

fn reveal_request() -> CapabilityRequest {
    CapabilityRequest {
        net: vec![],
        secrets: vec![SecretRequest {
            name: SECRET.into(),
            binding: SecretBinding::Reveal,
        }],
    }
}

fn reveal_policy() -> ToolPolicy {
    ToolPolicy {
        trust_domain: TrustDomain::ThirdParty,
        capabilities: reveal_request(),
        // Reveal secrets do not need an egress host binding.
        secret_hosts: BTreeMap::new(),
    }
}

/// P4.7: a reveal secret is staged as a control frame before the invoke and
/// never appears in the invoke payload.
#[tokio::test]
async fn reveal_secret_is_delivered_as_a_control_frame() {
    let (f, ctx) = fixture(reveal_request(), reveal_policy(), "s3cret-value");
    let output = invoke(&f, &ctx, json!({"query": "x"})).await;
    assert_eq!(output["reveals"], json!([SECRET]));
    let serialized = serde_json::to_string(&output).unwrap();
    assert!(
        !serialized.contains("s3cret-value"),
        "the value must not appear in the invoke result: {serialized}"
    );
}

#[tokio::test]
async fn reveal_secret_must_be_approved_by_policy() {
    let unapproved = ToolPolicy {
        trust_domain: TrustDomain::ThirdParty,
        capabilities: CapabilityRequest::default(),
        secret_hosts: BTreeMap::new(),
    };
    let (f, ctx) = fixture(reveal_request(), unapproved, "s3cret-value");
    let err = f
        .host
        .authorize(&f.snapshot, &f.bytes, &ctx)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("outside agent policy"),
        "unexpected error: {err}"
    );
}

/// Rotating a reveal value must change the grant class, so a VM that staged the
/// old value is never reused.
#[tokio::test]
async fn reveal_rotation_changes_the_grant_class() {
    let (f, ctx) = fixture(reveal_request(), reveal_policy(), "first-value");
    let first = f.host.authorize(&f.snapshot, &f.bytes, &ctx).await.unwrap();
    let mut rotated = session_ctx("second-value");
    rotated.agent_id = ctx.agent_id.clone();
    let second = f
        .host
        .authorize(&f.snapshot, &f.bytes, &rotated)
        .await
        .unwrap();
    assert_ne!(first.key().grant_class, second.key().grant_class);
    assert_eq!(first.reveal_secrets().len(), 1);
}

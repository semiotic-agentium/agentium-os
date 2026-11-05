// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! P3 host authority: grant intersection, VM reuse, budgets, and v2 invocation.

mod support;

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use baml_rt_tools::{
    external_tools::{
        CapabilityRequest, HostPort, SecretBinding, SecretRequest,
        host::{PoolPolicy, ToolPolicy, TrustDomain, VmBudget, VmPool},
    },
    tool_fsm::ToolStep,
    tools::ToolHandler,
};
use baml_sandbox_protocol::protocol_v2::GuestMessage;
use serde_json::json;
use support::host_v2::*;
use test_support::external_tools_v2::{bundle, descriptor};

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
async fn undeploy_releases_shared_vm_after_the_last_agent() {
    let requested = full_request();
    let ctx_a = session_ctx("shared-secret");
    let ctx_b = session_ctx("shared-secret");
    let f = build_for_agents(
        &descriptor(TOOL),
        requested.clone(),
        policy(requested),
        &[&ctx_a, &ctx_b],
        PoolPolicy::default(),
        512,
    );

    invoke(&f, &ctx_a, json!({"agent": "a"})).await;
    invoke(&f, &ctx_b, json!({"agent": "b"})).await;
    assert_eq!(f.vm.created.load(Ordering::SeqCst), 1);

    f.host
        .release_agent(&ctx_a.agent_id.to_string())
        .await
        .unwrap();
    assert_eq!(
        f.vm.teardowns.load(Ordering::SeqCst),
        0,
        "a shared VM must survive while agent B remains deployed"
    );

    f.host
        .release_agent(&ctx_b.agent_id.to_string())
        .await
        .unwrap();
    assert_eq!(f.vm.teardowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn undeploy_does_not_touch_another_agents_vm() {
    let requested = full_request();
    let ctx_a = session_ctx("agent-a-secret");
    let ctx_b = session_ctx("agent-b-secret");
    let f = build_for_agents(
        &descriptor(TOOL),
        requested.clone(),
        policy(requested),
        &[&ctx_a, &ctx_b],
        PoolPolicy::default(),
        512,
    );

    invoke(&f, &ctx_a, json!({"agent": "a"})).await;
    invoke(&f, &ctx_b, json!({"agent": "b"})).await;
    assert_eq!(f.vm.created.load(Ordering::SeqCst), 2);

    f.host
        .release_agent(&ctx_a.agent_id.to_string())
        .await
        .unwrap();
    assert_eq!(
        f.vm.teardowns.load(Ordering::SeqCst),
        1,
        "undeploying A must only tear down A's grant-class VM"
    );

    f.host
        .release_agent(&ctx_b.agent_id.to_string())
        .await
        .unwrap();
    assert_eq!(f.vm.teardowns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn releasing_during_a_call_waits_for_the_call_to_finish() {
    let requested = net("api.example.com");
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");
    let host = f.host.clone();
    let snapshot = f.snapshot.clone();
    let bytes = f.bytes.clone();
    let call_ctx = ctx.clone();
    let call = tokio::spawn(async move {
        host.invoke(
            &snapshot,
            &bytes,
            &call_ctx,
            Default::default(),
            json!({"q": "draining", "delay": 100}),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(f.vm.created.load(Ordering::SeqCst), 1);

    f.host
        .release_agent(&ctx.agent_id.to_string())
        .await
        .unwrap();
    assert_eq!(
        f.vm.teardowns.load(Ordering::SeqCst),
        0,
        "an in-flight call must keep its VM alive"
    );

    call.await.unwrap().unwrap();
    f.host.shutdown().await.unwrap();
    assert_eq!(f.vm.teardowns.load(Ordering::SeqCst), 1);
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

/// The VM resolves declared hosts from a pinned `/etc/hosts` (no guest DNS),
/// so the grant must carry exactly the addresses the CIDR policy allows.
#[tokio::test]
async fn grant_pins_declared_hosts_to_their_allowed_addresses() {
    let requested = net("api.example.com");
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");
    invoke(&f, &ctx, json!({"q": "hosts"})).await;
    let hosts = f.vm.hosts.lock().unwrap();
    assert_eq!(hosts.len(), 1);
    let ips: Vec<String> = hosts[0]["api.example.com"]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(ips, ["1.1.1.1"]);
    assert_eq!(hosts[0].len(), 1, "only declared hosts are pinned");
}

/// A host an egress secret is bound to is resolved by the VM's DNS forwarder
/// (the TLS identity the secret check needs), so it is allowed by name and
/// never pinned in `/etc/hosts`.
#[tokio::test]
async fn secret_bound_hosts_resolve_by_name_not_from_pinned_hosts() {
    let requested = full_request();
    let (f, ctx) = fixture(requested.clone(), policy(requested), "s3cret");
    invoke(&f, &ctx, json!({"q": "dns"})).await;
    let hosts = f.vm.hosts.lock().unwrap();
    assert!(
        !hosts[0].contains_key("api.example.com"),
        "a secret-bound host must not be pinned"
    );
    let dns_hosts = f.vm.dns_hosts.lock().unwrap();
    assert_eq!(
        dns_hosts[0],
        [HostPort {
            host: "api.example.com".into(),
            port: 443,
        }]
    );
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
        // The fake supervisor does not enforce deadlines, so outlast the
        // host's guest-deadline grace (2s) to exercise the host backstop.
        json!({"q": "slow", "delay": 2400}),
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

/// A restarted runner reclaims only what its own identity left behind, and
/// every VM it creates carries that identity for the next restart's sweep.
#[tokio::test]
async fn pool_start_sweeps_its_own_leftovers_and_owns_new_vms() {
    let fake = Arc::new(FakeVms {
        created: AtomicUsize::new(0),
        teardowns: AtomicUsize::new(0),
        cancels: Arc::new(AtomicUsize::new(0)),
        hosts: std::sync::Mutex::new(Vec::new()),
        dns_hosts: std::sync::Mutex::new(Vec::new()),
        owners: std::sync::Mutex::new(Vec::new()),
        sweeps: std::sync::Mutex::new(Vec::new()),
    });
    let pool = VmPool::start(fake.clone(), "runner-a", PoolPolicy::default()).await;
    assert_eq!(pool.runner_id(), "runner-a");
    assert_eq!(*fake.sweeps.lock().unwrap(), vec!["runner-a".to_string()]);
    // `new` (tests, library hosts) never sweeps.
    let _other = VmPool::new(fake.clone(), "runner-b", PoolPolicy::default());
    assert_eq!(fake.sweeps.lock().unwrap().len(), 1);

    let (f, ctx) = fixture(full_request(), policy(full_request()), "s3cret");
    invoke(&f, &ctx, json!({"q": 1})).await;
    assert_eq!(
        *f.vm.owners.lock().unwrap(),
        vec!["test-runner".to_string()]
    );
}

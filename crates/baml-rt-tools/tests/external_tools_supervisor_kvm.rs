// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P4.11 live lane: the P3 `ToolHost` against the real guest supervisor inside
//! a real microVM, plus the P0 isolation matrix.
//!
//! Enable with `--features sandbox-e2e-kvm` on a KVM host and point
//! `KARAKARA_KVM_IMAGE` at the digest-pinned `node` runtime image built by
//! `just tool-runtime-image`. Skipped otherwise so ordinary CI never pays for
//! it.

#![cfg(feature = "sandbox-e2e-kvm")]

use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use baml_ktool::{CapabilityRequest, DESCRIPTOR_ABI, OutputTrust, SideEffect, ToolDescriptor};
use baml_rt_core::{
    Result,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_tools::{
    ToolName, ToolSecretResolver, ToolSecretValue, ToolSecrets, ToolSessionContext,
    external_tools::{
        ExternalToolSnapshot, build_ktool_bundle, default_node_runtime_image,
        host::{HostPolicy, HostResolver, PoolPolicy, ToolHost, ToolPolicy, TrustDomain, VmPool},
        sandbox::MicrosandboxProvider,
    },
    tool_fsm::ToolSessionId,
    tools::SecretRequest as ToolSecretRequest,
};
use baml_sandbox_protocol::protocol_v2::{CallContext, GuestMessage};
use serde_json::{Value, json};

const TOOL: &str = "test/kvm-supervisor";

const TOOL_JS: &str = r#"
import fs from "node:fs";
import { defineTool, Type } from "@karakara/tool";

export default defineTool(
  {
    name: "test/kvm-supervisor",
    version: "0.1.0",
    description: "KVM supervisor probe",
    sideEffect: "read",
    input: Type.Any(),
    output: Type.Any(),
  },
  async (input) => {
    const out = { pid: process.pid };
    if (input.echo !== undefined) out.echo = input.echo;
    if (input.sleep_ms) await new Promise((r) => setTimeout(r, input.sleep_ms));
    if (input.exists) {
      out.exists = Object.fromEntries(input.exists.map((p) => [p, fs.existsSync(p)]));
    }
    return out;
  },
);
"#;

struct StaticResolver;

#[async_trait]
impl HostResolver for StaticResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>> {
        Ok(Vec::new())
    }
}

struct NoSecrets;

impl ToolSecretResolver for NoSecrets {
    fn resolve_tool_secret(&self, _name: &str) -> Option<ToolSecretValue> {
        None
    }
}

fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        descriptor_abi: DESCRIPTOR_ABI,
        name: TOOL.into(),
        version: "0.1.0".into(),
        description: "KVM supervisor probe".into(),
        input_schema: json!({}),
        output_schema: json!({}),
        error_classes: vec![],
        side_effect: SideEffect::Read,
        idempotent: true,
        output_trust: OutputTrust::Untrusted,
    }
}

fn session_ctx() -> ToolSessionContext {
    ToolSessionContext {
        session_id: ToolSessionId::random(),
        tool_name: ToolName::parse(TOOL).unwrap(),
        context_id: ContextId::new(1, 1),
        agent_id: AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4())),
        config: None,
        config_version: None,
        secrets: ToolSecrets::scoped(
            Some(Arc::new(NoSecrets)),
            &[ToolSecretRequest::api_key("UNUSED", "kvm", "kvm")],
        ),
        task_id: None,
        execution_classifier: None,
    }
}

#[tokio::test]
async fn real_vm_supervisor_runs_the_isolation_matrix() {
    let Ok(image) = std::env::var("KARAKARA_KVM_IMAGE") else {
        eprintln!("skipping: set KARAKARA_KVM_IMAGE to a digest-pinned node runtime image");
        return;
    };
    if !Path::new("/dev/kvm").exists() {
        eprintln!("skipping: /dev/kvm is not available");
        return;
    }

    let provider = MicrosandboxProvider::new()
        .expect("provider")
        .with_runtime_image(default_node_runtime_image(), image)
        .expect("register runtime image");
    let pool = VmPool::new(Arc::new(provider), "kvm-test", PoolPolicy::default());
    let ctx = session_ctx();
    let bytes = bundle();
    let audited = baml_rt_tools::external_tools::audit_ktool_bundle(&bytes).expect("audit");
    let policy = ToolPolicy {
        trust_domain: TrustDomain::ThirdParty,
        capabilities: CapabilityRequest::default(),
        secret_hosts: BTreeMap::new(),
    };
    let mut host_policy = HostPolicy::default();
    host_policy
        .tools
        .insert((ctx.agent_id.to_string(), audited.tool_digest), policy);
    let host = Arc::new(
        ToolHost::new(pool.clone(), host_policy, Arc::new(StaticResolver)).expect("tool host"),
    );

    let descriptor = descriptor();
    let snapshot = ExternalToolSnapshot::from_v2_bundle(
        descriptor,
        CapabilityRequest::default(),
        audited.bundle_digest,
        audited.tool_digest,
        audited.manifest.runtime_image,
        "kvm",
        "2026-01-01T00:00:00Z",
    )
    .expect("snapshot");

    // 1. Real invoke through the real supervisor and runtime image.
    let echo = invoke(
        &host,
        &snapshot,
        &bytes,
        &ctx,
        json!({"echo": "kvm"}),
        10_000,
    )
    .await;
    assert_eq!(echo["echo"], "kvm");

    // 2. Host filesystem is not reachable.
    let canary = std::env::temp_dir().join(format!("karakara-kvm-canary-{}", std::process::id()));
    std::fs::write(&canary, b"host-only").expect("canary");
    let canary_path = canary.to_string_lossy().into_owned();
    let probe = invoke(
        &host,
        &snapshot,
        &bytes,
        &ctx,
        json!({"exists": [canary_path, "/etc/shadow"]}),
        10_000,
    )
    .await;
    assert_eq!(probe["exists"][&canary_path], false);
    assert_eq!(probe["exists"]["/etc/shadow"], false);
    let _ = std::fs::remove_file(&canary);

    // 3. Deadline is enforced inside the VM and the channel survives.
    let started = std::time::Instant::now();
    let deadline_ms = now_ms() + 500;
    let timed_out = host
        .invoke(
            &snapshot,
            &bytes,
            &ctx,
            CallContext {
                deadline_ms,
                ..Default::default()
            },
            json!({"sleep_ms": 30_000}),
        )
        .await
        .expect("deadline call returns a result");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "supervisor did not enforce the deadline"
    );
    match timed_out {
        GuestMessage::Result {
            ok: false,
            error_class,
            ..
        } => assert_eq!(error_class, baml_sandbox_protocol::ErrorClass::Transient),
        other => panic!("expected a deadline failure, got {other:?}"),
    }
    let alive = invoke(
        &host,
        &snapshot,
        &bytes,
        &ctx,
        json!({"echo": "alive"}),
        10_000,
    )
    .await;
    assert_eq!(alive["echo"], "alive");

    host.shutdown().await.expect("shutdown");
}

async fn invoke(
    host: &ToolHost,
    snapshot: &ExternalToolSnapshot,
    bytes: &[u8],
    ctx: &ToolSessionContext,
    input: Value,
    timeout_ms: u64,
) -> Value {
    let context = CallContext {
        deadline_ms: now_ms() + timeout_ms,
        ..Default::default()
    };
    match host
        .invoke(snapshot, bytes, ctx, context, input)
        .await
        .expect("invoke")
    {
        GuestMessage::Result {
            ok: true, output, ..
        } => output.unwrap_or(Value::Null),
        other => panic!("unexpected supervisor result: {other:?}"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

fn bundle() -> Vec<u8> {
    build_ktool_bundle(
        &descriptor(),
        TOOL_JS.as_bytes().to_vec(),
        vec![],
        br#"{}"#.to_vec(),
    )
    .expect("bundle")
}

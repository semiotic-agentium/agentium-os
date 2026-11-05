// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P4.11 live lane: the P3 `ToolHost` against the real guest supervisor inside
//! a real microVM, plus the P0 isolation matrix and the egress-bound secret
//! promise (§8), all in one VM to keep the lane cheap.
//!
//! The egress checks reach the public internet: `postman-echo.com` is the
//! secret's bound host (it echoes request headers back), `example.com` is an
//! allowed destination the secret is not bound to. The secret value is fake.
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
use baml_ktool::{
    CapabilityRequest, DESCRIPTOR_ABI, HostPort, OutputTrust, SecretBinding,
    SecretRequest as KtoolSecretRequest, SideEffect, ToolDescriptor,
};
use baml_rt_core::{
    Result,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_tools::{
    ToolName, ToolSecretResolver, ToolSecretValue, ToolSecrets, ToolSessionContext,
    external_tools::{
        ExternalToolSnapshot, build_ktool_bundle, default_node_runtime_image,
        host::{
            DnsResolver, HostPolicy, HostResolver, PoolPolicy, ToolHost, ToolPolicy, TrustDomain,
            VmPool,
        },
        sandbox::MicrosandboxProvider,
    },
    tool_fsm::ToolSessionId,
    tools::SecretRequest as ToolSecretRequest,
};
use baml_sandbox_protocol::protocol_v2::{CallContext, GuestMessage};
use serde_json::{Value, json};

const TOOL: &str = "test/kvm-supervisor";
/// Egress-bound secret env name, its bound host, and an allowed unbound host.
const SECRET: &str = "KVM_EGRESS_SECRET";
const BOUND_HOST: &str = "postman-echo.com";
const UNBOUND_HOST: &str = "example.com";
/// Header the probe puts the placeholder in.
const PROBE_HEADER: &str = "x-karakara-probe";

const TOOL_JS: &str = r#"
import fs from "node:fs";
import dns from "node:dns/promises";

// MCP stdio transport: newline-delimited JSON-RPC (no Content-Length headers).
let buffer = "";
let next = Promise.resolve();

function send(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

async function handle(message) {
  if (message.id === undefined) return;
  if (message.method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "kvm-supervisor", version: "0.1.0" },
      },
    });
    return;
  }
  if (message.method === "tools/list") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        tools: [{
          name: "kvm-supervisor",
          description: "KVM supervisor probe",
          inputSchema: { type: "object" },
          outputSchema: { type: "object" },
          annotations: { readOnlyHint: true, idempotentHint: true },
        }],
      },
    });
    return;
  }
  if (message.method === "tools/call") {
    const input = message.params?.arguments ?? {};
    const out = { pid: process.pid };
    if (input.echo !== undefined) out.echo = input.echo;
    if (input.sleep_ms) await new Promise((resolve) => setTimeout(resolve, input.sleep_ms));
    if (input.exists) {
      out.exists = Object.fromEntries(input.exists.map((p) => [p, fs.existsSync(p)]));
    }
    if (input.readable) {
      out.readable = Object.fromEntries(input.readable.map((p) => {
        try { fs.readFileSync(p); return [p, true]; } catch { return [p, false]; }
      }));
    }
    if (input.env) {
      out.env = Object.fromEntries(input.env.map((n) => [n, process.env[n] ?? null]));
      out.env_values = Object.values(process.env);
    }
    if (input.lookup) {
      out.lookup = {};
      for (const name of input.lookup) {
        try { out.lookup[name] = (await dns.lookup(name, { all: true })).map((a) => a.address); }
        catch (e) { out.lookup[name] = { error: String(e?.code ?? e) }; }
      }
    }
    if (input.fetch) {
      out.fetch = [];
      for (const req of input.fetch) {
        const headers = {};
        if (req.secret) headers[req.header] = process.env[req.secret] ?? "";
        try {
          const res = await fetch(req.url, { headers, signal: AbortSignal.timeout(10000) });
          out.fetch.push({ status: res.status, body: await res.text() });
        } catch (e) {
          out.fetch.push({ error: String(e?.cause?.code ?? e?.cause?.message ?? e?.message ?? e) });
        }
      }
    }
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        content: [{ type: "text", text: JSON.stringify(out) }],
        structuredContent: out,
      },
    });
  }
}

process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let newline;
  while ((newline = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, newline).trim();
    buffer = buffer.slice(newline + 1);
    if (!line) continue;
    const message = JSON.parse(line);
    next = next.then(() => handle(message));
  }
});
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

/// Resolves [`SECRET`] to a fixed fake value.
struct FakeSecret(String);

impl ToolSecretResolver for FakeSecret {
    fn resolve_tool_secret(&self, name: &str) -> Option<ToolSecretValue> {
        (name == SECRET).then(|| ToolSecretValue::new(self.0.clone()))
    }
}

/// One egress-bound secret plus HTTPS to its bound host and to an unbound one.
fn egress_capabilities() -> CapabilityRequest {
    CapabilityRequest {
        net: [BOUND_HOST, UNBOUND_HOST]
            .map(|host| HostPort {
                host: host.into(),
                port: 443,
            })
            .to_vec(),
        secrets: vec![KtoolSecretRequest {
            name: SECRET.into(),
            binding: SecretBinding::EgressBound,
        }],
    }
}

fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        descriptor_abi: DESCRIPTOR_ABI,
        name: TOOL.into(),
        version: "0.1.0".into(),
        description: "KVM supervisor probe".into(),
        // Must match what TOOL_JS advertises in tools/list: the supervisor
        // re-verifies the live descriptor against the audited one at load.
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        // An MCP tool without `errorClasses` annotations maps to [Execution].
        error_classes: vec![baml_sandbox_protocol::ErrorClass::Execution],
        side_effect: SideEffect::Read,
        idempotent: true,
        output_trust: OutputTrust::Untrusted,
    }
}

fn session_ctx() -> ToolSessionContext {
    session_ctx_with(ToolSecrets::scoped(
        Some(Arc::new(NoSecrets)),
        &[ToolSecretRequest::api_key("UNUSED", "kvm", "kvm")],
    ))
}

fn session_ctx_with(secrets: ToolSecrets) -> ToolSessionContext {
    ToolSessionContext {
        session_id: ToolSessionId::random(),
        tool_name: ToolName::parse(TOOL).unwrap(),
        context_id: ContextId::new(1, 1),
        agent_id: AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4())),
        config: None,
        config_version: None,
        secrets,
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
    // Unique per run so the owner-label assertions see only this test's VMs.
    let owner = format!("kvm-test-{}", uuid::Uuid::new_v4());
    cleaning_up_vms(&[&owner], async {
        let pool = VmPool::new(Arc::new(provider), owner.clone(), PoolPolicy::default());
        // The VM is created with the egress grant so the isolation checks run in
        // the production configuration (TLS interception on) and the egress
        // checks below need no second VM.
        let real_secret = format!("kvm-real-{}", uuid::Uuid::new_v4().simple());
        let ctx = session_ctx_with(ToolSecrets::scoped(
            Some(Arc::new(FakeSecret(real_secret.clone()))),
            &[ToolSecretRequest::api_key(SECRET, "kvm", "kvm")],
        ));
        let capabilities = egress_capabilities();
        let bytes = test_support::external_tools_v2::bundle_with_entrypoint(
            &descriptor(),
            capabilities.clone(),
            TOOL_JS.as_bytes(),
        );
        let audited = baml_rt_tools::external_tools::audit_ktool_bundle(&bytes).expect("audit");
        let policy = ToolPolicy {
            trust_domain: TrustDomain::ThirdParty,
            capabilities: capabilities.clone(),
            secret_hosts: BTreeMap::from([(SECRET.to_string(), vec![BOUND_HOST.to_string()])]),
        };
        let mut host_policy = HostPolicy::default();
        host_policy
            .tools
            .insert((ctx.agent_id.to_string(), audited.tool_digest), policy);
        let host = Arc::new(
            ToolHost::new(pool.clone(), host_policy, Arc::new(DnsResolver)).expect("tool host"),
        );

        let descriptor = descriptor();
        let snapshot = ExternalToolSnapshot::from_v2_bundle(
            descriptor,
            capabilities,
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
        assert_eq!(
            owned_vms(&owner).await.len(),
            1,
            "the VM carries its owner label"
        );

        // 2. Host filesystem is not reachable.
        let canary =
            std::env::temp_dir().join(format!("karakara-kvm-canary-{}", std::process::id()));
        std::fs::write(&canary, b"host-only").expect("canary");
        let canary_path = canary.to_string_lossy().into_owned();
        let probe = invoke(
            &host,
            &snapshot,
            &bytes,
            &ctx,
            json!({"exists": [canary_path], "readable": ["/etc/shadow"]}),
            10_000,
        )
        .await;
        assert_eq!(probe["exists"][&canary_path], false);
        // The root backend keeps the runtime image's rootfs (only /tool and /tmp
        // are remounted), so the image's own /etc/shadow exists; the per-tool
        // unprivileged uid must not be able to read it.
        assert_eq!(probe["readable"]["/etc/shadow"], false);
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

        // 4. Egress-bound secret (§8). The worker sees only a placeholder.
        let seen = invoke(
            &host,
            &snapshot,
            &bytes,
            &ctx,
            json!({"env": [SECRET]}),
            10_000,
        )
        .await;
        let placeholder = seen["env"][SECRET]
            .as_str()
            .expect("worker env carries the declared secret's placeholder")
            .to_string();
        assert!(!placeholder.is_empty());
        assert!(
            !placeholder.contains(&real_secret),
            "the worker env holds the real secret value"
        );
        assert!(
            seen["env_values"]
                .as_array()
                .expect("env values")
                .iter()
                .all(|value| !value.as_str().unwrap_or_default().contains(&real_secret)),
            "some worker env value carries the real secret"
        );

        // 5. Guest DNS answers only granted names: the secret-bound host through
        //    the VM's forwarder, the unbound one from /etc/hosts, nothing else.
        let ungranted = "example.org";
        let lookups = invoke(
            &host,
            &snapshot,
            &bytes,
            &ctx,
            json!({"lookup": [BOUND_HOST, UNBOUND_HOST, ungranted]}),
            15_000,
        )
        .await;
        eprintln!("guest DNS lookups: {lookups}");
        for granted in [BOUND_HOST, UNBOUND_HOST] {
            assert!(
                lookups["lookup"][granted].is_array(),
                "granted host {granted} does not resolve: {lookups}"
            );
        }
        assert!(
            lookups["lookup"][ungranted].get("error").is_some(),
            "an ungranted name resolved in the guest: {lookups}"
        );

        // 6. The bound host receives the real value; an allowed but unbound host
        //    is reachable, yet a request carrying the placeholder to it is blocked.
        let probe = |url: String, secret: bool| {
            if secret {
                json!({"url": url, "header": PROBE_HEADER, "secret": SECRET})
            } else {
                json!({"url": url})
            }
        };
        let egress = invoke(
            &host,
            &snapshot,
            &bytes,
            &ctx,
            json!({"fetch": [
                probe(format!("https://{BOUND_HOST}/get"), true),
                probe(format!("https://{UNBOUND_HOST}/"), false),
                probe(format!("https://{UNBOUND_HOST}/"), true),
            ]}),
            30_000,
        )
        .await;
        eprintln!("egress probe results: {egress}");
        let results = egress["fetch"].as_array().expect("fetch results");
        let bound = &results[0];
        assert_eq!(bound["status"], 200, "bound host request failed: {bound}");
        let echoed: Value =
            serde_json::from_str(bound["body"].as_str().expect("echo body")).expect("echo json");
        // The echo carries the substituted value back into the guest. That is the
        // echo-back exposure of header substitution, accepted here because the
        // value is fake; it is how the test observes what the bound host received.
        assert_eq!(
            echoed["headers"][PROBE_HEADER], real_secret,
            "the bound host did not receive the substituted secret"
        );
        let control = &results[1];
        assert_eq!(
            control["status"], 200,
            "unbound host is not reachable without the secret: {control}"
        );
        let leak = &results[2];
        assert!(
            leak.get("error").is_some(),
            "a placeholder sent to an unbound host must be blocked: {leak}"
        );
        assert!(!leak.to_string().contains(&real_secret));

        host.shutdown().await.expect("shutdown");
        // Teardown removes the VM, not just stops it: nothing is left holding disk.
        assert!(
            owned_vms(&owner).await.is_empty(),
            "shutdown must remove the VM"
        );
    })
    .await;
}

/// A runner that dies without shutdown leaks its VM; the next start under the
/// same identity removes it, and never touches another owner's VM.
#[tokio::test]
async fn restarted_runner_sweeps_vms_its_previous_run_leaked() {
    let Ok(image) = std::env::var("KARAKARA_KVM_IMAGE") else {
        eprintln!("skipping: set KARAKARA_KVM_IMAGE to a digest-pinned node runtime image");
        return;
    };
    if !Path::new("/dev/kvm").exists() {
        eprintln!("skipping: /dev/kvm is not available");
        return;
    }
    let provider = || {
        Arc::new(
            MicrosandboxProvider::new()
                .expect("provider")
                .with_runtime_image(default_node_runtime_image(), image.clone())
                .expect("register runtime image"),
        )
    };
    let owner = format!("kvm-sweep-{}", uuid::Uuid::new_v4());
    let bystander = format!("kvm-bystander-{}", uuid::Uuid::new_v4());
    cleaning_up_vms(&[&owner, &bystander], async {
        let leaked = boot_one_vm(provider(), &owner).await;
        let other = boot_one_vm(provider(), &bystander).await;
        assert_eq!(owned_vms(&owner).await.len(), 1);
        // "Crash": drop the first runner's host without shutdown.
        drop(leaked);

        let restarted = VmPool::start(provider(), owner.clone(), PoolPolicy::default()).await;
        assert!(
            owned_vms(&owner).await.is_empty(),
            "restart must reclaim the leak"
        );
        assert_eq!(
            owned_vms(&bystander).await.len(),
            1,
            "another owner's VM is never swept"
        );
        restarted.shutdown().await.expect("shutdown");
        other.shutdown().await.expect("shutdown bystander");
        assert!(owned_vms(&bystander).await.is_empty());
    })
    .await;
}

/// Start a pool under `owner` and run one invoke so it holds one live VM.
async fn boot_one_vm(provider: Arc<MicrosandboxProvider>, owner: &str) -> Arc<ToolHost> {
    let pool = VmPool::new(provider, owner, PoolPolicy::default());
    let ctx = session_ctx();
    let bytes = bundle();
    let audited = baml_rt_tools::external_tools::audit_ktool_bundle(&bytes).expect("audit");
    let mut host_policy = HostPolicy::default();
    host_policy.tools.insert(
        (ctx.agent_id.to_string(), audited.tool_digest),
        ToolPolicy {
            trust_domain: TrustDomain::ThirdParty,
            capabilities: CapabilityRequest::default(),
            secret_hosts: BTreeMap::new(),
        },
    );
    let host =
        Arc::new(ToolHost::new(pool, host_policy, Arc::new(StaticResolver)).expect("tool host"));
    let snapshot = ExternalToolSnapshot::from_v2_bundle(
        descriptor(),
        CapabilityRequest::default(),
        audited.bundle_digest,
        audited.tool_digest,
        audited.manifest.runtime_image,
        "kvm",
        "2026-01-01T00:00:00Z",
    )
    .expect("snapshot");
    let echo = invoke(
        &host,
        &snapshot,
        &bytes,
        &ctx,
        json!({"echo": owner}),
        10_000,
    )
    .await;
    assert_eq!(echo["echo"], owner);
    host
}

/// Run a KVM test body. If it panics, kill and remove every VM labelled with
/// one of `owners` before re-raising, so a failed assertion never leaves a VM
/// (and the secret material microsandbox persists for it) behind.
async fn cleaning_up_vms(owners: &[&str], body: impl std::future::Future<Output = ()>) {
    use futures_util::FutureExt;
    if let Err(panic) = std::panic::AssertUnwindSafe(body).catch_unwind().await {
        for owner in owners {
            remove_owned_vms(owner).await;
        }
        std::panic::resume_unwind(panic);
    }
}

/// Best effort: this runs while a test is already failing, so it never panics.
async fn remove_owned_vms(owner: &str) {
    let Ok(page) = microsandbox::Sandbox::list_with(|query| {
        query.limit(100).label(
            baml_rt_tools::external_tools::sandbox::TOOL_VM_OWNER_LABEL,
            owner,
        )
    })
    .await
    else {
        eprintln!("cleanup: cannot list VMs owned by {owner}");
        return;
    };
    for vm in page.sandboxes {
        let name = vm.name().to_string();
        let _ = vm.kill().await;
        if let Err(error) = microsandbox::Sandbox::remove(&name).await {
            eprintln!("cleanup: cannot remove VM {name}: {error}");
        }
    }
}

/// Names of VMs carrying `owner` in the tool-VM owner label.
async fn owned_vms(owner: &str) -> Vec<String> {
    let page = microsandbox::Sandbox::list_with(|query| {
        query.limit(100).label(
            baml_rt_tools::external_tools::sandbox::TOOL_VM_OWNER_LABEL,
            owner,
        )
    })
    .await
    .expect("list VMs");
    page.sandboxes
        .iter()
        .map(|vm| vm.name().to_string())
        .collect()
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

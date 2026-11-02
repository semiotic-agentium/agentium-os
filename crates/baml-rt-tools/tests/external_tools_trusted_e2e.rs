// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P5.2/P5.5: the `TrustedHost` authority + execution path and both launch
//! modes.
//!
//! `authorize_requires_operator_trust` is pure (no process): it checks the
//! fail-closed operator-trust gate and the shared approval/identity prelude.
//! `trusted_invoke_echo_over_uds` is the live `Spawn` lane (gated on a built
//! supervisor and `node`): authorize → spawn an unsandboxed trusted process
//! over a UDS → load → echo invoke, all through `TrustedHost`.
//! `trusted_invoke_attaches_to_operator_daemon` is the `Attach` lane: it needs
//! no supervisor binary and no node, because a fake operator daemon speaks the
//! same `protocol_v2` frames on a socket the operator owns.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_tools::{
    ToolName, ToolSecrets, ToolSessionContext,
    external_tools::{
        BundleFile, CapabilityRequest, DESCRIPTOR_ABI, ErrorClass, ExternalToolSnapshot,
        KtoolRuntimeTier, OutputTrust, SideEffect, ToolRuntime, TrustedLaunch, TrustedRuntimeSpec,
        UnifiedToolDescriptor, audit_ktool_bundle, build_ktool_bundle_for_tier,
        host::{TrustedHost, TrustedHostConfig, TrustedPolicy},
    },
    tool_fsm::ToolSessionId,
};
use baml_sandbox_protocol::{
    TsrpcChannel,
    protocol::ErrorClass as WireErrorClass,
    protocol_v2::{CallContext, GuestMessage, HostMessage, InvokeRequest, PROTOCOL_V2_VERSION},
};
use serde_json::{Value, json};
use tokio::net::UnixListener;

const TOOL: &str = "support/trusted-echo";

const HARNESS: &str = r#"
const entry = process.env.KARAKARA_TOOL_ENTRY;
import(entry).then((m) => m.serve()).catch((e) => { console.error(String(e)); process.exit(2); });
"#;

const ECHO_TOOL: &str = r#"
let buffer = "";
const waiters = [];
function pump() {
  while (waiters.length && buffer.includes("\n")) {
    const index = buffer.indexOf("\n");
    const line = buffer.slice(0, index);
    buffer = buffer.slice(index + 1);
    if (line.trim()) waiters.shift()(JSON.parse(line));
  }
}
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => { buffer += chunk; pump(); });
const recv = () => new Promise((resolve) => { waiters.push(resolve); pump(); });
const send = (message) => process.stdout.write(JSON.stringify(message) + "\n");
const tool = {
  name: "trusted-echo", description: "trusted echo", inputSchema: { type: "object" },
  annotations: { readOnlyHint: true, karakara: {
    descriptorAbi: 1, version: "0.1.0", outputSchema: { type: "object" },
    errorClasses: ["execution"], sideEffect: "read", idempotent: true, outputTrust: "untrusted"
  }}
};
export async function serve() {
  for (;;) {
    const message = await recv();
    if (!message) return;
    if (message.method === "initialize") {
      send({ jsonrpc: "2.0", id: message.id, result: { protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "trusted-fixture", version: "0.1.0" } } });
    } else if (message.method === "notifications/initialized") {
      continue;
    } else if (message.method === "tools/list") {
      send({ jsonrpc: "2.0", id: message.id, result: { tools: [tool] } });
    } else if (message.method === "tools/call") {
      const input = message.params.arguments || {};
      send({ jsonrpc: "2.0", id: message.id, result: {
        structuredContent: { echo: input.echo },
        content: [{ type: "text", text: JSON.stringify({ echo: input.echo }) }]
      }});
    }
  }
}
"#;

fn descriptor() -> UnifiedToolDescriptor {
    UnifiedToolDescriptor {
        descriptor_abi: DESCRIPTOR_ABI,
        name: TOOL.into(),
        version: "0.1.0".into(),
        description: "trusted echo".into(),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        error_classes: vec![ErrorClass::Execution],
        side_effect: SideEffect::Read,
        idempotent: true,
        output_trust: OutputTrust::Untrusted,
    }
}

fn echo_bundle() -> Vec<u8> {
    build_ktool_bundle_for_tier(
        &descriptor(),
        KtoolRuntimeTier::Trusted,
        ECHO_TOOL.as_bytes().to_vec(),
        vec![BundleFile::new(
            "code/package.json",
            br#"{"type":"module"}"#.to_vec(),
        )],
        br#"{}"#.to_vec(),
    )
    .expect("build echo bundle")
}

fn session_ctx() -> ToolSessionContext {
    ToolSessionContext {
        session_id: ToolSessionId::random(),
        tool_name: ToolName::parse(TOOL).unwrap(),
        context_id: ContextId::new(1, 1),
        agent_id: AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4())),
        config: None,
        config_version: None,
        secrets: ToolSecrets::scoped(None, &[]),
        task_id: None,
        execution_classifier: None,
    }
}

fn snapshot(bundle: &[u8]) -> ExternalToolSnapshot {
    let audited = audit_ktool_bundle(bundle).expect("audit");
    ExternalToolSnapshot::from_v2_bundle(
        descriptor(),
        CapabilityRequest::default(),
        audited.bundle_digest,
        audited.tool_digest,
        audited.manifest.runtime_image,
        "operator",
        "2026-01-01T00:00:00Z",
    )
    .expect("snapshot")
}

/// The same snapshot, declaring that an operator daemon listens on `socket`.
fn attached_snapshot(bundle: &[u8], socket: &Path) -> ExternalToolSnapshot {
    let mut snapshot = snapshot(bundle);
    snapshot.tool.runtime = Some(ToolRuntime::Trusted(TrustedRuntimeSpec {
        launch: TrustedLaunch::Attach {
            socket_path: socket.to_path_buf(),
        },
        node: None,
    }));
    snapshot
}

fn config(
    policy: TrustedPolicy,
    supervisor_bin: PathBuf,
    env: Vec<(String, String)>,
) -> TrustedHostConfig {
    let root = std::env::temp_dir().join(format!(
        "karakara-trusted-host-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    TrustedHostConfig {
        supervisor_bin,
        root_dir: root,
        policy,
        control_timeout: Duration::from_secs(10),
        call_timeout: Duration::from_secs(30),
        process_env: env,
    }
}

#[test]
fn authorize_requires_operator_trust() {
    let bundle = echo_bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let snap = snapshot(&bundle);
    let ctx = session_ctx();

    // No trust recorded → fail closed.
    let host = TrustedHost::new(config(
        TrustedPolicy::new(),
        PathBuf::from("/nonexistent"),
        vec![],
    ))
    .unwrap();
    let err = host.authorize(&snap, &bundle, &ctx).unwrap_err();
    assert!(
        err.to_string().contains("not operator-trusted"),
        "unexpected error: {err}"
    );

    // Operator vouches for this agent + exact tool digest → authorized.
    let mut policy = TrustedPolicy::new();
    policy.trust(ctx.agent_id.to_string(), audited.tool_digest);
    let host = TrustedHost::new(config(policy, PathBuf::from("/nonexistent"), vec![])).unwrap();
    let grant = host.authorize(&snap, &bundle, &ctx).unwrap();
    assert_eq!(grant.tool_digest(), audited.tool_digest);
    assert_eq!(grant.descriptor_name(), TOOL);
    assert!(grant.node().is_none());
}

#[test]
fn authorize_rejects_sandbox_bundle() {
    let bundle = build_ktool_bundle_for_tier(
        &descriptor(),
        KtoolRuntimeTier::Sandbox,
        ECHO_TOOL.as_bytes().to_vec(),
        vec![BundleFile::new(
            "code/package.json",
            br#"{"type":"module"}"#.to_vec(),
        )],
        br#"{}"#.to_vec(),
    )
    .unwrap();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let snap = snapshot(&bundle);
    let ctx = session_ctx();
    let mut policy = TrustedPolicy::new();
    policy.trust(ctx.agent_id.to_string(), audited.tool_digest);
    let host = TrustedHost::new(config(policy, PathBuf::from("/nonexistent"), vec![])).unwrap();

    let err = host.authorize(&snap, &bundle, &ctx).unwrap_err();
    assert!(
        err.to_string().contains("trusted-tier .ktool"),
        "unexpected error: {err}"
    );
}

#[test]
fn authorize_rejects_unapproved_snapshot() {
    let bundle = echo_bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let mut snap = snapshot(&bundle);
    snap.approval.expires_at = Some("2025-01-01T00:00:00Z".into());
    let ctx = session_ctx();
    let mut policy = TrustedPolicy::new();
    policy.trust(ctx.agent_id.to_string(), audited.tool_digest);
    let host = TrustedHost::new(config(policy, PathBuf::from("/nonexistent"), vec![])).unwrap();
    let err = host.authorize(&snap, &bundle, &ctx).unwrap_err();
    assert!(
        err.to_string().contains("expired"),
        "unexpected error: {err}"
    );
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A minimal operator daemon: listens on the operator's socket and speaks the
/// same `protocol_v2` frames the guest supervisor does (hello, load, invoke).
/// It serves invokes until the host disconnects, so warm reuse is observable.
async fn serve_operator_daemon(listener: UnixListener) {
    let (stream, _) = listener.accept().await.expect("accept host connection");
    let (reader, writer) = stream.into_split();
    let mut channel = TsrpcChannel::new(reader, writer);

    let hello = channel.recv().await.expect("read hello");
    match serde_json::from_value::<HostMessage>(hello).expect("parse hello") {
        HostMessage::Hello { proto, .. } => assert_eq!(proto, PROTOCOL_V2_VERSION),
        other => panic!("expected hello, got {other:?}"),
    }
    channel
        .send(
            &serde_json::to_value(GuestMessage::HelloAck {
                proto: PROTOCOL_V2_VERSION,
                supervisor: "operator-daemon".into(),
            })
            .expect("serialize hello ack"),
        )
        .await
        .expect("send hello ack");

    while let Ok(value) = channel.recv().await {
        match serde_json::from_value::<HostMessage>(value).expect("parse host message") {
            HostMessage::LoadTool {
                tool_digest,
                bundle_len,
                ..
            } => {
                let mut received = 0u64;
                while received < bundle_len {
                    received += channel.recv_bytes().await.expect("read bundle chunk").len() as u64;
                }
                channel
                    .send(
                        &serde_json::to_value(GuestMessage::LoadAck {
                            tool_digest,
                            ok: true,
                            error: None,
                        })
                        .expect("serialize load ack"),
                    )
                    .await
                    .expect("send load ack");
            }
            HostMessage::Invoke(request) => {
                let InvokeRequest {
                    request_id, input, ..
                } = *request;
                let echo = input.get("echo").cloned().unwrap_or(Value::Null);
                channel
                    .send(
                        &serde_json::to_value(GuestMessage::Result {
                            request_id,
                            ok: true,
                            output: Some(json!({ "echo": echo, "daemon": true })),
                            error: None,
                            error_class: WireErrorClass::default(),
                            truncated: false,
                        })
                        .expect("serialize result"),
                    )
                    .await
                    .expect("send result");
            }
            HostMessage::Cancel { .. } | HostMessage::Secret { .. } | HostMessage::Hello { .. } => {
            }
        }
    }
}

fn attach_fixture() -> (tempfile::TempDir, PathBuf, UnixListener) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("operator-daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind operator daemon socket");
    (dir, socket, listener)
}

/// P5.5 `Attach`: the host connects to an operator-run daemon and never spawns.
#[tokio::test]
async fn trusted_invoke_attaches_to_operator_daemon() {
    let (dir, socket, listener) = attach_fixture();
    let daemon = tokio::spawn(serve_operator_daemon(listener));

    let bundle = echo_bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let snap = attached_snapshot(&bundle, &socket);
    let ctx = session_ctx();
    let mut policy = TrustedPolicy::new();
    policy.trust(ctx.agent_id.to_string(), audited.tool_digest);
    // The supervisor binary must stay unused on the attach path.
    let host = TrustedHost::new(config(
        policy,
        PathBuf::from("/nonexistent/supervisor"),
        vec![],
    ))
    .expect("trusted host");

    let grant = host.authorize(&snap, &bundle, &ctx).expect("authorize");
    assert_eq!(grant.attached_socket(), Some(socket.as_path()));

    let result = host
        .invoke(
            &snap,
            &bundle,
            &ctx,
            CallContext::default(),
            json!({"echo": "attached"}),
        )
        .await
        .expect("attached invoke");
    match result {
        GuestMessage::Result {
            ok: true, output, ..
        } => {
            let output = output.expect("output");
            assert_eq!(
                output.get("echo").and_then(|v| v.as_str()),
                Some("attached")
            );
            assert_eq!(output.get("daemon").and_then(|v| v.as_bool()), Some(true));
        }
        other => panic!("expected ok result, got {other:?}"),
    }

    // A second invoke reuses the same attached connection.
    let again = host
        .invoke(
            &snap,
            &bundle,
            &ctx,
            CallContext::default(),
            json!({"echo": "again"}),
        )
        .await
        .expect("second attached invoke");
    assert!(matches!(again, GuestMessage::Result { ok: true, .. }));

    // Shutting the host down must leave the operator's daemon socket alone.
    host.shutdown().await;
    assert!(socket.exists(), "host must not unlink an attached socket");

    daemon.abort();
    drop(dir);
}

/// P5.5 `Attach` fails closed when nothing is listening.
#[tokio::test]
async fn trusted_attach_fails_when_daemon_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");

    let bundle = echo_bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let snap = attached_snapshot(&bundle, &socket);
    let ctx = session_ctx();
    let mut policy = TrustedPolicy::new();
    policy.trust(ctx.agent_id.to_string(), audited.tool_digest);
    let host = TrustedHost::new(config(
        policy,
        PathBuf::from("/nonexistent/supervisor"),
        vec![],
    ))
    .expect("trusted host");

    let error = host
        .invoke(
            &snap,
            &bundle,
            &ctx,
            CallContext::default(),
            json!({"echo": "nobody-home"}),
        )
        .await
        .expect_err("a missing daemon must fail closed");
    assert!(
        error.to_string().contains("trusted daemon"),
        "unexpected error: {error}"
    );
}

/// Live lane: gate on `KARAKARA_TOOL_E2E=1`, `KARAKARA_SUPERVISOR_BIN`, and node.
#[tokio::test]
async fn trusted_invoke_echo_over_uds() {
    if std::env::var("KARAKARA_TOOL_E2E").as_deref() != Ok("1") {
        eprintln!("skipping trusted_invoke_echo_over_uds: set KARAKARA_TOOL_E2E=1");
        return;
    }
    let Some(bin) = std::env::var_os("KARAKARA_SUPERVISOR_BIN") else {
        eprintln!("skipping trusted_invoke_echo_over_uds: set KARAKARA_SUPERVISOR_BIN");
        return;
    };
    if !node_available() {
        eprintln!("skipping trusted_invoke_echo_over_uds: node unavailable");
        return;
    }

    let bundle = echo_bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let snap = snapshot(&bundle);
    let ctx = session_ctx();

    let mut policy = TrustedPolicy::new();
    policy.trust(ctx.agent_id.to_string(), audited.tool_digest);
    let host = TrustedHost::new(config(
        policy,
        PathBuf::from(bin),
        vec![("KARAKARA_WORKER_HARNESS".into(), HARNESS.into())],
    ))
    .unwrap();

    let result = host
        .invoke(
            &snap,
            &bundle,
            &ctx,
            CallContext::default(),
            json!({"echo": "trusted-host"}),
        )
        .await
        .expect("trusted invoke");

    match result {
        GuestMessage::Result {
            ok: true, output, ..
        } => {
            let output = output.expect("output");
            assert_eq!(
                output.get("echo").and_then(|v| v.as_str()),
                Some("trusted-host")
            );
        }
        other => panic!("expected ok result, got {other:?}"),
    }

    // A second invoke must reuse the same warm trusted process.
    let again = host
        .invoke(
            &snap,
            &bundle,
            &ctx,
            CallContext::default(),
            json!({"echo": "again"}),
        )
        .await
        .expect("second trusted invoke");
    assert!(matches!(again, GuestMessage::Result { ok: true, .. }));

    host.shutdown().await;
}

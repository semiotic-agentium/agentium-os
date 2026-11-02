// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! P4.13 unit lane: the real supervisor over a duplex, with a real node worker
//! under bwrap (no KVM).
//!
//! The SDK is not required: these tests override the worker harness with a
//! dependency-free `serve()` loop, so the test tool exercises the same MCP
//! stdio contract as a real worker.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use baml_ktool::{
    DESCRIPTOR_ABI, SideEffect, ToolDescriptor, audit_ktool_bundle, build_ktool_bundle,
    compute_descriptor_digest, default_node_runtime_image,
};
use baml_sandbox_protocol::{
    codec::{FrameReader, FrameWriter},
    protocol_v2::{GuestMessage, HostMessage, InvokeRequest, PROTOCOL_V2_VERSION, RevealedSecret},
};
use baml_tool_supervisor::{
    BwrapBackend, Supervisor, SupervisorLimits, WorkerBackend, supervisor::SUPERVISOR_NAME,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const HARNESS: &str = r#"
const entry = process.env.KARAKARA_TOOL_ENTRY;
import(entry).then((m) => m.serve()).catch((e) => { console.error(String(e)); process.exit(2); });
"#;

const TEST_TOOL: &str = r#"
import fs from "node:fs";

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
  name: "test/supervisor",
  description: "supervisor fixture",
  inputSchema: { type: "object" },
  annotations: {
    readOnlyHint: true,
    karakara: {
      descriptorAbi: 1,
      version: "0.1.0",
      outputSchema: { type: "object" },
      errorClasses: ["execution"],
      sideEffect: "read",
      idempotent: true,
      outputTrust: "untrusted",
    },
  },
};

async function call(message) {
  const { id, params = {} } = message;
  const input = params.arguments || {};
  if (input.sleep_ms) await new Promise((r) => setTimeout(r, input.sleep_ms));
  if (input.flood_stderr) process.stderr.write("x".repeat(input.flood_stderr));
  const output = { pid: process.pid };
  if (input.echo !== undefined) output.echo = input.echo;
  if (input.huge) output.huge = "y".repeat(input.huge);
  if (input.exit_now) process.exit(3);
  if (input.malformed) { process.stdout.write("not-json\\n"); return; }
  if (input.exists) output.exists = Object.fromEntries(input.exists.map((path) => [path, fs.existsSync(path)]));
  if (input.secret) {
    const table = JSON.parse(process.env.KARAKARA_REVEAL_FDS || "{}");
    const fd = table[input.secret];
    let secret = null;
    if (fd !== undefined) {
      const buf = Buffer.alloc(256);
      const read = fs.readSync(fd, buf, 0, buf.length, 0);
      secret = buf.subarray(0, read).toString("utf8");
      buf.fill(0);
    }
    output.secret = secret;
    output.env_leak = Object.entries(process.env).some(([name, value]) =>
      name !== "KARAKARA_REVEAL_FDS" && String(value).includes("s3cret-value"));
  }
  send({ jsonrpc: "2.0", id, result: { structuredContent: output, content: [{ type: "text", text: JSON.stringify(output) }] } });
}

export async function serve() {
  for (;;) {
    const message = await recv();
    if (!message) return;
    if (message.method === "initialize") {
      send({ jsonrpc: "2.0", id: message.id, result: {
        protocolVersion: "2025-06-18", capabilities: { tools: {} },
        serverInfo: { name: "supervisor-fixture", version: "0.1.0" },
      } });
    } else if (message.method === "tools/list") {
      send({ jsonrpc: "2.0", id: message.id, result: { tools: [tool] } });
    } else if (message.method === "tools/call") {
      call(message);
    }
  }
}
"#;

struct Harness {
    host: baml_sandbox_protocol::TsrpcChannel,
    supervisor: Arc<Supervisor>,
    supervisor_task: tokio::task::JoinHandle<Result<(), baml_tool_supervisor::SupervisorError>>,
    writer_task: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
    next_request: u64,
}

impl Harness {
    async fn start(limits: SupervisorLimits) -> Option<Self> {
        let backend = BwrapBackend::default();
        if !backend.available() || !node_available() {
            eprintln!("skipping: bwrap/node unavailable");
            return None;
        }
        let root = tempfile::tempdir().expect("tempdir");
        let (host, guest) = tokio::io::duplex(1024 * 1024);
        let (guest_reader, guest_writer) = tokio::io::split(guest);
        let (outbound, receiver) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(async move {
            let mut writer = FrameWriter::new(guest_writer);
            let mut receiver = receiver;
            while let Some(message) = receiver.recv().await {
                let value = serde_json::to_value(&message).expect("guest message");
                if writer.send(&value).await.is_err() {
                    break;
                }
            }
        });
        let backend: Arc<dyn WorkerBackend> = Arc::new(backend);
        let supervisor = Supervisor::new(
            SupervisorLimits {
                worker_harness: Some(HARNESS.to_string()),
                ..limits
            },
            root.path().to_path_buf(),
            backend,
            outbound,
        )
        .expect("supervisor");
        supervisor.clone().spawn_idle_reaper();
        let supervisor_task = tokio::spawn(supervisor.clone().run(FrameReader::new(guest_reader)));
        let (host_reader, host_writer) = tokio::io::split(host);
        Some(Self {
            host: baml_sandbox_protocol::TsrpcChannel::new(host_reader, host_writer),
            supervisor,
            supervisor_task,
            writer_task,
            _root: root,
            next_request: 1,
        })
    }

    async fn hello(&mut self) {
        self.host
            .send(
                &serde_json::to_value(HostMessage::Hello {
                    proto: PROTOCOL_V2_VERSION,
                    grant_class: "gc-test".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
        match self.recv().await {
            GuestMessage::HelloAck { proto, supervisor } => {
                assert_eq!(proto, PROTOCOL_V2_VERSION);
                assert_eq!(supervisor, SUPERVISOR_NAME);
            }
            other => panic!("unexpected hello reply: {other:?}"),
        }
    }

    async fn load(&mut self) -> (Vec<u8>, String, String) {
        let bundle = bundle();
        let audited = audit_ktool_bundle(&bundle).unwrap();
        let digest = audited.tool_digest.to_string();
        let image = audited.manifest.runtime_image.digest.to_string();
        let ack = self.load_raw(&bundle, &digest, &image).await;
        assert_eq!(
            ack,
            json!({"t": "load_ack", "tool_digest": digest, "ok": true})
        );
        (bundle, digest, image)
    }

    async fn load_raw(&mut self, bundle: &[u8], digest: &str, image: &str) -> Value {
        self.host
            .send(
                &serde_json::to_value(HostMessage::LoadTool {
                    tool_digest: digest.to_string(),
                    runtime_image_digest: image.to_string(),
                    bundle_len: bundle.len() as u64,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        for chunk in bundle.chunks(64 * 1024) {
            self.host.send_bytes(chunk).await.unwrap();
        }
        let message = self.recv().await;
        serde_json::to_value(&message).unwrap()
    }

    async fn invoke_with(
        &mut self,
        digest: &str,
        input: Value,
        deadline: Duration,
        secret: Option<(&str, &str)>,
    ) -> (GuestMessage, Vec<GuestMessage>) {
        let request_id = self.next_request;
        self.next_request += 1;
        if let Some((name, value)) = secret {
            self.host
                .send(
                    &serde_json::to_value(HostMessage::Secret {
                        request_id,
                        name: name.to_string(),
                        value: RevealedSecret(value.to_string()),
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        }
        let deadline_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + deadline.as_millis() as u64;
        self.host
            .send(
                &serde_json::to_value(HostMessage::Invoke(Box::new(InvokeRequest {
                    request_id,
                    tool: "test/supervisor".into(),
                    tool_digest: digest.to_string(),
                    call_context: Default::default(),
                    input,
                    deadline_ms,
                })))
                .unwrap(),
            )
            .await
            .unwrap();
        let mut events = Vec::new();
        loop {
            let message = self.recv().await;
            match message {
                GuestMessage::Event { .. } => events.push(message),
                GuestMessage::Result { request_id: id, .. } if id == request_id => {
                    return (message, events);
                }
                other => panic!("unexpected message while waiting for result: {other:?}"),
            }
        }
    }

    async fn recv(&mut self) -> GuestMessage {
        tokio::time::timeout(Duration::from_secs(20), self.host.recv())
            .await
            .expect("supervisor reply timed out")
            .map(|value| serde_json::from_value(value).expect("guest message"))
            .expect("channel read")
    }

    async fn cancel(&mut self, request_id: u64) {
        self.host
            .send(&serde_json::to_value(HostMessage::Cancel { request_id }).unwrap())
            .await
            .unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.writer_task.abort();
    }
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn bundle() -> Vec<u8> {
    bundle_for(&descriptor())
}

fn bundle_for(tool: &ToolDescriptor) -> Vec<u8> {
    build_ktool_bundle(
        tool,
        TEST_TOOL.as_bytes().to_vec(),
        vec![],
        br#"{}"#.to_vec(),
    )
    .expect("bundle")
}

fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        descriptor_abi: DESCRIPTOR_ABI,
        name: "test/supervisor".into(),
        version: "0.1.0".into(),
        description: "supervisor fixture".into(),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        error_classes: vec![baml_sandbox_protocol::ErrorClass::Execution],
        side_effect: SideEffect::Read,
        idempotent: true,
        output_trust: baml_ktool::OutputTrust::Untrusted,
    }
}

fn output(message: &GuestMessage) -> Option<&Value> {
    match message {
        GuestMessage::Result {
            ok: true, output, ..
        } => output.as_ref(),
        _ => None,
    }
}

#[tokio::test]
async fn handshake_load_and_invoke() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let (result, events) = harness
        .invoke_with(
            &digest,
            json!({"echo": "hello"}),
            Duration::from_secs(10),
            None,
        )
        .await;
    assert_eq!(output(&result).unwrap()["echo"], "hello");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GuestMessage::Event { kind, .. } if kind == "usage")),
        "usage event must precede the result: {events:?}"
    );
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn warm_worker_is_reused_across_calls() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let (first, _) = harness
        .invoke_with(&digest, json!({}), Duration::from_secs(10), None)
        .await;
    let pid = output(&first).unwrap()["pid"].clone();
    let (second, _) = harness
        .invoke_with(&digest, json!({}), Duration::from_secs(10), None)
        .await;
    assert_eq!(output(&second).unwrap()["pid"], pid);
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn concurrent_calls_share_one_mcp_worker() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let deadline_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 5_000;
    let started = Instant::now();
    for request_id in [41_u64, 42_u64] {
        harness
            .host
            .send(
                &serde_json::to_value(HostMessage::Invoke(Box::new(InvokeRequest {
                    request_id,
                    tool: "test/supervisor".into(),
                    tool_digest: digest.clone(),
                    call_context: Default::default(),
                    input: json!({"sleep_ms": 300, "echo": request_id}),
                    deadline_ms,
                })))
                .unwrap(),
            )
            .await
            .unwrap();
    }
    let mut results = Vec::new();
    while results.len() < 2 {
        if let GuestMessage::Result { request_id, .. } = harness.recv().await {
            results.push(request_id);
        }
    }
    assert_eq!(results.len(), 2);
    assert!(
        started.elapsed() < Duration::from_millis(550),
        "calls were serialized: {:?}",
        started.elapsed()
    );
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn deadline_kills_the_worker_and_the_channel_survives() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let started = std::time::Instant::now();
    let (result, _) = harness
        .invoke_with(
            &digest,
            json!({"sleep_ms": 30_000}),
            Duration::from_millis(400),
            None,
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "deadline must not wait for the tool"
    );
    match result {
        GuestMessage::Result {
            ok: false,
            error,
            error_class,
            ..
        } => {
            assert_eq!(error_class, baml_sandbox_protocol::ErrorClass::Transient);
            assert!(error.unwrap().contains("deadline"), "unexpected error");
        }
        other => panic!("expected a deadline failure, got {other:?}"),
    }
    // The vsock/channel survives: the next call cold-starts a new worker.
    let (next, _) = harness
        .invoke_with(
            &digest,
            json!({"echo": "alive"}),
            Duration::from_secs(10),
            None,
        )
        .await;
    assert_eq!(output(&next).unwrap()["echo"], "alive");
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn cancel_kills_the_inflight_worker() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let request_id = harness.next_request;
    harness.next_request += 1;
    let deadline_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 30_000;
    harness
        .host
        .send(
            &serde_json::to_value(HostMessage::Invoke(Box::new(InvokeRequest {
                request_id,
                tool: "test/supervisor".into(),
                tool_digest: digest.clone(),
                call_context: Default::default(),
                input: json!({"sleep_ms": 30_000}),
                deadline_ms,
            })))
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    harness.cancel(request_id).await;
    let result = loop {
        let message = harness.recv().await;
        match message {
            GuestMessage::Event { .. } => continue,
            GuestMessage::Result { request_id: id, .. } if id == request_id => break message,
            other => panic!("unexpected message: {other:?}"),
        }
    };
    match result {
        GuestMessage::Result {
            ok: false,
            error_class,
            ..
        } => assert_eq!(error_class, baml_sandbox_protocol::ErrorClass::Transient),
        other => panic!("expected cancellation, got {other:?}"),
    }
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn stderr_flood_is_capped_and_truncated() {
    let Some(mut harness) = Harness::start(SupervisorLimits {
        default_output_cap: 1024,
        ..SupervisorLimits::default()
    })
    .await
    else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let (result, _) = harness
        .invoke_with(
            &digest,
            json!({"flood_stderr": 64 * 1024}),
            Duration::from_secs(10),
            None,
        )
        .await;
    match result {
        GuestMessage::Result {
            ok: false,
            truncated,
            ..
        } => assert!(truncated, "over-cap logs must be reported truncated"),
        other => panic!("expected a truncated failure, got {other:?}"),
    }
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn malformed_worker_frame_retires_only_the_worker() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let (result, _) = harness
        .invoke_with(
            &digest,
            json!({"malformed": true}),
            Duration::from_secs(10),
            None,
        )
        .await;
    assert!(matches!(result, GuestMessage::Result { ok: false, .. }));
    let (next, _) = harness
        .invoke_with(
            &digest,
            json!({"echo": "still-here"}),
            Duration::from_secs(10),
            None,
        )
        .await;
    assert_eq!(output(&next).unwrap()["echo"], "still-here");
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn reveal_secret_reaches_the_worker_only_over_an_fd() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;
    let (result, _) = harness
        .invoke_with(
            &digest,
            json!({"secret": "TOKEN"}),
            Duration::from_secs(10),
            Some(("TOKEN", "s3cret-value")),
        )
        .await;
    let output = output(&result).expect("reveal call succeeds");
    assert_eq!(output["secret"], "s3cret-value");
    assert_eq!(
        output["env_leak"], false,
        "secret must not be in env values"
    );
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn tools_list_drift_is_rejected_at_load() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let mut tool = descriptor();
    tool.description = "different interface".into();
    let bundle = bundle_for(&tool);
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let ack = harness
        .load_raw(
            &bundle,
            &audited.tool_digest.to_string(),
            &audited.manifest.runtime_image.digest.to_string(),
        )
        .await;
    assert_eq!(ack["t"], "load_ack");
    assert_eq!(ack["ok"], false);
    assert!(ack["error"].as_str().unwrap().contains("drift"));
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn identity_mismatch_is_rejected_without_killing_the_channel() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let bundle = bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    let wrong = format!("sha256:{}", "a".repeat(64));
    let ack = harness
        .load_raw(
            &bundle,
            &wrong,
            &audited.manifest.runtime_image.digest.to_string(),
        )
        .await;
    assert_eq!(ack["t"], "load_ack");
    assert_eq!(ack["ok"], false);
    // The channel survives, and the correct identity loads.
    let (_bundle, digest, _) = harness.load().await;
    let (result, _) = harness
        .invoke_with(&digest, json!({"echo": 1}), Duration::from_secs(10), None)
        .await;
    assert_eq!(output(&result).unwrap()["echo"], 1);
    harness.supervisor.shutdown().await;
}

#[tokio::test]
async fn oversized_bundle_header_retires_the_connection() {
    let Some(mut harness) = Harness::start(SupervisorLimits {
        max_bundle_bytes: 16,
        ..SupervisorLimits::default()
    })
    .await
    else {
        return;
    };
    harness.hello().await;
    let bundle = bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    harness
        .host
        .send(
            &serde_json::to_value(HostMessage::LoadTool {
                tool_digest: audited.tool_digest.to_string(),
                runtime_image_digest: audited.manifest.runtime_image.digest.to_string(),
                bundle_len: bundle.len() as u64,
            })
            .unwrap(),
        )
        .await
        .unwrap();
    // The supervisor retires the connection instead of reading the bundle.
    assert!(
        harness.supervisor_task.is_finished() || {
            let finish =
                tokio::time::timeout(Duration::from_secs(5), &mut harness.supervisor_task).await;
            matches!(finish, Ok(Ok(Err(_))))
        },
        "expected the supervisor connection to retire"
    );
}

#[tokio::test]
async fn bundle_identity_helper_is_stable() {
    // Guards the shared digest seam independent of the worker path.
    let bundle = bundle();
    let audited = audit_ktool_bundle(&bundle).unwrap();
    assert_eq!(
        audited.manifest.tools[0].descriptor_digest,
        compute_descriptor_digest(&descriptor())
    );
    assert_eq!(audited.manifest.runtime_image, default_node_runtime_image());
    assert!(audited.tool_digest.to_string().starts_with("sha256:"));
    let _ = Path::new("/");
}

/// The probe posture from P0: host files and other tools are not reachable.
#[tokio::test]
async fn worker_sees_only_its_own_bundle() {
    let Some(mut harness) = Harness::start(SupervisorLimits::default()).await else {
        return;
    };
    harness.hello().await;
    let (_bundle, digest, _) = harness.load().await;

    let canary = std::env::temp_dir().join(format!(
        "karakara-canary-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&canary, b"host-only").unwrap();

    let (result, _) = harness
        .invoke_with(
            &digest,
            json!({"exists": [
                "/tool/code/tool.js",
                canary.to_string_lossy(),
                "/etc/shadow",
                "/proc/1/root/etc/shadow"
            ]}),
            Duration::from_secs(10),
            None,
        )
        .await;
    let exists = &output(&result).expect("probe succeeds")["exists"];
    assert_eq!(exists["/tool/code/tool.js"], true);
    assert_eq!(
        exists[canary.to_string_lossy().as_ref()],
        false,
        "host fs leak"
    );
    assert_eq!(exists["/etc/shadow"], false, "host fs leak");
    assert_eq!(exists["/proc/1/root/etc/shadow"], false, "host fs leak");
    let _ = std::fs::remove_file(&canary);
    harness.supervisor.shutdown().await;
}

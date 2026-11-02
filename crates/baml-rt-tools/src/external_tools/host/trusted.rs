// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `Trusted`-tier transport (external tools v2, P5.1).
//!
//! A `Trusted` tool runs **unsandboxed** as a crash-isolated host process, not a
//! microVM. It speaks the exact P4 wire (`protocol_v2` over `codec`), so it
//! reuses the [`SupervisorChannel`](super::channel) multiplexer unchanged — only
//! the transport differs: instead of a provider-owned vsock, the OS creates an
//! `AF_UNIX` listener and the trusted process dials it (same direction as the
//! vsock guest). There is **no** `grants.rs` CIDR/secret machinery on this path;
//! authority is the operator-trust record (P5.2 `TrustedHost`).
//!
//! P5.1 scope: the `Spawn` launch mode on the local node. The OS spawns
//! `baml-tool-supervisor --uds <path> --backend direct` and hands the accepted
//! connection to a `SupervisorChannel`. `Attach` (operator-run daemon) and
//! remote nodes are later phases.

// The transport is exercised by the gated e2e test below; the P5.2 `TrustedHost`
// is the first non-test consumer.
#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use baml_rt_core::Result;
use baml_sandbox_protocol::TsrpcChannel;
use tokio::{net::UnixListener, process::Command};

use super::invalid;

/// A spawned trusted-tool process plus the socket it dialed. Dropping it kills
/// the process (crash isolation: a dead runner never orphans a trusted process)
/// and unlinks the socket.
pub(super) struct TrustedSupervisor {
    child: tokio::process::Child,
    socket_path: PathBuf,
}

impl TrustedSupervisor {
    /// Bind a private `AF_UNIX` listener, spawn the trusted supervisor pointed at
    /// it with the unsandboxed (`direct`) backend, and return the accepted
    /// connection as a [`TsrpcChannel`] ready for [`SupervisorChannel::connect`].
    ///
    /// `extra_env` passes operator-controlled environment to the process (never
    /// the agent's). `socket_path` must live under a runner-private directory.
    pub(super) async fn spawn(
        supervisor_bin: &Path,
        root: &Path,
        socket_path: &Path,
        extra_env: &[(String, String)],
        timeout: Duration,
    ) -> Result<(Self, TsrpcChannel)> {
        // A stale socket from a crashed prior run would make bind fail.
        let _ = std::fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path)
            .map_err(|error| invalid(format!("cannot bind trusted-tool socket: {error}")))?;

        let mut command = Command::new(supervisor_bin);
        command
            .arg("--uds")
            .arg(socket_path)
            .arg("--backend")
            .arg("direct")
            .arg("--root")
            .arg(root)
            // The wire is the UDS; stdio is only for diagnostics.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .env_clear();
        for (name, value) in extra_env {
            command.env(name, value);
        }
        let child = command
            .spawn()
            .map_err(|error| invalid(format!("cannot spawn trusted supervisor: {error}")))?;

        let process = Self {
            child,
            socket_path: socket_path.to_path_buf(),
        };

        let (stream, _addr) = tokio::time::timeout(timeout, listener.accept())
            .await
            .map_err(|_| invalid("trusted supervisor did not connect before the timeout"))?
            .map_err(|error| invalid(format!("trusted supervisor connect failed: {error}")))?;
        let (reader, writer) = stream.into_split();
        Ok((process, TsrpcChannel::new(reader, writer)))
    }

    /// Reap the process (best effort) after the channel is retired.
    pub(super) async fn reap(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for TrustedSupervisor {
    fn drop(&mut self) {
        // `kill_on_drop` handles the process; make the socket cleanup explicit.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use baml_sandbox_protocol::protocol_v2::{CallContext, GuestMessage};
    use serde_json::json;

    use super::super::channel::SupervisorChannel;
    use super::*;
    use crate::external_tools::{
        BundleFile, CapabilityRequest, DESCRIPTOR_ABI, ErrorClass, OutputTrust, SideEffect,
        UnifiedToolDescriptor, audit_ktool_bundle, build_ktool_bundle,
    };
    use crate::mcp_snapshot::canonical_digest;

    // Dependency-free worker harness: exercises the same framed wire the SDK
    // `serve()` loop speaks, without needing `@karakara/tool` vendored.
    const HARNESS: &str = r#"
const entry = process.env.KARAKARA_TOOL_ENTRY;
import(entry).then((m) => m.serve()).catch((e) => { console.error(String(e)); process.exit(2); });
"#;

    const ECHO_TOOL: &str = r#"
const HEADER = 4;
let buffer = Buffer.alloc(0);
const waiters = [];
function pump() {
  while (waiters.length > 0 && buffer.length >= HEADER) {
    const length = buffer.readUInt32BE(0);
    if (buffer.length < HEADER + length) break;
    const message = JSON.parse(buffer.subarray(HEADER, HEADER + length).toString("utf8"));
    buffer = buffer.subarray(HEADER + length);
    waiters.shift()(message);
  }
}
process.stdin.on("data", (chunk) => { buffer = Buffer.concat([buffer, chunk]); pump(); });
const recv = () => new Promise((resolve) => { waiters.push(resolve); pump(); });
function send(message) {
  const body = Buffer.from(JSON.stringify(message), "utf8");
  const frame = Buffer.alloc(HEADER + body.length);
  frame.writeUInt32BE(body.length, 0);
  body.copy(frame, HEADER);
  process.stdout.write(frame);
}
export async function serve() {
  for (;;) {
    const message = await recv();
    if (!message) return;
    if (message.t !== "invoke") continue;
    send({ t: "result", request_id: message.request_id, ok: true,
           output: { echo: message.input.echo, pid: process.pid },
           error_class: "execution", truncated: false });
  }
}
"#;

    fn descriptor() -> UnifiedToolDescriptor {
        UnifiedToolDescriptor {
            descriptor_abi: DESCRIPTOR_ABI,
            name: "support/trusted-echo".into(),
            version: "0.1.0".into(),
            description: "trusted echo".into(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            error_classes: vec![ErrorClass::Execution],
            side_effect: SideEffect::Read,
            idempotent: true,
            output_trust: OutputTrust::Untrusted,
            requested: CapabilityRequest::default(),
        }
    }

    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn scratch(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("karakara-trusted-{tag}-{}-{nanos}", std::process::id()))
    }

    /// P5.1 live smoke test: the host `SupervisorChannel` drives a real spawned
    /// trusted supervisor process over a UDS through one echo invoke.
    ///
    /// Gated: set `KARAKARA_TOOL_E2E=1`, point `KARAKARA_SUPERVISOR_BIN` at a
    /// built `baml-tool-supervisor`, and have `node` on PATH.
    #[tokio::test]
    async fn trusted_echo_over_uds() {
        if std::env::var("KARAKARA_TOOL_E2E").as_deref() != Ok("1") {
            eprintln!("skipping trusted_echo_over_uds: set KARAKARA_TOOL_E2E=1");
            return;
        }
        let Some(bin) = std::env::var_os("KARAKARA_SUPERVISOR_BIN") else {
            eprintln!("skipping trusted_echo_over_uds: set KARAKARA_SUPERVISOR_BIN");
            return;
        };
        if !node_available() {
            eprintln!("skipping trusted_echo_over_uds: node unavailable");
            return;
        }

        let bundle = build_ktool_bundle(
            &descriptor(),
            ECHO_TOOL.as_bytes().to_vec(),
            vec![BundleFile::new("code/package.json", br#"{"type":"module"}"#.to_vec())],
            br#"{}"#.to_vec(),
        )
        .expect("build bundle");
        let audited = audit_ktool_bundle(&bundle).expect("audit bundle");

        let root = scratch("root");
        std::fs::create_dir_all(&root).expect("root dir");
        let socket = scratch("sock");

        let (process, channel) = TrustedSupervisor::spawn(
            Path::new(&bin),
            &root,
            &socket,
            &[("KARAKARA_WORKER_HARNESS".into(), HARNESS.into())],
            Duration::from_secs(10),
        )
        .await
        .expect("spawn trusted supervisor");

        // No grant class on the trusted path; a sentinel satisfies Hello.
        let grant_class = canonical_digest(&json!("trusted"));
        let supervisor = SupervisorChannel::connect(channel, grant_class, Duration::from_secs(10))
            .await
            .expect("handshake");

        supervisor
            .load(
                audited.tool_digest,
                audited.manifest.runtime_image.digest,
                &bundle,
            )
            .await
            .expect("load tool");

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let context = CallContext {
            deadline_ms: now_ms + 10_000,
            ..Default::default()
        };
        let result = supervisor
            .invoke(
                audited.tool_digest,
                context,
                json!({"echo": "hello-trusted"}),
                Duration::from_secs(15),
                &[],
            )
            .await
            .expect("invoke");

        match result {
            GuestMessage::Result {
                ok: true, output, ..
            } => {
                let output = output.expect("output present");
                assert_eq!(output.get("echo").and_then(|v| v.as_str()), Some("hello-trusted"));
            }
            other => panic!("expected ok result, got {other:?}"),
        }

        supervisor.close();
        process.reap().await;
        let _ = std::fs::remove_dir_all(&root);
    }
}

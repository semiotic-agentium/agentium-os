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


use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use baml_rt_core::Result;
use baml_sandbox_protocol::{
    TsrpcChannel,
    protocol_v2::{CallContext, GuestMessage},
};
use serde_json::json;
use tokio::{net::UnixListener, process::Command, sync::Mutex as AsyncMutex};

use super::{audit_approved_bundle, channel::SupervisorChannel, invalid};
use crate::{
    ToolSessionContext,
    external_tools::{ExternalToolSnapshot, ToolRuntime},
    mcp_snapshot::{Digest, canonical_digest},
};

static NEXT_TRUSTED_ID: AtomicU64 = AtomicU64::new(1);

/// Operator-trust policy: the presence of `(agent_id, tool_digest)` *is* the
/// authority for a `Trusted` tool. There are no capability fields — trust is
/// all-or-nothing (§3.2). Empty ⇒ deny all.
#[derive(Debug, Clone, Default)]
pub struct TrustedPolicy {
    trusted: HashSet<(String, Digest)>,
}

impl TrustedPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the operator vouches for this agent running this exact tool.
    pub fn trust(&mut self, agent_id: impl Into<String>, tool_digest: Digest) {
        self.trusted.insert((agent_id.into(), tool_digest));
    }

    pub fn is_trusted(&self, agent_id: &str, tool_digest: &Digest) -> bool {
        self.trusted.contains(&(agent_id.to_string(), *tool_digest))
    }
}

/// The sole authority artifact for a trusted invocation (contrast `ToolGrant`,
/// which carries CIDR/secret grants). Recorded in provenance.
#[derive(Debug, Clone)]
pub struct TrustGrant {
    agent_id: String,
    tool_digest: Digest,
    runtime_image_digest: Digest,
    node: Option<String>,
    descriptor_name: String,
}

impl TrustGrant {
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
    pub fn tool_digest(&self) -> Digest {
        self.tool_digest
    }
    pub fn node(&self) -> Option<&str> {
        self.node.as_deref()
    }
    pub fn descriptor_name(&self) -> &str {
        &self.descriptor_name
    }
}

/// Host configuration for the trusted tier. The supervisor binary and env are
/// operator-controlled; nothing here comes from the agent or the bundle.
pub struct TrustedHostConfig {
    /// Path to the `baml-tool-supervisor` the OS spawns for trusted tools.
    pub supervisor_bin: PathBuf,
    /// Runner-private directory for per-process sockets and scratch roots.
    pub root_dir: PathBuf,
    pub policy: TrustedPolicy,
    pub control_timeout: Duration,
    pub call_timeout: Duration,
    /// Operator environment passed to each trusted process (never the agent's).
    pub process_env: Vec<(String, String)>,
}

type TrustedKey = (Digest, Option<String>);

struct TrustedEntry {
    // Dropping the entry kills the process (kill_on_drop) and unlinks the
    // socket; the channel's Drop retires the connection.
    _process: TrustedSupervisor,
    channel: SupervisorChannel,
}

/// Runs `Trusted`-tier tools: operator-trusted, unsandboxed host processes over
/// the P4 wire. No `grants.rs` CIDR/secret machinery — authority is the
/// operator-trust policy, and the exact approved bytes are still pinned.
pub struct TrustedHost {
    config: TrustedHostConfig,
    processes: AsyncMutex<HashMap<TrustedKey, Arc<TrustedEntry>>>,
}

impl TrustedHost {
    pub fn new(config: TrustedHostConfig) -> Result<Self> {
        if config.control_timeout.is_zero() || config.call_timeout.is_zero() {
            return Err(invalid("trusted host timeouts must be nonzero"));
        }
        std::fs::create_dir_all(&config.root_dir)
            .map_err(|error| invalid(format!("cannot create trusted root: {error}")))?;
        Ok(Self {
            config,
            processes: AsyncMutex::new(HashMap::new()),
        })
    }

    /// Fail-closed operator-trust check. Shares the approval/expiry/audit/
    /// identity prelude with the Sandbox tier, then requires operator trust
    /// instead of resolving capability grants.
    pub fn authorize(
        &self,
        snapshot: &ExternalToolSnapshot,
        bytes: &[u8],
        ctx: &ToolSessionContext,
    ) -> Result<TrustGrant> {
        let audited = audit_approved_bundle(snapshot, bytes, ctx)?;
        let agent_id = ctx.agent_id.to_string();
        if !self.config.policy.is_trusted(&agent_id, &audited.tool_digest) {
            return Err(invalid(format!(
                "agent '{}' is not operator-trusted for tool '{}' ({})",
                agent_id, audited.descriptor.name, audited.tool_digest
            )));
        }
        Ok(TrustGrant {
            agent_id,
            tool_digest: audited.tool_digest,
            runtime_image_digest: audited.manifest.runtime_image.digest,
            node: trusted_node(snapshot),
            descriptor_name: audited.descriptor.name.clone(),
        })
    }

    /// Authorize, then run one invocation on the trusted process for this tool
    /// (spawning or reusing it), with the host deadline cap.
    pub async fn invoke(
        &self,
        snapshot: &ExternalToolSnapshot,
        bytes: &[u8],
        ctx: &ToolSessionContext,
        mut context: CallContext,
        input: serde_json::Value,
    ) -> Result<GuestMessage> {
        let grant = self.authorize(snapshot, bytes, ctx)?;
        context.agent_id = grant.agent_id.clone();
        context.context_id = ctx.context_id.to_string();
        context.task_id = ctx
            .task_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("host clock precedes Unix epoch"))?;
        let host_deadline = now
            .saturating_add(self.config.call_timeout)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        context.deadline_ms = if context.deadline_ms == 0 {
            host_deadline
        } else {
            context.deadline_ms.min(host_deadline)
        };
        let deadline = context.deadline_ms;
        let remaining = || -> Result<Duration> {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| invalid("invalid host clock"))?
                .as_millis() as u64;
            if deadline <= now {
                return Err(invalid("trusted invocation deadline elapsed"));
            }
            Ok(Duration::from_millis(deadline - now))
        };
        remaining()?;

        let entry = self.acquire(&grant).await?;
        let outcome = async {
            entry
                .channel
                .load(grant.tool_digest, grant.runtime_image_digest, bytes)
                .await?;
            // Reveal secrets are off by default for the trusted tier (§P5.6); a
            // trusted tool already has full host access.
            entry
                .channel
                .invoke(grant.tool_digest, context, input, remaining()?, &[])
                .await
        }
        .await;
        if outcome.is_err() {
            self.evict_unhealthy(&grant).await;
        }
        outcome
    }

    /// Reuse the live trusted process for this (tool, node), or spawn one.
    async fn acquire(&self, grant: &TrustGrant) -> Result<Arc<TrustedEntry>> {
        let key = (grant.tool_digest, grant.node.clone());
        let mut processes = self.processes.lock().await;
        if let Some(entry) = processes.get(&key) {
            if entry.channel.healthy() {
                return Ok(entry.clone());
            }
            processes.remove(&key);
        }
        let id = NEXT_TRUSTED_ID.fetch_add(1, Ordering::Relaxed);
        let socket = self.config.root_dir.join(format!("trusted-{id}.sock"));
        let root = self.config.root_dir.join(format!("trusted-root-{id}"));
        std::fs::create_dir_all(&root)
            .map_err(|error| invalid(format!("cannot create trusted process root: {error}")))?;
        let (process, io) = TrustedSupervisor::spawn(
            &self.config.supervisor_bin,
            &root,
            &socket,
            &self.config.process_env,
            self.config.control_timeout,
        )
        .await?;
        // The grant class gains the process no authority; it only labels the
        // handshake (the host is the authority).
        let grant_class = canonical_digest(&json!({
            "trust": grant.agent_id,
            "tool": grant.tool_digest.to_string(),
        }));
        // On handshake failure `process` drops at scope end, killing the
        // supervisor and unlinking the socket.
        let channel =
            SupervisorChannel::connect(io, grant_class, self.config.control_timeout).await?;
        let entry = Arc::new(TrustedEntry {
            _process: process,
            channel,
        });
        processes.insert(key, entry.clone());
        Ok(entry)
    }

    async fn evict_unhealthy(&self, grant: &TrustGrant) {
        let key = (grant.tool_digest, grant.node.clone());
        let mut processes = self.processes.lock().await;
        if processes
            .get(&key)
            .is_some_and(|entry| !entry.channel.healthy())
        {
            processes.remove(&key);
        }
    }

    /// Kill every trusted process. Entries drop here, killing their processes.
    pub async fn shutdown(&self) {
        self.processes.lock().await.clear();
    }
}

/// The node a trusted tool is pinned to, from its runtime spec. `None` = local.
fn trusted_node(snapshot: &ExternalToolSnapshot) -> Option<String> {
    match snapshot.tool.runtime.as_ref() {
        Some(ToolRuntime::Trusted(spec)) => spec.node.clone(),
        _ => None,
    }
}

/// A spawned trusted-tool process plus the socket it dialed. Dropping it kills
/// the process (crash isolation: a dead runner never orphans a trusted process)
/// and unlinks the socket.
pub(super) struct TrustedSupervisor {
    // Held so `kill_on_drop` terminates the process when the entry is dropped
    // (eviction/shutdown). Read only through `Drop`/`reap`, so allow dead_code.
    #[allow(dead_code)]
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

    /// Reap the process (best effort) after the channel is retired. Used by the
    /// P5.1 live test and the deterministic-shutdown path; pool eviction relies
    /// on `Drop`/`kill_on_drop` instead.
    #[allow(dead_code)]
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

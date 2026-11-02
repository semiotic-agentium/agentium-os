// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Per-tool worker isolation.
//!
//! Two production paths behind one trait:
//! - [`root::RootCloneBackend`] — the guest is root inside the microVM, so the
//!   supervisor uses raw `unshare`/`fork`/`setuid` + rlimits + seccomp and a
//!   distinct uid/gid per tool digest.
//! - [`bwrap::BwrapBackend`] — the unprivileged dev box (`karak tool test`),
//!   using bwrap's user/pid/mount/ipc/uts/cgroup namespaces.
//!
//! A [`direct::DirectBackend`] exists only for environments without bwrap; it
//! provides no isolation and must never be selected in a VM.

pub mod bwrap;
pub mod direct;
#[cfg(target_os = "linux")]
pub mod root;
#[cfg(target_os = "linux")]
pub mod seccomp;

use std::{
    io,
    os::fd::RawFd,
    path::{Path, PathBuf},
};

use tokio::process::{Child, Command};

/// Base fd number handed to a reveal-secret worker. The env table maps a
/// secret name to this number; the fd number itself is not secret material.
pub const REVEAL_FD_BASE: RawFd = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetNamespace {
    /// Keep the VM's network namespace. The host compiles the grant to egress
    /// rules *outside* the VM; the supervisor cannot widen them.
    Inherit,
    /// Loopback-only namespace, for tools that declared no `net` request.
    Isolated,
}

#[derive(Debug, Clone, Copy)]
pub struct Rlimits {
    pub as_bytes: u64,
    pub cpu_secs: u64,
    pub nofile: u64,
    pub nproc: u64,
    pub fsize_bytes: u64,
}

impl Rlimits {
    pub fn for_call(memory_bytes: u64, deadline_ms: u64) -> Self {
        Self {
            as_bytes: memory_bytes,
            // One extra second so the wall-clock deadline, not the CPU limit,
            // owns the kill decision.
            cpu_secs: deadline_ms.div_ceil(1000).max(1) + 1,
            nofile: 256,
            nproc: 64,
            fsize_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Everything the backend needs to create one worker's sandbox.
#[derive(Debug, Clone)]
pub struct WorkerSandbox {
    /// Distinct per tool digest (root backend). bwrap maps to its own user.
    pub uid: u32,
    pub gid: u32,
    pub bundle_dir: PathBuf,
    /// Mounted read-only at `/node_modules` when present.
    pub node_modules: Option<PathBuf>,
    pub tmpdir_bytes: u64,
    pub rlimits: Rlimits,
    pub net: NetNamespace,
    pub seccomp: bool,
}

/// One reveal secret to inherit as an fd.
#[derive(Debug, Clone)]
pub struct RevealFd {
    pub name: String,
    /// Source fd (kept open by the caller until after spawn).
    pub source: RawFd,
    /// Target fd number visible to the worker.
    pub target: RawFd,
}

/// The worker program plus its scrubbed environment.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub reveal: Vec<RevealFd>,
}

pub trait WorkerBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn spawn(&self, sandbox: &WorkerSandbox, command: WorkerCommand) -> io::Result<Child>;
}

/// Shared env construction. No secret value ever appears here.
pub fn worker_env(
    bundle_dir: &Path,
    _node_modules: Option<&Path>,
    reveals: &[RevealFd],
) -> Vec<(String, String)> {
    // Inside the sandbox the bundle is always mounted at /tool. The direct
    // (no-isolation) backend rewrites this to the host path.
    let _ = bundle_dir;
    let entry = format!("/tool/{}", baml_ktool::KTOOL_NODE_ENTRYPOINT);
    let mut env = vec![
        (
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin".to_string(),
        ),
        ("HOME".to_string(), "/tmp".to_string()),
        ("LANG".to_string(), "C.UTF-8".to_string()),
        ("TMPDIR".to_string(), "/tmp".to_string()),
        ("KARAKARA_TOOL_ENTRY".to_string(), entry),
    ];
    if !reveals.is_empty() {
        // Names → fd numbers only. This is a routing table, not a secret.
        let table: std::collections::BTreeMap<&str, RawFd> = reveals
            .iter()
            .map(|reveal| (reveal.name.as_str(), reveal.target))
            .collect();
        env.push((
            "KARAKARA_REVEAL_FDS".to_string(),
            serde_json::to_string(&table).expect("fd table serializes"),
        ));
    }
    env
}

/// The worker entry harness. The runtime image ships `@karakara/tool`; the
/// bundle ships only the tool code.
pub const WORKER_HARNESS: &str = r#"
import { serve, revealTransport } from "@karakara/tool";
const entry = process.env.KARAKARA_TOOL_ENTRY;
import(entry)
  .then((m) => serve(m.default, revealTransport()))
  .catch((e) => { console.error(String(e)); process.exit(2); });
"#;

pub fn command_for(
    program: &str,
    bundle_dir: &Path,
    node_modules: Option<&Path>,
    reveals: &[RevealFd],
    harness: Option<&str>,
    extra_env: Vec<(String, String)>,
) -> WorkerCommand {
    let mut env = worker_env(bundle_dir, node_modules, reveals);
    env.extend(extra_env);
    WorkerCommand {
        program: program.to_string(),
        args: vec![
            "--input-type=module".to_string(),
            "-e".to_string(),
            harness.unwrap_or(WORKER_HARNESS).trim().to_string(),
        ],
        env,
        // Node resolves the harness's own bare imports (and the tool's) by
        // walking up from cwd: /tool/code/node_modules → /node_modules.
        cwd: bundle_dir.join("code"),
        reveal: reveals.to_vec(),
    }
}

/// Convert a configured std command into a tokio child.
pub fn into_tokio(command: std::process::Command) -> Command {
    Command::from(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_carries_fd_numbers_not_secret_values() {
        let reveals = vec![RevealFd {
            name: "TOKEN".into(),
            source: 42,
            target: REVEAL_FD_BASE,
        }];
        let env = worker_env(Path::new("/run/ktool/digest"), None, &reveals);
        let table = env
            .iter()
            .find(|(k, _)| k == "KARAKARA_REVEAL_FDS")
            .map(|(_, v)| v.clone())
            .expect("fd table present");
        assert_eq!(table, r#"{"TOKEN":100}"#);
        assert!(!table.contains("s3cret"));
        assert!(env.iter().all(|(_, value)| !value.contains("s3cret")));
    }

    #[test]
    fn cpu_limit_tracks_the_wall_clock_deadline() {
        let limits = Rlimits::for_call(64 * 1024 * 1024, 1_500);
        assert_eq!(limits.cpu_secs, 3);
        assert_eq!(limits.as_bytes, 64 * 1024 * 1024);
    }
}

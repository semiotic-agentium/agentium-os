// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 restricted spawn for import-time MCP discovery.
//!
//! Threat model: a yet-to-be-approved MCP server may scribble, exfiltrate,
//! or hang. We mitigate by:
//!
//! - clearing ambient env and re-injecting only the variables the operator
//!   declared in `mcp-servers.json` (`env` plus resolved `secrets`);
//! - running the child in an ephemeral scratch dir (no inherited `cwd`);
//! - using `kill_on_drop` so any panic/error path tears the child down;
//! - enforcing a wall-clock timeout for the whole discovery exchange.
//!
//! Stronger isolation (microsandbox/seccomp) is separate from stdio discovery:
//! the existing microsandbox provider is shaped around length-prefixed RPC,
//! which does not fit raw stdio MCP. A second sandbox tier can be added
//! without changing this module's caller contract.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

use tempfile::TempDir;
use thiserror::Error;
use tokio::process::{Child, Command};

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("failed to create scratch directory: {0}")]
    Scratch(#[source] std::io::Error),
    #[error("failed to spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct SandboxedChild {
    pub child: Child,
    pub deadline: Instant,
    /// Held to keep the scratch dir alive for the child's lifetime.
    _scratch: TempDir,
    pub scratch_path: PathBuf,
}

impl SandboxedChild {
    /// `true` once wall-clock time has passed the spawn deadline.
    pub fn deadline_passed(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// Remaining time until the deadline, saturating at zero.
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

pub fn spawn(spec: SpawnSpec) -> Result<SandboxedChild, SandboxError> {
    let scratch = tempfile::Builder::new()
        .prefix("mcp-import-")
        .tempdir()
        .map_err(SandboxError::Scratch)?;
    let scratch_path = scratch
        .path()
        .canonicalize()
        .map_err(SandboxError::Scratch)?;

    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .current_dir(&scratch_path)
        .env_clear()
        .envs(&spec.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Detach into a new process group so a runaway child cannot signal the
    // runner's process group. Unix-only; Windows builds skip this.
    #[cfg(unix)]
    cmd.process_group(0);

    // Linux: ask the kernel to SIGKILL the child if the runner dies
    // unexpectedly (panic during async drop, `kill -9` from k8s, OOM).
    // `kill_on_drop` is best-effort and runs only if `Drop` executes; this
    // covers the cases where it does not.
    // SAFETY: prctl with PR_SET_PDEATHSIG is async-signal-safe and only
    // touches the calling thread's parent-death-signal flag. We do not call
    // into Rust runtime code between fork and exec.
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().map_err(|source| SandboxError::Spawn {
        command: spec.command.clone(),
        source,
    })?;

    Ok(SandboxedChild {
        child,
        deadline: Instant::now() + spec.timeout,
        _scratch: scratch,
        scratch_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_spec(command: &str, args: &[&str]) -> SpawnSpec {
        SpawnSpec {
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
            timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn spawned_child_runs_in_scratch_dir_with_clean_env() {
        let path_env = std::env::var("PATH").unwrap_or_default();
        let mut env = BTreeMap::new();
        // Need PATH to find `sh`/`pwd`/`printenv` on most systems.
        env.insert("PATH".into(), path_env);
        // Inject a tracer var to confirm we set what we want.
        env.insert("MCP_FIXTURE_MARKER".into(), "yes".into());
        let mut spec = base_spec(
            "sh",
            &[
                "-c",
                "pwd; printenv MCP_FIXTURE_MARKER; printenv HOME || true",
            ],
        );
        spec.env = env;

        let sandboxed = spawn(spec).unwrap();
        let output = sandboxed
            .child
            .wait_with_output()
            .await
            .expect("child output");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let mut lines = stdout.lines();
        let cwd = lines.next().unwrap();
        let marker = lines.next().unwrap();
        let home = lines.next().unwrap_or("");

        let cwd_path = std::path::Path::new(cwd)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(cwd));
        assert!(
            cwd_path.starts_with(&sandboxed.scratch_path),
            "cwd `{}` not under scratch `{}`",
            cwd_path.display(),
            sandboxed.scratch_path.display()
        );
        assert_eq!(marker, "yes");
        assert!(
            home.is_empty(),
            "expected HOME to be unset by env_clear, got `{home}`"
        );
    }

    #[tokio::test]
    async fn missing_command_returns_spawn_error() {
        let err = spawn(base_spec("definitely-not-a-real-binary-xyz", &[])).unwrap_err();
        assert!(matches!(err, SandboxError::Spawn { .. }));
    }
}

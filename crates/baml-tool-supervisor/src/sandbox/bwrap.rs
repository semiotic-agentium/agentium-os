// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Unprivileged dev backend: bwrap user/pid/mount/ipc/uts/cgroup namespaces.
//!
//! Same wire, same worker program, same deadline/output/cancel logic as the
//! root backend. The only difference is the privilege model: on a dev box all
//! workers share the host uid, so cross-tool isolation is namespace-based.

use std::{
    io,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::PathBuf,
    process::{Command, Stdio},
};

use tokio::process::Child;

use super::{NetNamespace, RevealFd, WorkerBackend, WorkerCommand, WorkerSandbox};

pub struct BwrapBackend {
    pub program: PathBuf,
}

impl Default for BwrapBackend {
    fn default() -> Self {
        Self {
            program: std::env::var_os("KARAKARA_BWRAP")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("bwrap")),
        }
    }
}

impl BwrapBackend {
    /// True when a bwrap binary is present. Used by `karak tool test` to give a
    /// clear configuration error instead of a spawn failure.
    pub fn available(&self) -> bool {
        Command::new(&self.program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

impl WorkerBackend for BwrapBackend {
    fn name(&self) -> &'static str {
        "bwrap"
    }

    fn spawn(&self, sandbox: &WorkerSandbox, command: WorkerCommand) -> io::Result<Child> {
        let mut args: Vec<String> = vec![
            "--die-with-parent".into(),
            "--new-session".into(),
            "--unshare-user".into(),
            "--unshare-pid".into(),
            "--unshare-ipc".into(),
            "--unshare-uts".into(),
            "--unshare-cgroup-try".into(),
            "--ro-bind".into(),
            "/usr".into(),
            "/usr".into(),
            "--ro-bind-try".into(),
            "/bin".into(),
            "/bin".into(),
            "--ro-bind-try".into(),
            "/lib".into(),
            "/lib".into(),
            "--ro-bind-try".into(),
            "/lib64".into(),
            "/lib64".into(),
            "--ro-bind-try".into(),
            "/etc/ssl".into(),
            "/etc/ssl".into(),
            "--ro-bind-try".into(),
            "/etc/ca-certificates".into(),
            "/etc/ca-certificates".into(),
            "--ro-bind".into(),
            sandbox.bundle_dir.display().to_string(),
            "/tool".into(),
            "--size".into(),
            sandbox.tmpdir_bytes.to_string(),
            "--tmpfs".into(),
            "/tmp".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--chdir".into(),
            "/tool/code".into(),
        ];
        if let Some(node_modules) = &sandbox.node_modules {
            // Test harnesses may need a dependency directory outside the
            // bundle. Published toolsets vendor dependencies in code/node_modules.
            args.push("--ro-bind".into());
            args.push(node_modules.display().to_string());
            args.push("/node_modules".into());
        }
        match sandbox.net {
            NetNamespace::Isolated => args.push("--unshare-net".into()),
            NetNamespace::Inherit => {
                // The VM's network policy (compiled from the grant by the
                // host) is the egress boundary. Give the worker DNS.
                args.push("--ro-bind-try".into());
                args.push("/etc/resolv.conf".into());
                args.push("/etc/resolv.conf".into());
            }
        }
        args.push("--".into());
        args.push(command.program.clone());
        args.extend(command.args.iter().cloned());

        let mut std_command = Command::new(&self.program);
        std_command
            .args(&args)
            .current_dir(&command.cwd)
            .env_clear()
            .envs(command.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        std_command.process_group(0);

        let limits = sandbox.rlimits;
        let reveals = command.reveal.clone();
        // SAFETY: only async-signal-safe calls (setrlimit, dup2, fcntl,
        // prctl) run between fork and exec.
        unsafe {
            std_command.pre_exec(move || {
                apply_rlimits(&limits, false)?;
                inherit_reveal_fds(&reveals)?;
                // The supervisor is the worker's parent; a dead supervisor
                // must not leave workers behind.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
                Ok(())
            });
        }
        super::into_tokio(std_command).spawn()
    }
}

pub(crate) fn apply_rlimits(limits: &super::Rlimits, include_nproc: bool) -> io::Result<()> {
    set_rlimit(libc::RLIMIT_AS, limits.as_bytes)?;
    set_rlimit(libc::RLIMIT_CPU, limits.cpu_secs)?;
    set_rlimit(libc::RLIMIT_NOFILE, limits.nofile)?;
    set_rlimit(libc::RLIMIT_FSIZE, limits.fsize_bytes)?;
    if include_nproc {
        // Genuinely per-tool only with a distinct uid (root backend).
        set_rlimit(libc::RLIMIT_NPROC, limits.nproc)?;
    }
    Ok(())
}

fn set_rlimit(resource: libc::__rlimit_resource_t, value: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `dup2` each reveal secret onto its target fd and clear `FD_CLOEXEC` there.
pub(crate) fn inherit_reveal_fds(reveals: &[RevealFd]) -> io::Result<()> {
    for reveal in reveals {
        if reveal.source == reveal.target {
            continue;
        }
        if unsafe { libc::dup2(reveal.source, reveal.target) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = unsafe { libc::fcntl(reveal.target, libc::F_GETFD) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(reveal.target, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn describe_status(status: std::process::ExitStatus) -> String {
    match status.signal() {
        Some(signal) => format!("killed by signal {signal}"),
        None => format!("exited with {status}"),
    }
}

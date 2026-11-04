// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Production backend: the supervisor is root inside the microVM.
//!
//! Uses raw `unshare`/`fork`/`setuid` rather than bwrap:
//! - fresh mount, PID, IPC, UTS and cgroup namespaces (plus a loopback-only
//!   net namespace for tools that declared no egress);
//! - a distinct uid/gid per tool digest, so `RLIMIT_NPROC` is genuinely
//!   per-tool and one tool cannot signal/read another's processes;
//! - a read-only bundle mount at `/tool`, sized private tmpfs at `/tmp`;
//! - rlimits and a pinned seccomp allowlist. Cgroup namespaces are used when
//!   the guest kernel provides them, with the VM and rlimits as the fallback.
//!
//! `unshare(CLONE_NEWPID)` only moves *future children* into the new PID
//! namespace, so the child forks once more: the spawned process is a small shim
//! that reaps the real worker and propagates its exit status. The shim and
//! worker keep the process group created by `Command`; the supervisor signals
//! that group for deadline/cancel.
//!
//! Post-fork code must stay async-signal-safe: all paths/options are built
//! before `pre_exec`, and only raw syscalls run after `fork`.

use std::{
    ffi::CString,
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

use tokio::process::Child;

use super::{NetNamespace, RevealFd, WorkerBackend, WorkerCommand, WorkerSandbox};

pub struct RootCloneBackend;

impl RootCloneBackend {
    pub fn running_as_root() -> bool {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
}

impl WorkerBackend for RootCloneBackend {
    fn name(&self) -> &'static str {
        "root"
    }

    fn spawn(&self, sandbox: &WorkerSandbox, command: WorkerCommand) -> io::Result<Child> {
        let plan = RootPlan::new(sandbox, &command)?;
        let mut std_command = Command::new(&command.program);
        // RootPlan mounts the bundle and chdirs into /tool/code in pre_exec.
        // Do not ask Command to chdir there first: that path only exists after
        // the worker's mount namespace is built.
        std_command
            .args(&command.args)
            .env_clear()
            .envs(command.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // New process group so deadline/cancel can kill the whole tree.
        std_command.process_group(0);

        // SAFETY: see the module note. `RootPlan` owns every allocation; the
        // closure only issues raw syscalls.
        unsafe {
            std_command.pre_exec(move || plan.enter());
        }
        super::into_tokio(std_command).spawn()
    }
}

/// Hand the unpacked bundle to the tool's uid with no cross-tool read access.
///
/// The tmpfs root is traversable (0711) so every worker can reach its own
/// digest directory, but each bundle tree is owned by exactly one uid and mode
/// 0700/0400. Caller must be root.
pub fn seal_bundle(dir: &Path, uid: u32, gid: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fn visit(dir: &Path, uid: u32, gid: u32) -> io::Result<()> {
        nix::unistd::chown(
            dir,
            Some(nix::unistd::Uid::from_raw(uid)),
            Some(nix::unistd::Gid::from_raw(gid)),
        )
        .map_err(|error| io::Error::other(format!("chown {}: {error}", dir.display())))?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::other(format!(
                "bundle contains a symlink: {}",
                dir.display()
            )));
        }
        if metadata.is_dir() {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            for entry in std::fs::read_dir(dir)? {
                visit(&entry?.path(), uid, gid)?;
            }
        } else {
            let executable = metadata.permissions().mode() & 0o111 != 0;
            std::fs::set_permissions(
                dir,
                std::fs::Permissions::from_mode(if executable { 0o500 } else { 0o400 }),
            )?;
        }
        Ok(())
    }

    visit(dir, uid, gid)
}

struct RootPlan {
    unshare_flags: libc::c_int,
    root: CString,
    tmpfs_source: CString,
    tmpfs_target: CString,
    tmpfs_options: CString,
    bundle: CString,
    tool: CString,
    node_modules_source: Option<CString>,
    node_modules_target: CString,
    proc_source: CString,
    proc_target: CString,
    workdir: CString,
    uid: libc::uid_t,
    gid: libc::gid_t,
    limits: super::Rlimits,
    reveals: Vec<RevealFd>,
    #[cfg(target_os = "linux")]
    seccomp: Option<super::seccomp::Profile>,
}

impl RootPlan {
    fn new(sandbox: &WorkerSandbox, command: &WorkerCommand) -> io::Result<Self> {
        let path = |value: &Path| -> io::Result<CString> {
            CString::new(value.as_os_str().as_encoded_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
        };
        let mut flags = libc::CLONE_NEWNS
            | libc::CLONE_NEWIPC
            | libc::CLONE_NEWUTS
            | libc::CLONE_NEWPID
            | libc::CLONE_NEWCGROUP;
        if sandbox.net == NetNamespace::Isolated {
            flags |= libc::CLONE_NEWNET;
        }
        #[cfg(target_os = "linux")]
        let seccomp =
            if sandbox.seccomp {
                Some(super::seccomp::program().map_err(|error| {
                    io::Error::other(format!("compile seccomp profile: {error}"))
                })?)
            } else {
                None
            };

        Ok(Self {
            unshare_flags: flags,
            root: CString::new("/").expect("static"),
            tmpfs_source: CString::new("tmpfs").expect("static"),
            tmpfs_target: CString::new("/tmp").expect("static"),
            tmpfs_options: CString::new(format!(
                "size={}k,mode=1777,nosuid,nodev,noexec",
                sandbox.tmpdir_bytes / 1024
            ))
            .expect("tmpfs options have no NUL"),
            bundle: path(&sandbox.bundle_dir)?,
            tool: CString::new("/tool").expect("static"),
            node_modules_source: sandbox
                .node_modules
                .as_ref()
                .map(|source| path(source))
                .transpose()?,
            node_modules_target: CString::new("/node_modules").expect("static"),
            proc_source: CString::new("proc").expect("static"),
            proc_target: CString::new("/proc").expect("static"),
            workdir: CString::new("/tool/code").expect("static"),
            uid: sandbox.uid,
            gid: sandbox.gid,
            limits: sandbox.rlimits,
            reveals: command.reveal.clone(),
            #[cfg(target_os = "linux")]
            seccomp,
        })
    }

    /// Runs in the forked child, before `exec`. Must not allocate.
    fn enter(&self) -> io::Result<()> {
        // SAFETY: raw syscalls with values owned by `self`.
        unsafe {
            if libc::unshare(self.unshare_flags) != 0 {
                let error = io::Error::last_os_error();
                // Some microsandbox guest kernels omit CONFIG_CGROUP_NS. The
                // per-tool VM, uid, PID namespace, and rlimits still apply;
                // retry only this optional namespace on EINVAL.
                if error.raw_os_error() == Some(libc::EINVAL)
                    && self.unshare_flags & libc::CLONE_NEWCGROUP != 0
                {
                    if libc::unshare(self.unshare_flags & !libc::CLONE_NEWCGROUP) != 0 {
                        return Err(io::Error::other(format!(
                            "unshare worker namespaces without cgroup namespace: {}",
                            io::Error::last_os_error()
                        )));
                    }
                } else {
                    return Err(io::Error::other(format!(
                        "unshare worker namespaces: {error}"
                    )));
                }
            }

            // Move the exec'd process into the new PID namespace: the first
            // child of a process that unshared CLONE_NEWPID is PID 1 there.
            let pid = libc::fork();
            if pid < 0 {
                return Err(io::Error::last_os_error());
            }
            if pid > 0 {
                // Shim: reap the worker and mirror its exit status. This keeps
                // the supervisor's Child handle pointing at a live process and
                // makes the whole tree share the signaled process group.
                //
                // The shim never execs, so it must drop every inherited fd
                // first. In particular it holds std's CLOEXEC exec-status
                // channel: `Command::spawn()` in the supervisor blocks until
                // every copy of that channel closes, so a shim keeping it open
                // would block spawn for the worker's whole lifetime (the worker
                // is never handed its MCP `initialize`). The worker child keeps
                // its own copy, so exec success and pre-exec errors are still
                // reported. Dropping 0-2 also lets the supervisor see stdout
                // EOF as soon as the worker itself exits.
                close_all_fds();
                let mut status: libc::c_int = 0;
                loop {
                    let reaped = libc::waitpid(pid, &mut status, 0);
                    if reaped == pid {
                        break;
                    }
                    if reaped < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                    {
                        libc::_exit(1);
                    }
                }
                libc::_exit(exit_code(status));
            }

            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(io::Error::other(format!(
                    "set worker parent-death signal: {}",
                    io::Error::last_os_error()
                )));
            }

            // Make the mount tree private, then build the worker's view.
            if libc::mount(
                std::ptr::null(),
                self.root.as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ) != 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EINVAL) {
                    return Err(io::Error::other(format!(
                        "make mount tree private: {error}"
                    )));
                }
            }
            if let Err(error) = mount(
                self.tmpfs_source.as_ptr(),
                self.tmpfs_target.as_ptr(),
                self.tmpfs_source.as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                self.tmpfs_options.as_ptr(),
            ) {
                // Some guest kernels reject a nested tmpfs mount even though
                // the worker mount namespace is available. The VM already
                // isolates tools, and /tmp's sticky permissions plus the
                // worker rlimits remain in force, so keep the image /tmp in
                // that case. Other mount failures are still fatal.
                if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::EPERM)) {
                    return Err(io::Error::other(format!("mount worker tmpfs: {error}")));
                }
            }
            mount(
                self.bundle.as_ptr(),
                self.tool.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            )
            .map_err(|error| io::Error::other(format!("bind bundle at /tool: {error}")))?;
            mount(
                self.bundle.as_ptr(),
                self.tool.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID,
                std::ptr::null(),
            )
            .map_err(|error| io::Error::other(format!("remount /tool read-only: {error}")))?;
            if let Some(source) = &self.node_modules_source {
                // The image owns /node_modules; bind the SDK over it for the
                // DevBackend/test path.
                libc::mkdir(self.node_modules_target.as_ptr(), 0o755);
                mount(
                    source.as_ptr(),
                    self.node_modules_target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REC,
                    std::ptr::null(),
                )?;
                mount(
                    source.as_ptr(),
                    self.node_modules_target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID,
                    std::ptr::null(),
                )?;
            }
            mount(
                self.proc_source.as_ptr(),
                self.proc_target.as_ptr(),
                self.proc_source.as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                std::ptr::null(),
            )
            .map_err(|error| io::Error::other(format!("mount /proc: {error}")))?;

            // Distinct uid/gid per tool digest, after clearing supplementary
            // groups so the worker holds nothing from the supervisor.
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(io::Error::other(format!(
                    "clear supplementary groups: {}",
                    io::Error::last_os_error()
                )));
            }
            if libc::setgid(self.gid) != 0 {
                return Err(io::Error::other(format!(
                    "set worker gid: {}",
                    io::Error::last_os_error()
                )));
            }
            if libc::setuid(self.uid) != 0 {
                return Err(io::Error::other(format!(
                    "set worker uid: {}",
                    io::Error::last_os_error()
                )));
            }
            // setgid/setuid call commit_creds(), which clears the parent-death
            // signal. Restore it after dropping privileges so killing the shim
            // also kills the PID-namespace worker.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(io::Error::other(format!(
                    "restore worker parent-death signal: {}",
                    io::Error::last_os_error()
                )));
            }

            super::bwrap::apply_rlimits(&self.limits, true)
                .map_err(|error| io::Error::other(format!("apply worker rlimits: {error}")))?;
            super::bwrap::inherit_reveal_fds(&self.reveals)
                .map_err(|error| io::Error::other(format!("inherit reveal fds: {error}")))?;

            #[cfg(target_os = "linux")]
            if let Some(program) = &self.seccomp {
                super::seccomp::apply(program)?;
            }

            if libc::chdir(self.workdir.as_ptr()) != 0 {
                return Err(io::Error::other(format!(
                    "chdir to /tool/code: {}",
                    io::Error::last_os_error()
                )));
            }
        }
        Ok(())
    }
}

/// `mount(2)` with already-nul-terminated strings.
unsafe fn mount(
    source: *const libc::c_char,
    target: *const libc::c_char,
    filesystem: *const libc::c_char,
    flags: libc::c_ulong,
    data: *const libc::c_char,
) -> io::Result<()> {
    // SAFETY: delegated to the caller.
    if unsafe { libc::mount(source, target, filesystem, flags, data.cast()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Close every fd of the calling (forked, not yet exec'd) process.
/// Async-signal-safe: raw syscalls only.
fn close_all_fds() {
    // SAFETY: close_range/close have no memory-safety preconditions.
    unsafe {
        if libc::syscall(libc::SYS_close_range, 0u32, u32::MAX, 0u32) == 0 {
            return;
        }
        // Kernels before 5.9 lack close_range.
        let max = libc::sysconf(libc::_SC_OPEN_MAX);
        let max = if max <= 0 { 1024 } else { max.min(1 << 20) } as libc::c_int;
        for fd in 0..max {
            libc::close(fd);
        }
    }
}

fn exit_code(status: libc::c_int) -> libc::c_int {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        // Signal-terminated children report 128+signal, the shell convention.
        128 + libc::WTERMSIG(status)
    }
}

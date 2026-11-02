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
//! - rlimits and a pinned seccomp allowlist.
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
        std_command
            .args(&command.args)
            .current_dir(&command.cwd)
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
    seccomp: Option<seccompiler::BpfProgram>,
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
                return Err(io::Error::last_os_error());
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
                return Err(io::Error::last_os_error());
            }

            // Make the mount tree private, then build the worker's view.
            mount(
                std::ptr::null(),
                self.root.as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )?;
            mount(
                self.tmpfs_source.as_ptr(),
                self.tmpfs_target.as_ptr(),
                self.tmpfs_source.as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                self.tmpfs_options.as_ptr(),
            )?;
            mount(
                self.bundle.as_ptr(),
                self.tool.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            )?;
            mount(
                self.bundle.as_ptr(),
                self.tool.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID,
                std::ptr::null(),
            )?;
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
            )?;

            // Distinct uid/gid per tool digest, after clearing supplementary
            // groups so the worker holds nothing from the supervisor.
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::setgid(self.gid) != 0 || libc::setuid(self.uid) != 0 {
                return Err(io::Error::last_os_error());
            }
            // setgid/setuid call commit_creds(), which clears the parent-death
            // signal. Restore it after dropping privileges so killing the shim
            // also kills the PID-namespace worker.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }

            super::bwrap::apply_rlimits(&self.limits, true)?;
            super::bwrap::inherit_reveal_fds(&self.reveals)?;

            #[cfg(target_os = "linux")]
            if let Some(program) = &self.seccomp {
                super::seccomp::apply(program)?;
            }

            if libc::chdir(self.workdir.as_ptr()) != 0 {
                return Err(io::Error::last_os_error());
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

fn exit_code(status: libc::c_int) -> libc::c_int {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        // Signal-terminated children report 128+signal, the shell convention.
        128 + libc::WTERMSIG(status)
    }
}

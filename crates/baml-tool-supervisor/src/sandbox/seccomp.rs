// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Pinned seccomp allowlist for the `node` runtime image (`supervisor_abi: 1`).
//!
//! The list is deliberately the Node 22 / libuv / glibc syscall surface rather
//! than a minimal C program's: libuv probes optional facilities, so a syscall
//! that is missing must not be a kill. Everything outside the runtime's surface
//! (module loading, `ptrace`, `bpf`, `kexec`, raw mounts, keyrings, …) is
//! default-deny and kills the worker.
//!
//! Regenerating this profile (node major bump, new native dependency) bumps the
//! runtime-image registry digest and therefore every tool digest.

#[cfg(target_os = "linux")]
use std::collections::BTreeMap;

#[cfg(target_os = "linux")]
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};

/// The pinned profile: two stacked filters, installed together by [`apply`]
/// in a forked worker child.
///
/// A seccomp filter has a single match action, so "fail softly" syscalls need
/// their own filter. The kernel evaluates every installed filter and takes the
/// most severe verdict (KILL > ERRNO > ALLOW), so a syscall answered with
/// `ENOSYS` by `soft_deny` must still be *allowed* by `allowlist`, otherwise
/// the allowlist's kill wins.
#[cfg(target_os = "linux")]
pub struct Profile {
    soft_deny: BpfProgram,
    allowlist: BpfProgram,
}

/// Compile the pinned profile.
#[cfg(target_os = "linux")]
pub fn program() -> Result<Profile, String> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = allowed_syscalls()
        .into_iter()
        .map(|number| (number, Vec::new()))
        .collect();
    rules.insert(libc::SYS_ioctl, ioctl_rules()?);
    let allowlist = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        target_arch()?,
    )
    .map_err(|error| error.to_string())?;
    let soft_deny = SeccompFilter::new(
        soft_denied_syscalls()
            .into_iter()
            .map(|number| (number, Vec::new()))
            .collect(),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        target_arch()?,
    )
    .map_err(|error| error.to_string())?;
    Ok(Profile {
        soft_deny: BpfProgram::try_from(soft_deny).map_err(|error| error.to_string())?,
        allowlist: BpfProgram::try_from(allowlist).map_err(|error| error.to_string())?,
    })
}

/// Install both filters. Async-signal-safe: no allocation, raw syscalls only.
#[cfg(target_os = "linux")]
pub fn apply(profile: &Profile) -> std::io::Result<()> {
    // The soft-deny filter goes first so installing it never depends on the
    // allowlist permitting `seccomp(2)`.
    for program in [&profile.soft_deny, &profile.allowlist] {
        seccompiler::apply_filter(program)
            .map_err(|error| std::io::Error::other(format!("install seccomp filter: {error}")))?;
    }
    Ok(())
}

/// Syscalls the runtime probes but must not get: they fail with `ENOSYS` so
/// the runtime takes its fallback path instead of being killed.
///
/// `io_uring`: submissions run in kernel context and are NOT checked against
/// seccomp, so a usable ring would bypass this whole allowlist (openat,
/// connect, …). It also hangs node in the microVM: libuv sets up its
/// epoll-ctl ring and the event loop blocks forever in `io_cqring_wait`, so the
/// worker never reads the MCP `initialize`. On `ENOSYS` libuv falls back to
/// plain epoll, the same path it takes under Docker's default profile.
#[cfg(target_os = "linux")]
fn soft_denied_syscalls() -> Vec<i64> {
    vec![
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
    ]
}

#[cfg(target_os = "linux")]
fn target_arch() -> Result<TargetArch, String> {
    #[cfg(target_arch = "x86_64")]
    {
        Ok(TargetArch::x86_64)
    }
    #[cfg(target_arch = "aarch64")]
    {
        Ok(TargetArch::aarch64)
    }
    #[cfg(target_arch = "riscv64")]
    {
        Ok(TargetArch::riscv64)
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )))]
    {
        Err("unsupported seccomp target architecture".to_string())
    }
}

/// `ioctl` is allowed only for the fd-flag and tty-query requests libuv
/// issues on stdio pipes and sockets: `isatty` (`TCGETS`), terminal size
/// probes, `uv__nonblock` (`FIONBIO`), `uv__cloexec` (`FIOCLEX`/`FIONCLEX`)
/// and readable-byte counts (`FIONREAD`). Without `ioctl` at all, node is
/// killed with SIGSYS on its first stdio probe (worker exit status 159).
/// Device and terminal-mutating requests stay default-deny.
#[cfg(target_os = "linux")]
// `c_ulong` is u64 on x86_64/aarch64 but not on every Linux target.
#[allow(clippy::useless_conversion)]
fn ioctl_rules() -> Result<Vec<SeccompRule>, String> {
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition};
    [
        libc::TCGETS,
        libc::TIOCGWINSZ,
        libc::TIOCGPGRP,
        libc::FIONBIO,
        libc::FIOCLEX,
        libc::FIONCLEX,
        libc::FIONREAD,
    ]
    .into_iter()
    .map(|request| {
        // The kernel compares the request as a 32-bit command.
        let condition = SeccompCondition::new(
            1,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            u64::from(request),
        )
        .map_err(|error| error.to_string())?;
        SeccompRule::new(vec![condition]).map_err(|error| error.to_string())
    })
    .collect()
}

#[cfg(target_os = "linux")]
fn allowed_syscalls() -> Vec<i64> {
    let mut calls: Vec<i64> = vec![
        // I/O and filesystem (bundle is read-only at /tool; /tmp is private).
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_pwrite64,
        libc::SYS_preadv,
        libc::SYS_pwritev,
        libc::SYS_preadv2,
        libc::SYS_pwritev2,
        libc::SYS_open,
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_creat,
        libc::SYS_close,
        libc::SYS_close_range,
        libc::SYS_lseek,
        libc::SYS_dup,
        libc::SYS_dup2,
        libc::SYS_dup3,
        libc::SYS_pipe,
        libc::SYS_pipe2,
        libc::SYS_fcntl,
        libc::SYS_flock,
        libc::SYS_stat,
        libc::SYS_fstat,
        libc::SYS_lstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_statfs,
        libc::SYS_fstatfs,
        libc::SYS_access,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_getdents,
        libc::SYS_getdents64,
        libc::SYS_readlink,
        libc::SYS_readlinkat,
        libc::SYS_getcwd,
        libc::SYS_chdir,
        libc::SYS_fchdir,
        libc::SYS_mkdir,
        libc::SYS_mkdirat,
        libc::SYS_rmdir,
        libc::SYS_rename,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_unlink,
        libc::SYS_unlinkat,
        libc::SYS_link,
        libc::SYS_linkat,
        libc::SYS_symlink,
        libc::SYS_symlinkat,
        libc::SYS_chmod,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_umask,
        libc::SYS_chown,
        libc::SYS_fchown,
        libc::SYS_lchown,
        libc::SYS_fchownat,
        libc::SYS_truncate,
        libc::SYS_ftruncate,
        libc::SYS_fallocate,
        libc::SYS_fsync,
        libc::SYS_fdatasync,
        libc::SYS_sync,
        libc::SYS_syncfs,
        libc::SYS_sync_file_range,
        libc::SYS_copy_file_range,
        libc::SYS_sendfile,
        libc::SYS_splice,
        libc::SYS_tee,
        libc::SYS_vmsplice,
        libc::SYS_utime,
        libc::SYS_utimes,
        libc::SYS_utimensat,
        libc::SYS_futimesat,
        libc::SYS_getxattr,
        libc::SYS_lgetxattr,
        libc::SYS_fgetxattr,
        libc::SYS_listxattr,
        libc::SYS_llistxattr,
        libc::SYS_flistxattr,
        libc::SYS_setxattr,
        libc::SYS_lsetxattr,
        libc::SYS_fsetxattr,
        libc::SYS_removexattr,
        libc::SYS_lremovexattr,
        libc::SYS_fremovexattr,
        // Memory.
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mprotect,
        libc::SYS_mremap,
        libc::SYS_msync,
        libc::SYS_madvise,
        libc::SYS_mincore,
        libc::SYS_mlock,
        libc::SYS_munlock,
        libc::SYS_mlockall,
        libc::SYS_munlockall,
        libc::SYS_mlock2,
        libc::SYS_membarrier,
        libc::SYS_remap_file_pages,
        libc::SYS_pkey_mprotect,
        libc::SYS_pkey_alloc,
        libc::SYS_pkey_free,
        // Processes and threads.
        libc::SYS_clone,
        libc::SYS_clone3,
        libc::SYS_fork,
        libc::SYS_vfork,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_wait4,
        libc::SYS_waitid,
        libc::SYS_kill,
        libc::SYS_tkill,
        libc::SYS_tgkill,
        libc::SYS_getpid,
        libc::SYS_getppid,
        libc::SYS_gettid,
        libc::SYS_getpgrp,
        libc::SYS_getpgid,
        libc::SYS_setpgid,
        libc::SYS_getsid,
        libc::SYS_setsid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_getgroups,
        libc::SYS_setgroups,
        libc::SYS_setuid,
        libc::SYS_setgid,
        libc::SYS_setreuid,
        libc::SYS_setregid,
        libc::SYS_setresuid,
        libc::SYS_setresgid,
        libc::SYS_getresuid,
        libc::SYS_getresgid,
        libc::SYS_setfsuid,
        libc::SYS_setfsgid,
        libc::SYS_set_tid_address,
        libc::SYS_set_robust_list,
        libc::SYS_get_robust_list,
        libc::SYS_restart_syscall,
        libc::SYS_futex,
        libc::SYS_futex_waitv,
        libc::SYS_sched_yield,
        libc::SYS_sched_setaffinity,
        libc::SYS_sched_getaffinity,
        libc::SYS_sched_getparam,
        libc::SYS_sched_setparam,
        libc::SYS_sched_getscheduler,
        libc::SYS_sched_setscheduler,
        libc::SYS_sched_get_priority_max,
        libc::SYS_sched_get_priority_min,
        libc::SYS_sched_rr_get_interval,
        libc::SYS_getpriority,
        libc::SYS_setpriority,
        libc::SYS_prctl,
        libc::SYS_prlimit64,
        libc::SYS_getrlimit,
        libc::SYS_setrlimit,
        libc::SYS_getrusage,
        libc::SYS_sysinfo,
        libc::SYS_times,
        libc::SYS_uname,
        libc::SYS_sysinfo,
        libc::SYS_capget,
        libc::SYS_capset,
        libc::SYS_seccomp,
        libc::SYS_personality,
        // Signals.
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigpending,
        libc::SYS_rt_sigtimedwait,
        libc::SYS_rt_sigqueueinfo,
        libc::SYS_rt_sigsuspend,
        libc::SYS_rt_tgsigqueueinfo,
        libc::SYS_sigaltstack,
        libc::SYS_restart_syscall,
        // Time.
        libc::SYS_clock_gettime,
        libc::SYS_clock_getres,
        libc::SYS_clock_nanosleep,
        libc::SYS_clock_settime,
        libc::SYS_clock_adjtime,
        libc::SYS_gettimeofday,
        libc::SYS_settimeofday,
        libc::SYS_time,
        libc::SYS_nanosleep,
        libc::SYS_alarm,
        libc::SYS_getitimer,
        libc::SYS_setitimer,
        libc::SYS_timer_create,
        libc::SYS_timer_settime,
        libc::SYS_timer_gettime,
        libc::SYS_timer_getoverrun,
        libc::SYS_timer_delete,
        libc::SYS_timerfd_create,
        libc::SYS_timerfd_settime,
        libc::SYS_timerfd_gettime,
        // Polling and events.
        libc::SYS_poll,
        libc::SYS_ppoll,
        libc::SYS_select,
        libc::SYS_pselect6,
        libc::SYS_epoll_create,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_wait,
        libc::SYS_epoll_pwait,
        libc::SYS_epoll_pwait2,
        libc::SYS_eventfd,
        libc::SYS_eventfd2,
        libc::SYS_signalfd,
        libc::SYS_signalfd4,
        libc::SYS_inotify_init,
        libc::SYS_inotify_init1,
        libc::SYS_inotify_add_watch,
        libc::SYS_inotify_rm_watch,
        libc::SYS_fanotify_init,
        libc::SYS_fanotify_mark,
        // Sockets (used only when the VM grant permits egress).
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_bind,
        libc::SYS_listen,
        libc::SYS_accept,
        libc::SYS_accept4,
        libc::SYS_connect,
        libc::SYS_getsockname,
        libc::SYS_getpeername,
        libc::SYS_getsockopt,
        libc::SYS_setsockopt,
        libc::SYS_sendto,
        libc::SYS_recvfrom,
        libc::SYS_sendmsg,
        libc::SYS_recvmsg,
        libc::SYS_sendmmsg,
        libc::SYS_recvmmsg,
        libc::SYS_shutdown,
        // SysV IPC used by some native dependencies.
        libc::SYS_semget,
        libc::SYS_semop,
        libc::SYS_semctl,
        libc::SYS_semtimedop,
        libc::SYS_shmget,
        libc::SYS_shmat,
        libc::SYS_shmctl,
        libc::SYS_shmdt,
        libc::SYS_msgget,
        libc::SYS_msgsnd,
        libc::SYS_msgrcv,
        libc::SYS_msgctl,
        // Randomness, memory fds, and anonymous files.
        libc::SYS_getrandom,
        libc::SYS_memfd_create,
        libc::SYS_io_setup,
        libc::SYS_io_destroy,
        libc::SYS_io_getevents,
        libc::SYS_io_submit,
        libc::SYS_io_cancel,
        // Allowed here only so the stacked `soft_deny` filter's ENOSYS wins
        // over a kill; see `soft_denied_syscalls`.
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        // Misc runtime probes.
        libc::SYS_readahead,
        libc::SYS_fadvise64,
        libc::SYS_getcpu,
        libc::SYS_rseq,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_prlimit64,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_send_signal,
        libc::SYS_pidfd_getfd,
        libc::SYS_getrlimit,
    ];
    #[cfg(target_arch = "x86_64")]
    {
        calls.extend([
            libc::SYS_arch_prctl,
            libc::SYS_iopl,
            libc::SYS_ioperm,
            libc::SYS_modify_ldt,
            libc::SYS_set_thread_area,
            libc::SYS_get_thread_area,
        ]);
    }
    calls.sort_unstable();
    calls.dedup();
    calls
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_profile_compiles() {
        let profile = super::program().expect("seccomp profile compiles");
        assert!(!profile.allowlist.is_empty());
        assert!(!profile.soft_deny.is_empty());
    }

    /// io_uring must fail softly with ENOSYS (libuv falls back to epoll), not
    /// kill the worker and not hand it a ring that bypasses seccomp.
    #[cfg(target_os = "linux")]
    #[test]
    fn io_uring_setup_fails_with_enosys() {
        let profile = super::program().expect("seccomp profile compiles");
        // SAFETY: the child only issues raw syscalls and `_exit`s.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                if super::apply(&profile).is_err() {
                    libc::_exit(2);
                }
                let mut params = [0u8; 120]; // struct io_uring_params
                let ret = libc::syscall(libc::SYS_io_uring_setup, 8u32, params.as_mut_ptr());
                let errno = *libc::__errno_location();
                libc::_exit(if ret == -1 && errno == libc::ENOSYS {
                    0
                } else {
                    4
                });
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "io_uring_setup was not answered with ENOSYS: status {status:#x}"
        );
    }

    /// libuv's stdio probes (`isatty`, non-blocking pipes) must survive the
    /// filter; a missing `ioctl` rule killed node with SIGSYS in the VM.
    #[cfg(target_os = "linux")]
    #[test]
    fn libuv_stdio_ioctls_are_allowed() {
        let program = super::program().expect("seccomp profile compiles");
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: the child only issues raw syscalls and `_exit`s.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                if super::apply(&program).is_err() {
                    libc::_exit(2);
                }
                let mut termios = std::mem::zeroed::<libc::termios>();
                libc::ioctl(fds[0], libc::TCGETS, &mut termios);
                let mut on: libc::c_int = 1;
                if libc::ioctl(fds[0], libc::FIONBIO, &mut on) != 0 {
                    libc::_exit(3);
                }
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child killed or failed under the filter: status {status:#x}"
        );
    }
}

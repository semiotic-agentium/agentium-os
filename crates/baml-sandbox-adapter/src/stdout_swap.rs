// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Stdout file-descriptor swap that keeps framed output pure.
//!
//! The adapter wire protocol requires stdout to carry only length-prefixed
//! JSON frames (see `tool_sandbox.md` §5.2). A single stray `println!` in
//! user code — or in a library a tool pulls in — would desync the host
//! reader permanently. [`install_stdout_swap`] relocates the process's
//! original stdout onto a fresh file descriptor (CLOEXEC-set) and points
//! fd `STDOUT_FILENO` at fd `STDERR_FILENO`. After installation:
//!
//! - Any `println!` / `print!` in user code lands on stderr (host reads
//!   that channel as free-form log output, never for frames).
//! - The returned [`StdoutHandle`] owns the dup'd original stdout and
//!   produces the async writer used for framed JSON.
//!
//! Unix only. The crate root emits a [`compile_error!`] on non-Unix
//! targets so this module is never reached off-platform. `dup3` would
//! pair dup + CLOEXEC atomically, but it's Linux/FreeBSD only; since this
//! runs at process start before any `fork` can occur, the dup + fcntl
//! pair is race-free in practice and portable across all Unix.

#![cfg(unix)]

use std::{
    io,
    os::fd::{FromRawFd, OwnedFd},
};

use tokio::fs::File as TokioFile;

/// Owns the fresh fd that points at the process's original stdout.
///
/// Dropping the handle closes the fd via [`OwnedFd`]'s own `Drop`
/// implementation — no manual `close` required, no double-close risk.
#[derive(Debug)]
pub(crate) struct StdoutHandle {
    fd: OwnedFd,
}

impl StdoutHandle {
    /// Consume the handle and produce a Tokio async writer over the
    /// original stdout.
    ///
    /// Pragmatic choice: `tokio::fs::File::from_std` works over pipes
    /// (which is what microsandbox `ExecHandle` hands us) and keeps the
    /// implementation simple. If back-pressure or readiness semantics
    /// become an issue, graduate to `AsyncFd<OwnedFd>` with a hand-rolled
    /// `AsyncWrite`.
    pub(crate) fn into_async_writer(self) -> TokioFile {
        let std_file = std::fs::File::from(self.fd);
        TokioFile::from_std(std_file)
    }
}

/// Perform the fd shuffle: dup `STDOUT_FILENO` to a fresh fd (CLOEXEC),
/// then redirect `STDOUT_FILENO` onto `STDERR_FILENO`.
///
/// Returns a [`StdoutHandle`] that owns the dup'd fd. Call exactly once
/// at process start, before spawning any threads or subprocesses that
/// could write to stdout.
///
/// Every syscall return is checked; errors capture errno through
/// [`io::Error::last_os_error`]. Partial-success cleanup closes the
/// dup'd fd directly (it isn't yet owned by a [`OwnedFd`]), so there is
/// no leak on the failure path.
pub(crate) fn install_stdout_swap() -> io::Result<StdoutHandle> {
    // SAFETY: all libc calls operate on valid, well-known file
    // descriptors (`STDOUT_FILENO`, `STDERR_FILENO`) or on a dup'd fd
    // returned by the kernel. Negative returns surface errno via
    // `io::Error::last_os_error`, and the dup'd fd is only wrapped in
    // `OwnedFd` once every subsequent syscall has succeeded.
    unsafe {
        let original = libc::dup(libc::STDOUT_FILENO);
        if original < 0 {
            return Err(io::Error::last_os_error());
        }

        let flags = libc::fcntl(original, libc::F_GETFD);
        if flags < 0 {
            let err = io::Error::last_os_error();
            libc::close(original);
            return Err(err);
        }
        if libc::fcntl(original, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            let err = io::Error::last_os_error();
            libc::close(original);
            return Err(err);
        }

        if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
            let err = io::Error::last_os_error();
            libc::close(original);
            return Err(err);
        }

        Ok(StdoutHandle {
            fd: OwnedFd::from_raw_fd(original),
        })
    }
}

// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Supervisor transports: `AF_VSOCK` in the microVM, stdio in the DevBackend.
//!
//! The guest dials the host (CID 2, `VMADDR_CID_HOST`) on the port the provider
//! passed with `--vsock`. `karak tool test` speaks the same framed wire over
//! stdin/stdout to the parent CLI.

use std::{io, time::Duration};

use baml_sandbox_protocol::{
    TsrpcChannel,
    codec::{FrameReader, FrameWriter},
};

/// Dial the host over vsock, retrying briefly while the provider finishes
/// wiring the listener.
#[cfg(target_os = "linux")]
pub async fn connect_vsock(port: u32, timeout: Duration) -> io::Result<TsrpcChannel> {
    use std::os::fd::{FromRawFd, OwnedFd};

    let socket = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if socket < 0 {
        return Err(io::Error::last_os_error());
    }
    // Own it immediately so every early return closes the fd.
    let fd = unsafe { OwnedFd::from_raw_fd(socket) };

    let address = libc::sockaddr_vm {
        svm_family: libc::AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: libc::VMADDR_CID_HOST,
        svm_zero: [0; 4],
    };
    let started = std::time::Instant::now();
    loop {
        let result = unsafe {
            libc::connect(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                (&address as *const libc::sockaddr_vm).cast(),
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if result == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        let retryable = matches!(
            error.kind(),
            io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::TimedOut
                | io::ErrorKind::NotConnected
        );
        if !retryable || started.elapsed() >= timeout {
            return Err(error);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Tokio's reactor needs non-blocking I/O.
    let flags = unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&fd), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe {
        libc::fcntl(
            std::os::fd::AsRawFd::as_raw_fd(&fd),
            libc::F_SETFL,
            flags | libc::O_NONBLOCK,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }

    #[allow(deprecated)]
    let stream = VsockStream {
        fd: tokio::io::unix::AsyncFd::new(fd)?,
    };
    let (reader, writer) = tokio::io::split(stream);
    Ok(TsrpcChannel::new(reader, writer))
}

#[cfg(not(target_os = "linux"))]
pub async fn connect_vsock(_port: u32, _timeout: Duration) -> io::Result<TsrpcChannel> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "vsock is only available on Linux",
    ))
}

/// The DevBackend channel: the parent host owns the other end of stdio.
pub fn stdio_channel() -> TsrpcChannel {
    let reader = tokio::io::stdin();
    let writer = tokio::io::stdout();
    TsrpcChannel::new(reader, writer)
}

/// Dial the host over a `AF_UNIX` stream socket (the `Trusted` tier, P5.1).
///
/// A trusted tool runs as an unsandboxed host process, not a microVM, so there
/// is no vsock. The host creates the listener and spawns this supervisor; the
/// supervisor dials it (same direction as vsock: the supervisor always dials),
/// retrying briefly while the host finishes binding.
pub async fn connect_uds(path: &std::path::Path, timeout: Duration) -> io::Result<TsrpcChannel> {
    let started = std::time::Instant::now();
    loop {
        match tokio::net::UnixStream::connect(path).await {
            Ok(stream) => {
                let (reader, writer) = stream.into_split();
                return Ok(TsrpcChannel::new(reader, writer));
            }
            Err(error) => {
                let retryable = matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionReset
                );
                if !retryable || started.elapsed() >= timeout {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
}

/// Split a channel for the supervisor's single reader + shared writer.
pub fn split(channel: TsrpcChannel) -> (FrameReader, FrameWriter) {
    channel.into_split()
}

#[cfg(target_os = "linux")]
struct VsockStream {
    fd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}

#[cfg(target_os = "linux")]
impl tokio::io::AsyncRead for VsockStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        use std::os::fd::AsRawFd as _;

        loop {
            let _guard = match self.fd.poll_read_ready(cx) {
                std::task::Poll::Ready(Ok(guard)) => guard,
                std::task::Poll::Ready(Err(error)) => return std::task::Poll::Ready(Err(error)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            let fd = self.fd.as_raw_fd();
            let unfilled = buf.initialize_unfilled();
            let read = unsafe { libc::read(fd, unfilled.as_mut_ptr().cast(), unfilled.len()) };
            if read >= 0 {
                buf.advance(read as usize);
                return std::task::Poll::Ready(Ok(()));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                continue;
            }
            return std::task::Poll::Ready(Err(error));
        }
    }
}

#[cfg(target_os = "linux")]
impl tokio::io::AsyncWrite for VsockStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        use std::os::fd::AsRawFd as _;

        loop {
            let _guard = match self.fd.poll_write_ready(cx) {
                std::task::Poll::Ready(Ok(guard)) => guard,
                std::task::Poll::Ready(Err(error)) => return std::task::Poll::Ready(Err(error)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            let fd = self.fd.as_raw_fd();
            let written = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
            if written >= 0 {
                return std::task::Poll::Ready(Ok(written as usize));
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                continue;
            }
            return std::task::Poll::Ready(Err(error));
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        // vsock has no user-visible write buffer to flush.
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        use std::os::fd::AsRawFd as _;
        let fd = self.fd.as_raw_fd();
        unsafe { libc::shutdown(fd, libc::SHUT_WR) };
        std::task::Poll::Ready(Ok(()))
    }
}

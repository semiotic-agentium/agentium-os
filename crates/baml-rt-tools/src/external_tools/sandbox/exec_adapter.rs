//! Bridge between `microsandbox::ExecHandle` and [`TsrpcChannel`].
//!
//! `ExecHandle` exposes an event-based API (`recv()` → `ExecEvent`) and a
//! separate stdin sink (`take_stdin()` → `ExecSink`). [`TsrpcChannel`] needs
//! plain [`AsyncRead`]/[`AsyncWrite`] halves. This module spans the gap with
//! two background tasks forwarding bytes through a single `tokio::io::duplex`
//! pair:
//!
//! ```text
//!           ┌──────────────────────────────┐
//! Host R ◀─ │ guest stdout (ExecEvent::    │ ◀── ExecHandle.recv()
//!  (TSRPC)  │   Stdout / Stderr / Exited)  │
//!           ├──────────────────────────────┤
//! Host W ─▶ │ guest stdin (ExecSink)       │ ──▶ ExecSink.write()
//!  (TSRPC)  └──────────────────────────────┘
//! ```
//!
//! Stderr is routed to a `tracing::debug!` so the control channel stays
//! unpolluted (§5.3 "logs never multiplexed with control").
//!
//! Only compiled under the `sandbox-provider` feature — keeps the
//! microsandbox crate out of default builds.

#![cfg(feature = "sandbox-provider")]

use microsandbox::{ExecEvent, ExecHandle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use super::channel::TsrpcChannel;

/// Buffer size for each direction of the in-process duplex between the
/// TSRPC channel and the exec adapter tasks. Sized to comfortably hold a
/// handful of typical tool invoke payloads without blocking.
const BRIDGE_BUF_BYTES: usize = 64 * 1024;

/// Adapt a live `microsandbox::ExecHandle` into a [`TsrpcChannel`].
///
/// Spawns two background tasks:
/// - **recv pump**: drains `handle.recv()`, writing `Stdout` bytes to the
///   channel's reader side; `Stderr` is logged; `Exited` ends the pump.
/// - **send pump**: reads from the channel's writer side, forwards bytes
///   into the guest's stdin via `ExecSink::write`; channel close ⇒ stdin
///   `close()` called.
///
/// Contract: the caller must have started the exec with `stdin_pipe()`
/// enabled on the [`microsandbox::ExecOptions`]; otherwise `take_stdin`
/// returns `None` and this function errors.
pub fn exec_handle_into_channel(mut handle: ExecHandle) -> Result<TsrpcChannel, &'static str> {
    let stdin = handle
        .take_stdin()
        .ok_or("ExecHandle missing stdin sink; the exec must be started with stdin_pipe()")?;

    // Two halves of a single duplex — the TsrpcChannel reads from `host_r`
    // and writes into `host_w`; the two background pumps own the other
    // halves to feed / drain the VM.
    let (host_side, vm_side) = tokio::io::duplex(BRIDGE_BUF_BYTES);
    let (host_r, host_w) = tokio::io::split(host_side);
    let (mut vm_r, mut vm_w) = tokio::io::split(vm_side);

    // recv pump — VM → host
    tokio::spawn(async move {
        while let Some(event) = handle.recv().await {
            match event {
                ExecEvent::Stdout(bytes) => {
                    if let Err(err) = vm_w.write_all(&bytes).await {
                        debug!(
                            ?err,
                            "sandbox exec adapter stdout write failed; stopping pump"
                        );
                        break;
                    }
                }
                ExecEvent::Stderr(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    info!(stderr = %text.trim_end(), "sandbox tool-adapter stderr");
                }
                ExecEvent::Exited { code } => {
                    info!(
                        exit_code = code,
                        "sandbox tool-adapter exited; closing channel"
                    );
                    break;
                }
                ExecEvent::Started { pid } => {
                    info!(pid, "sandbox tool-adapter started");
                }
            }
        }
        // `vm_w` dropped here → `host_r` sees EOF, TsrpcChannel::recv returns
        // an error, SandboxInvoker surfaces it per §13 taxonomy.
    });

    // send pump — host → VM
    tokio::spawn(async move {
        let mut buf = vec![0u8; BRIDGE_BUF_BYTES];
        loop {
            match vm_r.read(&mut buf).await {
                Ok(0) => break, // host side closed
                Ok(n) => {
                    if let Err(err) = stdin.write(&buf[..n]).await {
                        warn!(
                            ?err,
                            "sandbox exec adapter stdin write failed; stopping pump"
                        );
                        break;
                    }
                }
                Err(err) => {
                    debug!(?err, "sandbox exec adapter host-read failed; stopping pump");
                    break;
                }
            }
        }
        if let Err(err) = stdin.close().await {
            debug!(?err, "sandbox exec adapter stdin close failed");
        }
    });

    Ok(TsrpcChannel::new(host_r, host_w))
}

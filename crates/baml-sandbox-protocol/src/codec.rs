//! Length-prefixed JSON frame codec for the sandbox workload transport (§5.2).
//!
//! Each frame: `[u32 big-endian length][JSON payload of exactly that many
//! bytes]`. No newline delimiter. Binary-safe — chosen over NDJSON because
//! `exec_stream` chunks may arrive in arbitrary sizes and a buggy adapter
//! could pollute stdout (`tool_sandbox.md` §5.2).
//!
//! The codec is transport-agnostic: [`TsrpcChannel`] wraps any `AsyncRead +
//! AsyncWrite`, so the same type works for real microsandbox exec handles and
//! for in-memory duplex streams used in tests.
//!
//! Errors are reported via [`CodecError`]. This crate deliberately avoids
//! depending on host-side error types (e.g. `baml-rt-core`'s `BamlRtError`)
//! so it can be used inside distroless guest images without pulling the rest
//! of the runtime. Host code converts [`CodecError`] into its own error
//! taxonomy at the call site.

use std::pin::Pin;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound on a single TSRPC frame payload.
///
/// Not the same as `max_payload_bytes` in `tool/describe` (which the tool
/// negotiates per call); this is a host-side DoS ceiling applied at the codec
/// layer so a malicious/broken adapter can't force the runtime to allocate
/// unbounded buffers.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// Errors raised by [`TsrpcChannel`] send/recv operations.
///
/// Host-side callers typically map these onto a classified tool error (for
/// example, EOF and other I/O failures become `RpcChannelUnavailable` in the
/// §13 failure taxonomy). Guest-side callers can surface them directly.
#[derive(Debug, Error)]
pub enum CodecError {
    /// Failed to serialize an outgoing frame payload to JSON.
    #[error("failed to serialize TSRPC frame: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
    /// Failed to deserialize an incoming frame payload as JSON.
    #[error("failed to parse TSRPC frame payload as JSON: {source}")]
    Deserialize {
        #[source]
        source: serde_json::Error,
    },
    /// I/O failure while reading or writing a frame (includes EOF on read).
    #[error("failed to {op} TSRPC frame: {source}")]
    Io {
        op: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// A frame exceeded [`MAX_FRAME_BYTES`] (either outgoing or incoming).
    #[error("TSRPC frame {len} bytes exceeds MAX_FRAME_BYTES {max}")]
    FrameTooLarge { len: usize, max: usize },
}

/// Async read/write channel used for framed JSON messages.
///
/// Owns a boxed, pinned pair of `AsyncRead + AsyncWrite` halves so real
/// providers (microsandbox `ExecHandle`) and fakes (in-memory duplex) both
/// fit behind the same type.
pub struct TsrpcChannel {
    reader: Pin<Box<dyn AsyncRead + Send + Unpin>>,
    writer: Pin<Box<dyn AsyncWrite + Send + Unpin>>,
}

impl TsrpcChannel {
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self {
            reader: Box::pin(reader),
            writer: Box::pin(writer),
        }
    }

    /// Send one JSON value as a length-prefixed frame.
    pub async fn send(&mut self, value: &Value) -> Result<(), CodecError> {
        let bytes = serde_json::to_vec(value).map_err(|source| CodecError::Serialize { source })?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(CodecError::FrameTooLarge {
                len: bytes.len(),
                max: MAX_FRAME_BYTES,
            });
        }
        let len = (bytes.len() as u32).to_be_bytes();
        self.writer
            .write_all(&len)
            .await
            .map_err(|source| CodecError::Io {
                op: "write length",
                source,
            })?;
        self.writer
            .write_all(&bytes)
            .await
            .map_err(|source| CodecError::Io {
                op: "write body",
                source,
            })?;
        self.writer
            .flush()
            .await
            .map_err(|source| CodecError::Io {
                op: "flush",
                source,
            })?;
        Ok(())
    }

    /// Explicitly flush and close the write half.
    ///
    /// Relying on `Drop` is not sufficient to guarantee a final frame
    /// reaches the peer — `AsyncWrite::poll_shutdown` is the only
    /// contracted flush point. Callers that want to finish emitting
    /// before process exit must `shutdown().await` the channel.
    pub async fn shutdown(&mut self) -> Result<(), CodecError> {
        self.writer
            .shutdown()
            .await
            .map_err(|source| CodecError::Io {
                op: "shutdown",
                source,
            })
    }

    /// Receive one JSON value from a length-prefixed frame.
    ///
    /// Returns `Err(CodecError::Io)` on EOF or other I/O failure (the host
    /// maps that to `RpcChannelUnavailable` in the §13 failure taxonomy).
    pub async fn recv(&mut self) -> Result<Value, CodecError> {
        let mut len_buf = [0u8; 4];
        self.reader
            .read_exact(&mut len_buf)
            .await
            .map_err(|source| CodecError::Io {
                op: "read length",
                source,
            })?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(CodecError::FrameTooLarge {
                len,
                max: MAX_FRAME_BYTES,
            });
        }
        let mut body = vec![0u8; len];
        self.reader
            .read_exact(&mut body)
            .await
            .map_err(|source| CodecError::Io {
                op: "read body",
                source,
            })?;
        serde_json::from_slice(&body).map_err(|source| CodecError::Deserialize { source })
    }
}

impl std::fmt::Debug for TsrpcChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsrpcChannel").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::duplex;

    use super::*;

    #[tokio::test]
    async fn roundtrip_single_frame() {
        let (a, b) = duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let mut client = TsrpcChannel::new(ar, aw);
        let mut server = TsrpcChannel::new(br, bw);

        let send = tokio::spawn(async move {
            client
                .send(&json!({"method": "tool/describe", "id": 1}))
                .await
                .unwrap();
        });
        let got = server.recv().await.unwrap();
        send.await.unwrap();
        assert_eq!(got["method"], "tool/describe");
        assert_eq!(got["id"], 1);
    }

    #[tokio::test]
    async fn rejects_oversized_outgoing_frame() {
        let (a, _b) = duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let mut chan = TsrpcChannel::new(ar, aw);
        let huge = Value::String("x".repeat(MAX_FRAME_BYTES + 1));
        let err = chan.send(&huge).await.unwrap_err();
        assert!(matches!(err, CodecError::FrameTooLarge { .. }));
    }

    #[tokio::test]
    async fn recv_surfaces_eof_as_io_error() {
        let (a, b) = duplex(4096);
        drop(b); // close the writer end → reader sees EOF
        let (ar, aw) = tokio::io::split(a);
        let mut chan = TsrpcChannel::new(ar, aw);
        let err = chan.recv().await.unwrap_err();
        assert!(matches!(err, CodecError::Io { op: "read length", .. }));
    }

    #[tokio::test]
    async fn shutdown_surfaces_final_frame_before_close() {
        let (a, b) = duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let mut client = TsrpcChannel::new(ar, aw);
        let mut server = TsrpcChannel::new(br, bw);

        let sender = tokio::spawn(async move {
            client.send(&json!({"final": true})).await.unwrap();
            client.shutdown().await.unwrap();
        });
        let got = server.recv().await.unwrap();
        sender.await.unwrap();
        assert_eq!(got["final"], true);
        // Next recv must see EOF (shutdown closed the writer cleanly).
        let err = server.recv().await.unwrap_err();
        assert!(matches!(err, CodecError::Io { op: "read length", .. }));
    }

    #[tokio::test]
    async fn rejects_oversized_incoming_frame() {
        let (a, b) = duplex(8);
        let (ar, _aw) = tokio::io::split(a);
        let (_br, mut bw) = tokio::io::split(b);
        let mut chan = TsrpcChannel::new(ar, tokio::io::sink());

        // Write a bogus length header larger than MAX_FRAME_BYTES.
        let bogus_len = (MAX_FRAME_BYTES as u64 + 1) as u32;
        let writer = tokio::spawn(async move {
            bw.write_all(&bogus_len.to_be_bytes()).await.unwrap();
        });
        let err = chan.recv().await.unwrap_err();
        writer.await.unwrap();
        assert!(matches!(err, CodecError::FrameTooLarge { .. }));
    }
}

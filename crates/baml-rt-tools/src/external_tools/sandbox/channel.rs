//! Length-prefixed JSON frame codec for the sandbox workload transport (§5.2).
//!
//! Each frame: `[u32 big-endian length][JSON payload of exactly that many
//! bytes]`. No newline delimiter. Binary-safe — chosen over NDJSON because
//! `exec_stream` chunks may arrive in arbitrary sizes and a buggy adapter
//! could pollute stdout (`tool_sandbox.md` §5.2).
//!
//! The codec is transport-agnostic: [`TsrpcChannel`] wraps any `AsyncRead +
//! AsyncWrite`, so the same type works for real microsandbox exec handles
//! and for the in-memory mock provider.

use std::pin::Pin;

use baml_rt_core::{BamlRtError, Result};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound on a single TSRPC frame payload.
///
/// Not the same as `max_payload_bytes` in `tool/describe` (which the tool
/// negotiates per call); this is a host-side DoS ceiling applied at the
/// codec layer so a malicious/broken adapter can't force the runtime to
/// allocate unbounded buffers.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

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
    pub async fn send(&mut self, value: &Value) -> Result<()> {
        let bytes =
            serde_json::to_vec(value).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to serialize TSRPC frame".to_string(),
                source: Box::new(e),
            })?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(BamlRtError::InvalidArgument(format!(
                "TSRPC frame {} bytes exceeds MAX_FRAME_BYTES {}",
                bytes.len(),
                MAX_FRAME_BYTES
            )));
        }
        let len = (bytes.len() as u32).to_be_bytes();
        self.writer
            .write_all(&len)
            .await
            .map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to write TSRPC frame length".to_string(),
                source: Box::new(e),
            })?;
        self.writer.write_all(&bytes).await.map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "failed to write TSRPC frame body".to_string(),
                source: Box::new(e),
            }
        })?;
        self.writer
            .flush()
            .await
            .map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to flush TSRPC frame".to_string(),
                source: Box::new(e),
            })?;
        Ok(())
    }

    /// Receive one JSON value from a length-prefixed frame.
    ///
    /// Returns `Err` on EOF (channel closed) — callers treat that as
    /// `RpcChannelUnavailable` in the §13 failure taxonomy.
    pub async fn recv(&mut self) -> Result<Value> {
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf).await.map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "failed to read TSRPC frame length".to_string(),
                source: Box::new(e),
            }
        })?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(BamlRtError::InvalidArgument(format!(
                "incoming TSRPC frame {} bytes exceeds MAX_FRAME_BYTES {}",
                len, MAX_FRAME_BYTES
            )));
        }
        let mut body = vec![0u8; len];
        self.reader.read_exact(&mut body).await.map_err(|e| {
            BamlRtError::InvalidArgumentWithSource {
                message: "failed to read TSRPC frame body".to_string(),
                source: Box::new(e),
            }
        })?;
        serde_json::from_slice(&body).map_err(|e| BamlRtError::InvalidArgumentWithSource {
            message: "failed to parse TSRPC frame payload as JSON".to_string(),
            source: Box::new(e),
        })
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
        assert!(format!("{err}").contains("exceeds MAX_FRAME_BYTES"));
    }
}

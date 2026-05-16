//! Shared newline-delimited JSON framing used by the stdio MCP transport.
//!
//! Both the import-time client (`client.rs`) and the test fake server
//! (`fixture.rs`) write JSON-RPC envelopes one per line; centralising the
//! framing prevents the two sides from drifting on whitespace or flush
//! behaviour.

use serde::Serialize;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Serialises `value` as JSON, appends `\n`, writes, and flushes.
/// Serialisation errors surface as `io::ErrorKind::InvalidData`.
pub async fn write_json_line<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let mut payload = serde_json::to_vec(value)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    payload.push(b'\n');
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

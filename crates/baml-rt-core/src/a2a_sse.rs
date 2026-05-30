// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! SSE framing for HTTP POST `/agents/.../a2a`: each event carries one JSON-RPC 2.0 object in `data:` lines.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum A2aSseParseError {
    #[error("SSE stream contained non-whitespace text but no data: lines")]
    MissingDataLines,
    #[error("SSE stream event is not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("invalid JSON in SSE data payload: {0}")]
    Json(#[from] serde_json::Error),
}

/// Incremental SSE decoder for A2A JSON-RPC chunks.
///
/// Accepts raw byte slices from an HTTP body stream and yields fully parsed JSON-RPC payloads
/// whenever one or more SSE events are complete. Call [`finish`](Self::finish) at EOF to flush a
/// final unterminated event, matching the full-body parser's permissive semantics.
#[derive(Debug, Default)]
pub struct A2aSseDecoder {
    pending: Vec<u8>,
}

impl A2aSseDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one body chunk and return every fully decoded JSON-RPC event now available.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Value>, A2aSseParseError> {
        self.pending.extend_from_slice(bytes);
        self.drain_complete_events(false)
    }

    /// Finish the stream and flush the trailing event, if any.
    pub fn finish(mut self) -> Result<Vec<Value>, A2aSseParseError> {
        self.drain_complete_events(true)
    }

    fn drain_complete_events(&mut self, flush_tail: bool) -> Result<Vec<Value>, A2aSseParseError> {
        let mut out = Vec::new();
        while let Some((event_end, delimiter_len)) = find_event_delimiter(&self.pending) {
            let event_bytes = self.pending.drain(..event_end).collect::<Vec<_>>();
            self.pending.drain(..delimiter_len);
            if let Some(value) = parse_sse_event_bytes(&event_bytes)? {
                out.push(value);
            }
        }

        if flush_tail && !self.pending.is_empty() {
            let event_bytes = std::mem::take(&mut self.pending);
            if let Some(value) = parse_sse_event_bytes(&event_bytes)? {
                out.push(value);
            }
        }

        Ok(out)
    }
}

/// Extract JSON-RPC values from an SSE (`text/event-stream`) response body.
///
/// Implements the minimal subset used by the runner: events separated by a blank line,
/// each event containing one or more `data:` lines (joined with `\n` per SSE rules).
pub fn parse_a2a_sse_json_rpc_chunks(body: &str) -> Result<Vec<Value>, A2aSseParseError> {
    let mut decoder = A2aSseDecoder::new();
    let mut out = decoder.feed(body.as_bytes())?;
    out.extend(decoder.finish()?);
    if out.is_empty() {
        let trimmed = body.trim();
        if !trimmed.is_empty() && (trimmed.starts_with('{') || trimmed.starts_with('[')) {
            return Err(A2aSseParseError::MissingDataLines);
        }
    }
    Ok(out)
}

fn find_event_delimiter(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0usize;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

fn parse_sse_event_bytes(event_bytes: &[u8]) -> Result<Option<Value>, A2aSseParseError> {
    let raw_event = String::from_utf8(event_bytes.to_vec())?;
    let payload = parse_sse_event_payload(&raw_event)?;
    match payload {
        Some(payload) => Ok(Some(serde_json::from_str(&payload)?)),
        None => Ok(None),
    }
}

fn parse_sse_event_payload(raw_event: &str) -> Result<Option<String>, A2aSseParseError> {
    let normalized = raw_event.replace("\r\n", "\n");
    let raw_event = normalized.trim();
    if raw_event.is_empty() {
        return Ok(None);
    }

    let mut data_lines = Vec::new();
    for line in raw_event.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }

    if data_lines.is_empty() {
        return Ok(None);
    }

    Ok(Some(data_lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_single_event() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"x\":2}}\n\n";
        let v = parse_a2a_sse_json_rpc_chunks(body).expect("parse");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], json!({"jsonrpc":"2.0","id":1,"result":{"x":2}}));
    }

    #[test]
    fn parses_two_events() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{\"stream\":true,\"chunk\":{},\"index\":0,\"final\":false}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{\"stream\":true,\"chunk\":{},\"index\":1,\"final\":true}}\n\n",
        );
        let v = parse_a2a_sse_json_rpc_chunks(body).expect("parse");
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn rejects_json_array_without_sse_framing() {
        let body = "[{\"jsonrpc\":\"2.0\",\"id\":1}]";
        let err = parse_a2a_sse_json_rpc_chunks(body).unwrap_err();
        assert!(matches!(err, A2aSseParseError::MissingDataLines));
    }

    #[test]
    fn empty_body_ok() {
        assert!(parse_a2a_sse_json_rpc_chunks("").unwrap().is_empty());
        assert!(parse_a2a_sse_json_rpc_chunks("   \n  ").unwrap().is_empty());
    }

    #[test]
    fn ignores_comment_only_frames() {
        let body = ": ping\n\n";
        let parsed = parse_a2a_sse_json_rpc_chunks(body).expect("comment-only frame");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parses_data_frames_after_comment_frames() {
        let body = concat!(
            ": keepalive\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n",
        );
        let parsed = parse_a2a_sse_json_rpc_chunks(body).expect("comment plus data");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["id"], 1);
    }

    #[test]
    fn incremental_decoder_emits_complete_events_without_buffering_tail() {
        let mut decoder = A2aSseDecoder::new();
        let first = decoder
            .feed(b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"x\":2}}\n\n")
            .expect("first event");
        let second = decoder
            .feed(b"data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"x\":3}}")
            .expect("partial second event");
        let tail = decoder.finish().expect("finish second event");

        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "unterminated tail must stay buffered");
        assert_eq!(tail.len(), 1);
        assert_eq!(first[0]["id"], 1);
        assert_eq!(tail[0]["id"], 2);
    }

    #[test]
    fn incremental_decoder_handles_chunk_split_delimiter() {
        let mut decoder = A2aSseDecoder::new();
        let before = decoder
            .feed(b"data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
            .expect("partial delimiter");
        let after = decoder.feed(b"\n").expect("complete delimiter");

        assert!(before.is_empty());
        assert_eq!(after.len(), 1);
        assert_eq!(after[0]["id"], 1);
    }
}

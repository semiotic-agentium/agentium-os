//! SSE framing for HTTP POST `/agents/.../a2a`: each event carries one JSON-RPC 2.0 object in `data:` lines.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum A2aSseParseError {
    #[error("SSE stream contained non-whitespace text but no data: lines")]
    MissingDataLines,
    #[error("invalid JSON in SSE data payload: {0}")]
    Json(#[from] serde_json::Error),
}

/// Extract JSON-RPC values from an SSE (`text/event-stream`) response body.
///
/// Implements the minimal subset used by the runner: events separated by a blank line,
/// each event containing one or more `data:` lines (joined with `\n` per SSE rules).
pub fn parse_a2a_sse_json_rpc_chunks(body: &str) -> Result<Vec<Value>, A2aSseParseError> {
    let payloads = split_sse_data_payloads(body);
    if payloads.is_empty() && body.chars().any(|c| !c.is_whitespace()) {
        return Err(A2aSseParseError::MissingDataLines);
    }
    let mut out = Vec::with_capacity(payloads.len());
    for p in payloads {
        out.push(serde_json::from_str(&p)?);
    }
    Ok(out)
}

fn split_sse_data_payloads(body: &str) -> Vec<String> {
    let normalized = body.replace("\r\n", "\n");
    let mut out = Vec::new();
    for raw_event in normalized.split("\n\n") {
        let raw_event = raw_event.trim();
        if raw_event.is_empty() {
            continue;
        }
        let mut data_lines = Vec::new();
        for line in raw_event.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start());
            }
        }
        if data_lines.is_empty() {
            continue;
        }
        out.push(data_lines.join("\n"));
    }
    out
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
}

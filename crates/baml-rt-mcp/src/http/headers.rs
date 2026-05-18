//! Reserved-header guard for operator-supplied static headers.
//!
//! The MCP Streamable HTTP transport owns a small set of headers that must
//! never be set by the operator config: protocol/session correlation
//! (`mcp-session-id`, `mcp-protocol-version`, `last-event-id`), content
//! negotiation (`accept`, `content-type`), and `authorization` (which must always come through the
//! secret-injection path, never as a plaintext static value).
//!
//! Validation is one-shot at launch-config construction. A misconfigured
//! header at this layer is a fail-closed configuration error, not a runtime
//! recoverable.

use baml_rt_tools::mcp_config::HttpHeader;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use thiserror::Error;

/// Headers operators may not set via `static_headers`. The transport itself
/// writes `accept`, `content-type`, `mcp-protocol-version`, and
/// `last-event-id`; the rmcp transport writes `mcp-session-id`;
/// `authorization` only legitimately comes
/// from `SecretInjection::HttpAuthorizationBearer` or `HttpBasicPassword`.
pub const RESERVED_HEADERS: &[&str] = &[
    "accept",
    "authorization",
    "content-type",
    "last-event-id",
    "mcp-protocol-version",
    "mcp-session-id",
];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error(
        "MCP server `{server}` static header `{name}` is reserved; the transport sets it (use the secret-injection path for authorization)"
    )]
    Reserved { server: String, name: String },
    #[error("MCP server `{server}` static header `{name}` is malformed: {reason}")]
    Malformed {
        server: String,
        name: String,
        reason: String,
    },
}

/// Lowercased reserved set lookup. Header names are case-insensitive per
/// RFC 7230, so the comparison is too.
pub fn is_reserved(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    RESERVED_HEADERS.contains(&lower.as_str())
}

/// Validate operator-supplied static headers and build the baseline header
/// map for an MCP HTTP request. Refuses reserved names; refuses malformed
/// values. Returns the validated map ready to hand to rmcp's
/// `custom_headers`.
pub fn build_validated_static_headers(
    server: &str,
    static_headers: &[HttpHeader],
) -> Result<HeaderMap, HeaderError> {
    let mut out = HeaderMap::new();
    for h in static_headers {
        if is_reserved(&h.name) {
            return Err(HeaderError::Reserved {
                server: server.to_string(),
                name: h.name.clone(),
            });
        }
        let name = HeaderName::try_from(h.name.as_str()).map_err(|err| HeaderError::Malformed {
            server: server.to_string(),
            name: h.name.clone(),
            reason: err.to_string(),
        })?;
        let value =
            HeaderValue::try_from(h.value.as_str()).map_err(|err| HeaderError::Malformed {
                server: server.to_string(),
                name: h.name.clone(),
                reason: err.to_string(),
            })?;
        out.insert(name, value);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> HttpHeader {
        HttpHeader {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn accepts_safe_header() {
        let map = build_validated_static_headers("s", &[header("x-tenant", "t1")]).unwrap();
        assert_eq!(map.get("x-tenant").unwrap(), "t1");
    }

    #[test]
    fn rejects_reserved_session_id() {
        let err = build_validated_static_headers("s", &[header("Mcp-Session-Id", "abc")])
            .expect_err("must reject");
        assert!(matches!(err, HeaderError::Reserved { .. }));
    }

    #[test]
    fn rejects_reserved_authorization_case_insensitive() {
        let err = build_validated_static_headers("s", &[header("AUTHORIZATION", "Bearer xyz")])
            .expect_err("must reject");
        assert!(matches!(err, HeaderError::Reserved { .. }));
    }

    #[test]
    fn rejects_reserved_protocol_version() {
        let err = build_validated_static_headers("s", &[header("mcp-protocol-version", "x")])
            .expect_err("must reject");
        assert!(matches!(err, HeaderError::Reserved { .. }));
    }

    #[test]
    fn rejects_reserved_last_event_id_case_insensitive() {
        let err = build_validated_static_headers("s", &[header("Last-Event-ID", "7")])
            .expect_err("must reject");
        assert!(matches!(err, HeaderError::Reserved { .. }));
    }

    #[test]
    fn rejects_malformed_name() {
        let err = build_validated_static_headers("s", &[header("invalid header", "v")])
            .expect_err("must reject");
        assert!(matches!(err, HeaderError::Malformed { .. }));
    }
}

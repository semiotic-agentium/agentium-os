// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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

/// Re-export of the canonical reserved set defined in `baml-rt-tools`. The
/// authoritative copy lives there so config-load validation and runtime
/// guarding share one list and cannot drift.
pub use baml_rt_tools::mcp_config::RESERVED_HTTP_HEADERS as RESERVED_HEADERS;
use baml_rt_tools::mcp_config::{HttpHeader, is_reserved_http_header};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use thiserror::Error;

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
    is_reserved_http_header(name)
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

    struct HeaderCase {
        label: &'static str,
        name: &'static str,
        value: &'static str,
        expect_ok: bool,
        check_err: Option<fn(&HeaderError) -> bool>,
    }

    #[test]
    fn mcp_static_header_policy_matrix() {
        let cases = [
            HeaderCase {
                label: "accepts_safe_header",
                name: "x-tenant",
                value: "t1",
                expect_ok: true,
                check_err: None,
            },
            HeaderCase {
                label: "rejects_reserved_session_id",
                name: "Mcp-Session-Id",
                value: "abc",
                expect_ok: false,
                check_err: Some(|e| matches!(e, HeaderError::Reserved { .. })),
            },
            HeaderCase {
                label: "rejects_reserved_authorization_case_insensitive",
                name: "AUTHORIZATION",
                value: "Bearer xyz",
                expect_ok: false,
                check_err: Some(|e| matches!(e, HeaderError::Reserved { .. })),
            },
            HeaderCase {
                label: "rejects_reserved_protocol_version",
                name: "mcp-protocol-version",
                value: "x",
                expect_ok: false,
                check_err: Some(|e| matches!(e, HeaderError::Reserved { .. })),
            },
            HeaderCase {
                label: "rejects_reserved_last_event_id_case_insensitive",
                name: "Last-Event-ID",
                value: "7",
                expect_ok: false,
                check_err: Some(|e| matches!(e, HeaderError::Reserved { .. })),
            },
            HeaderCase {
                label: "rejects_malformed_name",
                name: "invalid header",
                value: "v",
                expect_ok: false,
                check_err: Some(|e| matches!(e, HeaderError::Malformed { .. })),
            },
        ];
        for case in cases {
            let result = build_validated_static_headers("s", &[header(case.name, case.value)]);
            if case.expect_ok {
                let map = result.expect(case.label);
                assert_eq!(map.get(case.name).unwrap(), case.value, "{}", case.label);
            } else {
                let err = result.expect_err(case.label);
                let check = case.check_err.expect("reject row must supply check_err");
                assert!(check(&err), "{}: {err:?}", case.label);
            }
        }
    }
}

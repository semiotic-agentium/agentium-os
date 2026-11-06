// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Wire protocol v2 (external tools rewrite — see
//! `plans/external-tools-v2-p1-spec.md` §P1.3).
//!
//! v1 ([`crate::protocol`]) is JSON-RPC 2.0, single-shot, one process per call.
//! v2 is a **long-lived, multiplexed** message stream carried over a vsock
//! channel (host is the server; the guest supervisor dials out). Many invokes
//! share one connection, keyed by `request_id`.
//!
//! Framing is unchanged: length-prefixed JSON frames via [`crate::codec`]. Each
//! frame is exactly one [`HostMessage`] or [`GuestMessage`]. The one exception
//! is [`HostMessage::LoadTool`], which is a *header*: the raw bundle bytes
//! follow as subsequent binary frames (`bundle_len` says how many bytes), so a
//! multi-MB bundle never sits inside a JSON string.
//!
//! This module adds no dependencies (serde/serde_json only) so it still ships
//! into distroless guests.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::ErrorClass;

/// Protocol major version advertised in the handshake.
pub const PROTOCOL_V2_VERSION: u32 = 2;

/// Identity envelope carried on every invoke. **Facts only** — the guest gains
/// no authority from these; the host logs/partitions by them. Mirrors the
/// agent-identity plan's fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallContext {
    pub agent_id: String,
    pub artifact_hash: String,
    pub context_id: String,
    pub task_id: String,
    pub invocation_id: String,
    pub trace_id: String,
    pub deadline_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caller_chain: Vec<String>,
}

/// A reveal-tier secret value delivered host→guest over the control channel
/// (§P1.5). Wrapped so its `Debug` never prints the plaintext — a reveal
/// secret must not leak into logs or panics.
#[derive(Clone, Serialize, Deserialize)]
pub struct RevealedSecret(pub String);

impl std::fmt::Debug for RevealedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RevealedSecret(<redacted>)")
    }
}

/// Payload of [`HostMessage::Invoke`] (boxed in the enum to keep variant sizes
/// even). Serializes flat alongside the `"t": "invoke"` tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeRequest {
    pub request_id: u64,
    /// Local MCP tool name within the loaded toolset.
    #[serde(default)]
    pub tool: String,
    pub tool_digest: String,
    pub call_context: CallContext,
    pub input: Value,
    pub deadline_ms: u64,
}

/// Messages the host sends to the guest supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMessage {
    /// Handshake. `grant_class` is the id the host stamped for this VM (§6.3).
    Hello { proto: u32, grant_class: String },
    /// Announce a bundle; the `bundle_len` raw bytes follow in later frames.
    /// The supervisor re-verifies `tool_digest` against the received bytes.
    LoadTool {
        tool_digest: String,
        runtime_image_digest: String,
        bundle_len: u64,
    },
    /// Run one tool invocation. Multiplexed by `request_id`. Boxed because the
    /// payload (identity envelope + input) dwarfs the other variants.
    Invoke(Box<InvokeRequest>),
    /// Best-effort cancel of an in-flight invoke (supervisor SIGKILLs the
    /// worker process group).
    Cancel { request_id: u64 },
    /// Reveal-tier secret for one invoke (§P1.5). Only sent for tools with an
    /// approved `reveal` grant; egress-bound secrets never appear on the wire.
    Secret {
        request_id: u64,
        name: String,
        value: RevealedSecret,
    },
}

/// Messages the guest supervisor sends back to the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum GuestMessage {
    HelloAck {
        proto: u32,
        supervisor: String,
    },
    LoadAck {
        tool_digest: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Optional progress/log for an in-flight invoke (0..N before `Result`).
    Event {
        request_id: u64,
        kind: String,
        payload: Value,
    },
    /// Terminal outcome of one invoke.
    Result {
        request_id: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default)]
        error_class: ErrorClass,
        #[serde(default)]
        truncated: bool,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn roundtrip_host(m: &HostMessage) -> HostMessage {
        serde_json::from_slice(&serde_json::to_vec(m).unwrap()).unwrap()
    }
    fn roundtrip_guest(m: &GuestMessage) -> GuestMessage {
        serde_json::from_slice(&serde_json::to_vec(m).unwrap()).unwrap()
    }

    #[test]
    fn host_messages_roundtrip() {
        let invoke = HostMessage::Invoke(Box::new(InvokeRequest {
            request_id: 7,
            tool: "meteo".into(),
            tool_digest: "sha256:abc".into(),
            call_context: CallContext {
                agent_id: "a".into(),
                invocation_id: "i".into(),
                deadline_ms: 5000,
                ..Default::default()
            },
            input: json!({"latitude": 52.52}),
            deadline_ms: 5000,
        }));
        // invoke serializes flat with the tag
        let jv = serde_json::to_value(&invoke).unwrap();
        assert_eq!(jv["t"], "invoke");
        assert_eq!(jv["request_id"], 7);
        match roundtrip_host(&invoke) {
            HostMessage::Invoke(req) => {
                assert_eq!(req.request_id, 7);
                assert_eq!(req.deadline_ms, 5000);
            }
            other => panic!("wrong variant: {other:?}"),
        }
        // tag discriminates variants
        let v = serde_json::to_value(&HostMessage::Cancel { request_id: 1 }).unwrap();
        assert_eq!(v["t"], "cancel");
    }

    #[test]
    fn guest_result_defaults_error_class() {
        let v = json!({"t": "result", "request_id": 1, "ok": true});
        match serde_json::from_value::<GuestMessage>(v).unwrap() {
            GuestMessage::Result {
                error_class,
                truncated,
                ..
            } => {
                assert_eq!(error_class, ErrorClass::Execution); // default
                assert!(!truncated);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn revealed_secret_debug_is_redacted() {
        let s = RevealedSecret("super-secret-token".into());
        assert_eq!(format!("{s:?}"), "RevealedSecret(<redacted>)");
        // value still serializes (it must cross the wire)
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!("super-secret-token")
        );
    }

    #[test]
    fn roundtrip_guest_result() {
        let r = GuestMessage::Result {
            request_id: 9,
            ok: false,
            output: None,
            error: Some("boom".into()),
            error_class: ErrorClass::Transient,
            truncated: true,
        };
        match roundtrip_guest(&r) {
            GuestMessage::Result {
                error_class,
                truncated,
                error,
                ..
            } => {
                assert_eq!(error_class, ErrorClass::Transient);
                assert!(truncated);
                assert_eq!(error.as_deref(), Some("boom"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}

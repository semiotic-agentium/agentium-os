//! Transport-independent serde types describing an approved MCP server import.
//!
//! These types are shared by the builder, runner, and runtime so they can
//! read snapshots without depending on any MCP protocol/transport crate.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::tools::ToolAccess;

pub const MCP_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Top-level snapshot describing one MCP server import and the tools it exposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSnapshot {
    pub schema_version: u32,
    pub server_id: String,
    pub transport: McpTransportRef,
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<Value>,
    pub server_config_digest: Digest,
    /// Digest of the server-advertised identity contract captured at import
    /// time. Covers `capabilities` + `serverInfo.name` so cosmetic version
    /// bumps don't trip reconnect checks while binary swap / capability
    /// changes still fail closed.
    pub server_identity_digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_artifact_digest: Option<Digest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_refs: Vec<SecretRef>,
    pub approval: ApprovalRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<String>,
    pub tools: Vec<McpImportedTool>,
}

/// Per-tool snapshot entry projected into platform tool metadata after approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpImportedTool {
    pub platform_tool_name: String,
    pub mcp_tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    pub input_schema_digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_digest: Option<Digest>,
    pub output_mode: McpOutputMode,
    pub access_level: ToolAccess,
    pub approval: ApprovalRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque_fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub annotations: Value,
}

/// Approval lifecycle for a server or imported tool entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalState {
    Pending,
    Approved,
    Rejected,
    Stale,
}

impl McpApprovalState {
    pub fn is_approved(self) -> bool {
        matches!(self, McpApprovalState::Approved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub state: McpApprovalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl ApprovalRecord {
    pub fn pending() -> Self {
        Self {
            state: McpApprovalState::Pending,
            owner: None,
            reviewed_at: None,
            expires_at: None,
        }
    }
}

/// Output shape selected at import time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpOutputMode {
    /// Stable content-block envelope; default when no validated output schema exists.
    ContentEnvelope,
    /// Raw opaque JSON wrapper for unsupported or weakly typed outputs.
    OpaqueJson,
    /// Output schema imported and digested.
    JsonSchema { schema: Value, digest: Digest },
}

/// Transport locator. Never carries raw secret values; only references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpTransportRef {
    /// Locally executed stdio MCP server. `command_ref` resolves to a vetted
    /// binary or artifact identifier outside the snapshot file.
    Stdio {
        command_ref: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
    /// Remote MCP server over HTTP/Streamable HTTP. Phase 1 reserves this
    /// variant so snapshots can be parsed; production runtime support lands later.
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowlist_digest: Option<Digest>,
    },
}

/// Reference to a secret stored outside the snapshot. The runtime resolves
/// the actual value through the existing secret-resolver chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SecretRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Newtype wrapper around a digest string. Format is implementation-defined
/// but conventionally `sha256:<hex>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Computes the server identity digest from the MCP `initialize` response.
///
/// Covers only the fields whose change must invalidate the existing
/// approval: server-advertised `capabilities` and `serverInfo.name`. The
/// cosmetic `serverInfo.version` and other implementation fields are
/// deliberately excluded so patch releases of an approved server do not
/// force re-approval.
///
/// `protocolVersion` is enforced by the client's pinned version during
/// `initialize`; a server that speaks a different revision cannot complete
/// the handshake, so including it here would be redundant.
pub fn compute_server_identity_digest(capabilities: &Value, server_info: &Value) -> Digest {
    let server_name = server_info.get("name").cloned().unwrap_or(Value::Null);
    let canonical = json!({
        "capabilities": capabilities,
        "server_info_name": server_name,
    });
    let bytes = serde_jcs::to_vec(&canonical).unwrap_or_else(|_| b"null".to_vec());
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Digest::new(format!("sha256:{:x}", hasher.finalize()))
}

/// Returns true when the server and the named tool are both `Approved`.
/// Builder/runtime callers use this to decide whether a tool can be projected
/// into platform metadata.
pub fn is_tool_projectable(snapshot: &McpServerSnapshot, platform_tool_name: &str) -> bool {
    if !snapshot.approval.state.is_approved() {
        return false;
    }
    snapshot
        .tools
        .iter()
        .find(|tool| tool.platform_tool_name == platform_tool_name)
        .is_some_and(|tool| tool.approval.state.is_approved())
}

/// Iterator over tools whose per-tool approval is `Approved` when the server
/// itself is also `Approved`.
pub fn approved_tools(snapshot: &McpServerSnapshot) -> impl Iterator<Item = &McpImportedTool> {
    let server_approved = snapshot.approval.state.is_approved();
    snapshot
        .tools
        .iter()
        .filter(move |tool| server_approved && tool.approval.state.is_approved())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_tool(name: &str, state: McpApprovalState) -> McpImportedTool {
        McpImportedTool {
            platform_tool_name: format!("fake/{name}"),
            mcp_tool_name: name.to_string(),
            description: Some(format!("{name} description")),
            input_schema: json!({ "type": "object", "properties": {} }),
            input_schema_digest: Digest::new("sha256:input"),
            prompt_digest: None,
            output_mode: McpOutputMode::ContentEnvelope,
            access_level: ToolAccess::Read,
            approval: ApprovalRecord {
                state,
                owner: Some("reviewer@example.com".into()),
                reviewed_at: Some("2026-05-13T00:00:00Z".into()),
                expires_at: None,
            },
            opaque_fallback_reason: None,
            annotations: Value::Null,
        }
    }

    fn sample_snapshot(state: McpApprovalState) -> McpServerSnapshot {
        McpServerSnapshot {
            schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
            server_id: "fake-server".into(),
            transport: McpTransportRef::Stdio {
                command_ref: "fake-mcp".into(),
                args: vec![],
            },
            protocol_version: "2025-06-18".into(),
            server_info: Some(json!({ "name": "fake", "version": "0.1.0" })),
            server_config_digest: Digest::new("sha256:server-config"),
            server_identity_digest: Digest::new("sha256:server-identity"),
            runtime_artifact_digest: None,
            secret_refs: vec![SecretRef {
                name: "fake/api_token".into(),
                version: None,
            }],
            approval: ApprovalRecord {
                state,
                owner: Some("reviewer@example.com".into()),
                reviewed_at: Some("2026-05-13T00:00:00Z".into()),
                expires_at: None,
            },
            sandbox_profile: Some("mcp-import-restricted".into()),
            tools: vec![
                sample_tool("search", McpApprovalState::Approved),
                sample_tool("query", McpApprovalState::Approved),
            ],
        }
    }

    #[test]
    fn round_trip_minimal_approved_snapshot() {
        let snap = sample_snapshot(McpApprovalState::Approved);
        let json = serde_json::to_string(&snap).expect("serialize");
        let parsed: McpServerSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(snap, parsed);
    }

    #[test]
    fn approval_variants_serialize_predictably() {
        for state in [
            McpApprovalState::Pending,
            McpApprovalState::Approved,
            McpApprovalState::Rejected,
            McpApprovalState::Stale,
        ] {
            let snap = sample_snapshot(state);
            let json = serde_json::to_value(&snap).expect("serialize");
            let serialized_state = json
                .pointer("/approval/state")
                .and_then(Value::as_str)
                .expect("state field present");
            let expected = match state {
                McpApprovalState::Pending => "pending",
                McpApprovalState::Approved => "approved",
                McpApprovalState::Rejected => "rejected",
                McpApprovalState::Stale => "stale",
            };
            assert_eq!(serialized_state, expected);
        }
    }

    #[test]
    fn projectable_requires_server_and_tool_approved() {
        let mut snap = sample_snapshot(McpApprovalState::Approved);
        assert!(is_tool_projectable(&snap, "fake/search"));

        snap.tools[0].approval.state = McpApprovalState::Pending;
        assert!(!is_tool_projectable(&snap, "fake/search"));
        assert!(is_tool_projectable(&snap, "fake/query"));

        let stale = sample_snapshot(McpApprovalState::Stale);
        assert!(!is_tool_projectable(&stale, "fake/search"));
    }

    #[test]
    fn approved_tools_excludes_unapproved_entries() {
        let mut snap = sample_snapshot(McpApprovalState::Approved);
        snap.tools
            .push(sample_tool("draft", McpApprovalState::Pending));
        let names: Vec<&str> = approved_tools(&snap)
            .map(|tool| tool.platform_tool_name.as_str())
            .collect();
        assert_eq!(names, vec!["fake/search", "fake/query"]);
    }

    #[test]
    fn approved_tools_empty_when_server_not_approved() {
        let snap = sample_snapshot(McpApprovalState::Rejected);
        assert_eq!(approved_tools(&snap).count(), 0);
    }

    #[test]
    fn snapshot_serialization_contains_no_raw_secret_fields() {
        let snap = sample_snapshot(McpApprovalState::Approved);
        let json = serde_json::to_string(&snap).expect("serialize");
        for forbidden in [
            "\"token\"",
            "\"password\"",
            "\"api_key\"",
            "\"secret_value\"",
            "\"credential\"",
        ] {
            assert!(
                !json.contains(forbidden),
                "snapshot contains forbidden secret field {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn output_mode_variants_round_trip() {
        let envelope = McpOutputMode::ContentEnvelope;
        let opaque = McpOutputMode::OpaqueJson;
        let schema = McpOutputMode::JsonSchema {
            schema: json!({ "type": "object" }),
            digest: Digest::new("sha256:out"),
        };
        for mode in [envelope, opaque, schema] {
            let json = serde_json::to_string(&mode).expect("serialize");
            let parsed: McpOutputMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(mode, parsed);
        }
    }

    #[test]
    fn transport_ref_variants_round_trip() {
        let stdio = McpTransportRef::Stdio {
            command_ref: "fake-mcp".into(),
            args: vec!["--quiet".into()],
        };
        let http = McpTransportRef::Http {
            url: "https://example.invalid/mcp".into(),
            allowlist_digest: Some(Digest::new("sha256:allow")),
        };
        for transport in [stdio, http] {
            let json = serde_json::to_string(&transport).expect("serialize");
            let parsed: McpTransportRef = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(transport, parsed);
        }
    }

    #[test]
    fn digest_stable_for_same_input() {
        let a = Digest::new("sha256:abc");
        let b = Digest::new("sha256:abc");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "sha256:abc");
    }
}

//! MCP registry domain types.

use baml_rt_tools::{
    mcp_snapshot::{Digest, McpApprovalState, McpServerSnapshot, canonical_digest},
    tools::ToolAccess,
};
use serde::{Deserialize, Serialize};

/// Registry row for an MCP server id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRegistryServer {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<u32>,
}

/// Immutable registry version for one imported MCP server snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRegistryServerVersion {
    pub server_id: String,
    pub version: u32,
    pub snapshot_digest: Digest,
    pub server_config_digest: Digest,
    pub server_identity_digest: Digest,
    pub tools_digest: Digest,
    pub protocol_version: String,
    pub transport_json: serde_json::Value,
    pub secret_refs_json: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<String>,
    pub approval_state: McpApprovalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_at: Option<String>,
}

/// Per-tool registry projection tied to a server version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRegistryToolVersion {
    pub server_id: String,
    pub server_version: u32,
    pub platform_tool_name: String,
    pub mcp_tool_name: String,
    pub input_schema_digest: Digest,
    pub output_mode_json: serde_json::Value,
    pub access_level: ToolAccess,
    pub approval_state: McpApprovalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque_fallback_reason: Option<String>,
    pub tool_json: serde_json::Value,
}

/// Full snapshot payload addressed by digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpSnapshotBlob {
    pub snapshot_digest: Digest,
    pub snapshot_json: String,
}

/// Computes the content digest used for immutable snapshot versions.
pub fn compute_snapshot_digest(snapshot: &McpServerSnapshot) -> Digest {
    canonical_digest(snapshot)
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External-tool registry domain types.
//!
//! Mirrors the MCP registry layout ([`crate::mcp`]) for approved external-tool
//! snapshots: one row per tool id, one immutable row per version, and a
//! content-addressed blob holding the full snapshot JSON.

use baml_rt_tools::{
    external_tools::ExternalApprovalState, mcp_snapshot::Digest,
};
use serde::{Deserialize, Serialize};

/// Registry row for one external-tool id (`bundle/local`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalToolRegistryTool {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<u32>,
}

/// Immutable registry version for one imported external-tool snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalToolRegistryToolVersion {
    pub tool_name: String,
    pub version: u32,
    pub snapshot_digest: Digest,
    pub manifest_digest: Digest,
    pub schema_digest: Digest,
    pub runtime_digest: Digest,
    pub protocol_version: String,
    pub runtime_json: serde_json::Value,
    pub secrets_json: serde_json::Value,
    pub capabilities_json: serde_json::Value,
    pub approval_state: ExternalApprovalState,
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

/// Full snapshot payload addressed by digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalToolSnapshotBlob {
    pub snapshot_digest: Digest,
    pub snapshot_json: String,
}

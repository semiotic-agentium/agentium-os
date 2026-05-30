// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent discovery catalogue: cards, entries, and the [`AgentLister`] trait for GET /agents and tools.

use serde::{Deserialize, Serialize};

use crate::EventSubscription;

/// Cut-down A2A-like agent card for discovery (included in every GET /agents entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub version: String,
    /// Repository content hash identity (sha256 tar.gz), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Repository monotonic version number, when deployed from repository metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_version: Option<u32>,
    /// Route key for A2A dispatch.
    pub agent_package: String,
    pub agent_instance_id: String,
    /// Tool names declared in manifest.
    #[serde(default)]
    pub tools: Vec<String>,
    /// BAML function names registered in the agent's runtime (populated at boot).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baml_functions: Vec<String>,
    /// From manifest.discovery when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Manifest tags (single source of truth).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Event subscriptions declared by this agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EventSubscription>,
}

/// Narrow trait: lists running agents. HTTP GET /agents and system/discover_agents depend on this.
pub trait AgentLister: Send + Sync {
    fn list_agents(&self) -> Vec<AgentDiscoveryEntry>;
}

/// Discovery entry for one running agent instance (GET /agents).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDiscoveryEntry {
    pub agent_package: String,
    pub agent_instance_id: String,
    /// Manifest name (human-readable).
    pub name: String,
    pub version: String,
    /// Agent card (cut-down shape) for discovery.
    pub agent_card: AgentCard,
}

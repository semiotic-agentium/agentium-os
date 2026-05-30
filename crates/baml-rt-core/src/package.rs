// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent package manifest (manifest.json) — shared contract between builder and runner.

use serde::{Deserialize, Serialize};

use crate::EventSubscription;

fn default_entry_point() -> String {
    "dist/index.js".to_string()
}

/// Optional non-derivable card metadata for discovery (description, capabilities, subscriptions).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestDiscovery {
    /// Description for discovery listing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Capability labels for filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Event subscriptions this agent wants the host to deliver.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EventSubscription>,
}

/// Canonical agent package manifest (manifest.json).
/// Single source of truth for the package contract between baml-rt-builder and baml-agent-runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub version: String,
    pub name: String,
    #[serde(default = "default_entry_point")]
    pub entry_point: String,
    /// Stable archive identity; packager sets to UUID if missing when packaging.
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional discovery card metadata (non-derivable fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<ManifestDiscovery>,
}

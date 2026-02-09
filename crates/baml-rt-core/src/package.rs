//! Agent package manifest (manifest.json) — shared contract between builder and runner.

use serde::{Deserialize, Serialize};

fn default_entry_point() -> String {
    "dist/index.js".to_string()
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
}

//! Repository entry types: the canonical representation of a stored agent.
//!
//! An entry is the atomic unit of the repository. It bundles immutable source
//! content (ts + baml + manifest.json) with mutable metadata (fitness scores,
//! tags) and structural lineage information.

use serde::{Deserialize, Serialize};

use crate::ids::{ContentHash, Generation, VersionRef};
use crate::lineage::Parentage;

// ---------------------------------------------------------------------------
// Source content — the canonical hash inputs
// ---------------------------------------------------------------------------

/// A single source file within a package, identified by its path relative to
/// the package root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    pub path: SourcePath,
    pub content: SourceContent,
}

/// Relative path within a package (e.g. `src/index.ts`, `baml_src/main.baml`).
///
/// Normalised to forward slashes, no leading slash, no `..` components.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourcePath(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid source path: {reason}")]
pub struct SourcePathError {
    pub reason: &'static str,
}

impl SourcePath {
    pub fn new(path: impl Into<String>) -> Result<Self, SourcePathError> {
        let s = path.into();
        if s.is_empty() {
            return Err(SourcePathError {
                reason: "path must not be empty",
            });
        }
        if s.starts_with('/') {
            return Err(SourcePathError {
                reason: "path must be relative (no leading slash)",
            });
        }
        if s.contains("..") {
            return Err(SourcePathError {
                reason: "path must not contain '..' components",
            });
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourcePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Raw file content as UTF-8 text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceContent(String);

impl SourceContent {
    pub fn new(content: impl Into<String>) -> Self {
        Self(content.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Source bundle — the complete canonical source for hashing
// ---------------------------------------------------------------------------

/// The complete source bundle that defines an agent package.
///
/// This is the input to the canonical hashing function. It contains:
/// - The manifest (manifest.json) — authored agent contract.
/// - TypeScript source files — agent implementation.
/// - BAML prompt files — LLM interaction definitions.
///
/// Runtime-generated artefacts (d.ts, tsconfig, compiled JS, baml_client/)
/// are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceBundle {
    /// The agent manifest (manifest.json). Always present.
    pub manifest: ManifestSource,
    /// TypeScript source files, sorted by path for canonical ordering.
    pub ts_sources: Vec<SourceFile>,
    /// BAML prompt files, sorted by path for canonical ordering.
    pub baml_sources: Vec<SourceFile>,
}

/// The raw manifest.json content, stored as structured JSON so it can be
/// queried without full deserialization into `AgentManifest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ManifestSource(serde_json::Value);

impl ManifestSource {
    pub fn new(value: serde_json::Value) -> Self {
        Self(value)
    }

    pub fn as_value(&self) -> &serde_json::Value {
        &self.0
    }

    /// Extract the agent name from the manifest.
    pub fn name(&self) -> Option<&str> {
        self.0.get("name")?.as_str()
    }

    /// Extract the manifest version string.
    pub fn version(&self) -> Option<&str> {
        self.0.get("version")?.as_str()
    }

    /// Extract the tools list.
    pub fn tools(&self) -> Vec<&str> {
        self.0
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default()
    }

    /// Extract capabilities from discovery metadata.
    pub fn capabilities(&self) -> Vec<&str> {
        self.0
            .get("discovery")
            .and_then(|d| d.get("capabilities"))
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default()
    }

    /// Extract description from discovery metadata.
    pub fn description(&self) -> Option<&str> {
        self.0.get("discovery")?.get("description")?.as_str()
    }
}

// ---------------------------------------------------------------------------
// Change rationale — mandatory on every publish
// ---------------------------------------------------------------------------

/// Mandatory explanation of *why* a change was made, stored alongside the entry.
///
/// Non-empty by construction. Every publish or fork must carry a rationale.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChangeRationale(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("change rationale must not be empty")]
pub struct ChangeRationaleEmpty;

impl ChangeRationale {
    pub fn new(rationale: impl Into<String>) -> Result<Self, ChangeRationaleEmpty> {
        let s = rationale.into();
        if s.trim().is_empty() {
            return Err(ChangeRationaleEmpty);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChangeRationale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Timestamps — typed wrapper for repository-internal timestamps
// ---------------------------------------------------------------------------

/// UTC timestamp in RFC 3339 format, used for repository event ordering.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(String);

impl Timestamp {
    /// Create a timestamp from an RFC 3339 string. No validation beyond
    /// non-emptiness — the storage layer is responsible for producing valid
    /// timestamps.
    pub fn new(rfc3339: impl Into<String>) -> Self {
        Self(rfc3339.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Fitness — evaluation scores attached to an entry
// ---------------------------------------------------------------------------

/// A single evaluation score for a specific domain/benchmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitnessScore {
    pub domain: FitnessDomain,
    pub score: f64,
    pub recorded_at: Timestamp,
}

/// The domain or benchmark against which an agent was evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FitnessDomain(String);

impl FitnessDomain {
    pub fn new(domain: impl Into<String>) -> Self {
        Self(domain.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FitnessDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// RepositoryEntry — the complete stored agent record
// ---------------------------------------------------------------------------

/// A complete, immutable agent record in the repository.
///
/// The `hash` is the primary key. The `version_ref` is the human-friendly
/// secondary key. Both are unique within the repository.
///
/// Source content and lineage are immutable after creation. Metadata (fitness
/// scores, tags) may be appended but never modified in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryEntry {
    // --- Identity ---
    pub hash: ContentHash,
    pub version_ref: VersionRef,

    // --- Content ---
    pub source: SourceBundle,

    // --- Lineage ---
    pub parentage: Parentage,
    pub generation: Generation,

    // --- Provenance ---
    pub change_rationale: ChangeRationale,
    pub created_at: Timestamp,

    // --- Mutable metadata ---
    pub fitness_scores: Vec<FitnessScore>,
    pub tags: Vec<Tag>,
}

/// A searchable label attached to an entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tag(String);

impl Tag {
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// RepositoryEntryHeader — lightweight projection for listings
// ---------------------------------------------------------------------------

/// A lightweight projection of a `RepositoryEntry` for search results and
/// listings. Excludes full source content to keep payloads small.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryEntryHeader {
    pub hash: ContentHash,
    pub version_ref: VersionRef,
    pub parentage: Parentage,
    pub generation: Generation,
    pub change_rationale: ChangeRationale,
    pub created_at: Timestamp,
    pub fitness_scores: Vec<FitnessScore>,
    pub tags: Vec<Tag>,
    /// Extracted from manifest for quick display.
    pub description: Option<String>,
    /// Tool names from manifest.
    pub tools: Vec<String>,
    /// Capability labels from manifest discovery.
    pub capabilities: Vec<String>,
}

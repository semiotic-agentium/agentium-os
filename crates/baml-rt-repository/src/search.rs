//! Search query types for repository metadata and source content.
//!
//! Queries are structured values — not raw SQL or free-form strings. The type
//! system ensures only valid filter combinations reach the storage layer.

use serde::{Deserialize, Serialize};

use crate::{
    entry::FitnessDomain,
    ids::{AgentName, Generation},
    lineage::LineageKind,
};

// ---------------------------------------------------------------------------
// SearchQuery — the top-level query structure
// ---------------------------------------------------------------------------

/// A structured search over the repository.
///
/// All filters are conjunctive (AND). An empty query matches all entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Full-text search over source content (ts + baml + manifest).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<FullTextTerm>,

    /// Filter by agent name (exact match).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<AgentName>,

    /// Filter by capabilities declared in the manifest.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub capabilities: Vec<CapabilityFilter>,

    /// Filter by tools declared in the manifest.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<ToolFilter>,

    /// Filter by tags attached to entries.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<TagFilter>,

    /// Filter by minimum fitness score in a domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_fitness: Option<FitnessFilter>,

    /// Filter by lineage depth range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationFilter>,

    /// Filter by lineage relationship to a specific entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<LineageFilter>,

    /// Maximum number of results to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,

    /// Result ordering.
    #[serde(default)]
    pub order: SearchOrder,
}

// ---------------------------------------------------------------------------
// Filter types — each is a distinct, validated value
// ---------------------------------------------------------------------------

/// Full-text search term for content search (FTS5 or similar).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FullTextTerm(String);

impl FullTextTerm {
    pub fn new(term: impl Into<String>) -> Self {
        Self(term.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Filter entries that declare a specific capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityFilter(String);

impl CapabilityFilter {
    pub fn new(capability: impl Into<String>) -> Self {
        Self(capability.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Filter entries that declare a specific tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolFilter(String);

impl ToolFilter {
    pub fn new(tool: impl Into<String>) -> Self {
        Self(tool.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Filter entries that carry a specific tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TagFilter(String);

impl TagFilter {
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Minimum fitness score in a specific domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitnessFilter {
    pub domain: FitnessDomain,
    pub min_score: f64,
}

/// Filter by lineage generation range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationFilter {
    pub min: Option<Generation>,
    pub max: Option<Generation>,
}

/// Filter by lineage relationship to a specific entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageFilter {
    pub relation: LineageRelation,
}

/// The type of lineage relationship to filter by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineageRelation {
    /// Entries that are descendants of a given hash.
    DescendantOf {
        ancestor: crate::ids::ContentHash,
        kind: Option<LineageKind>,
    },
    /// Entries that are ancestors of a given hash.
    AncestorOf {
        descendant: crate::ids::ContentHash,
        kind: Option<LineageKind>,
    },
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

/// Result ordering for search queries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchOrder {
    /// Most recently created first.
    #[default]
    Newest,
    /// Oldest first.
    Oldest,
    /// Highest fitness score first (requires `min_fitness` domain).
    HighestFitness,
    /// Most relevant to full-text query first.
    Relevance,
}

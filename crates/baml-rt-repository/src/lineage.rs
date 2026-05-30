// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Lineage graph: first-class edges encoding derivation and influence.
//!
//! The repository is a DAG of agent entries connected by typed edges.
//! Two edge kinds exist:
//!
//! - **Fork**: hard derivation — "this agent was created by mutating that one."
//! - **Influence**: soft reference — "this agent was informed by those entries."
//!
//! An agent may have zero parents (original), one parent (fork), or many
//! influences (synthesis / group ADAS). Fork and influence are structurally
//! distinct so the graph can answer different questions: "what was directly
//! derived?" vs "what references informed this design?"

use serde::{Deserialize, Serialize};

use crate::ids::{ContentHash, Generation, LineageEdgeId};

// ---------------------------------------------------------------------------
// Edge kind — discriminated, not a boolean flag
// ---------------------------------------------------------------------------

/// The nature of a lineage relationship.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineageKind {
    /// Hard derivation: the child was created by directly mutating the parent.
    Fork,
    /// Soft reference: the child was informed by this entry (e.g. ADAS archive
    /// selection, cross-pollination, prompt context).
    Influence,
}

// ---------------------------------------------------------------------------
// LineageEdge — a single directed relationship in the DAG
// ---------------------------------------------------------------------------

/// A directed edge from `source` (parent/reference) to `target` (child/derived).
///
/// Immutable once recorded. The `description` captures the *rationale* for this
/// specific relationship (what mutation was applied, why this reference was
/// selected).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageEdge {
    pub id: LineageEdgeId,
    pub source: ContentHash,
    pub target: ContentHash,
    pub kind: LineageKind,
    pub description: EdgeDescription,
}

/// Human-readable description of a lineage edge — why this relationship exists.
///
/// Non-empty by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EdgeDescription(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("edge description must not be empty")]
pub struct EdgeDescriptionEmpty;

impl EdgeDescription {
    pub fn new(description: impl Into<String>) -> Result<Self, EdgeDescriptionEmpty> {
        let s = description.into();
        if s.trim().is_empty() {
            return Err(EdgeDescriptionEmpty);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EdgeDescription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Parentage — the typed origin of an agent entry
// ---------------------------------------------------------------------------

/// Describes how an agent entry came into being.
///
/// A discriminated union that makes the three origin cases structurally
/// distinct. An `Original` agent has no parents; a `Forked` agent has exactly
/// one fork parent; a `Synthesized` agent was influenced by one or more
/// references (with no single fork parent).
///
/// This is **not** `Option<Vec<...>>`. Invalid states (e.g. a "fork" with zero
/// parents) are unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parentage {
    /// No parent. This agent is an original creation.
    Original,

    /// Created by directly mutating a single parent.
    Forked {
        parent: ContentHash,
        description: EdgeDescription,
    },

    /// Synthesized from one or more reference agents (ADAS archive selection,
    /// cross-pollination). All edges are `LineageKind::Influence`.
    Synthesized {
        /// Non-empty by construction (enforced at creation).
        influences: Vec<InfluenceRef>,
    },
}

/// A single influence reference within a `Synthesized` parentage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfluenceRef {
    pub source: ContentHash,
    pub description: EdgeDescription,
}

// ---------------------------------------------------------------------------
// Ancestry — query result types
// ---------------------------------------------------------------------------

/// A node in an ancestry traversal result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AncestryNode {
    pub hash: ContentHash,
    pub generation: Generation,
    pub parentage: Parentage,
}

/// A subgraph of the lineage DAG centered on a particular entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageSubgraph {
    /// The focal entry.
    pub root: ContentHash,
    /// Ancestor nodes ordered by generation (root-most first).
    pub ancestors: Vec<AncestryNode>,
    /// Direct descendant nodes (one level deep).
    pub descendants: Vec<AncestryNode>,
    /// All edges within this subgraph.
    pub edges: Vec<LineageEdge>,
}

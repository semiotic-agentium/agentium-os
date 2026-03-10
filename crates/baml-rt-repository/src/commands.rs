//! Command types for repository write operations.
//!
//! Each command is a complete, validated request to mutate repository state.
//! Commands are distinct types (not a god-enum with optional fields) so that
//! invalid operations are unrepresentable.

use serde::{Deserialize, Serialize};

use crate::entry::{ChangeRationale, SourceBundle, Tag};
use crate::ids::{AgentName, ContentHash};
use crate::lineage::{EdgeDescription, InfluenceRef};

// ---------------------------------------------------------------------------
// Publish — add a new version to an existing or new lineage
// ---------------------------------------------------------------------------

/// Publish a new agent version. The repository assigns the next version number
/// automatically.
///
/// If the `AgentName` does not yet exist, a new lineage begins at v1 with
/// `Parentage::Original`.
///
/// If the `AgentName` already exists, the next version is appended and
/// parentage describes the relationship to prior versions or external
/// influences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishCommand {
    pub name: AgentName,
    pub source: SourceBundle,
    pub rationale: ChangeRationale,
    pub origin: PublishOrigin,
    pub tags: Vec<Tag>,
}

/// How this published version relates to existing entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PublishOrigin {
    /// Brand-new creation with no parent.
    Original,

    /// Iteration on the same lineage — the source evolved from the previous
    /// version of this agent. The repository records an implicit fork edge
    /// from the prior version.
    Iteration,

    /// Explicitly influenced by one or more reference agents.
    Influenced { influences: Vec<InfluenceRef> },
}

// ---------------------------------------------------------------------------
// Fork — create a new lineage from an existing entry
// ---------------------------------------------------------------------------

/// Fork an existing entry into a new lineage. The new lineage starts at v1
/// under the given `new_name`. A `LineageKind::Fork` edge is recorded from
/// `source_hash` to the new entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkCommand {
    pub source_hash: ContentHash,
    pub new_name: AgentName,
    pub source: SourceBundle,
    pub rationale: ChangeRationale,
    pub fork_description: EdgeDescription,
    pub tags: Vec<Tag>,
}

// ---------------------------------------------------------------------------
// RecordFitness — post-evaluation score update
// ---------------------------------------------------------------------------

/// Record a fitness evaluation result for an existing entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordFitnessCommand {
    pub hash: ContentHash,
    pub domain: crate::entry::FitnessDomain,
    pub score: f64,
}

// ---------------------------------------------------------------------------
// PublishResult — the outcome of a successful publish or fork
// ---------------------------------------------------------------------------

/// Returned after a successful publish or fork.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResult {
    pub hash: ContentHash,
    pub version_ref: crate::ids::VersionRef,
    pub generation: crate::ids::Generation,
}

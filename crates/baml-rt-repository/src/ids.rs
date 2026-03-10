//! Strongly-typed identifiers for the agent repository domain.
//!
//! Every distinct concept receives its own newtype. No bare `String` or `Uuid`
//! crosses a public boundary; the type system enforces that a `ContentHash`
//! cannot be confused with an `AgentName`, a `Version` cannot masquerade as a
//! `Generation`, and lineage edges carry typed endpoints.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ContentHash — the canonical content-addressable key
// ---------------------------------------------------------------------------

/// SHA-256 digest of the canonical agent source content.
///
/// Computed over `manifest.json || sorted .ts sources || sorted .baml prompts`,
/// each prefixed with a length-delimited header. Two packages with identical
/// source produce identical hashes; runtime-generated artefacts are excluded.
///
/// Represented as lowercase hex (64 chars).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

/// Rejection when parsing a `ContentHash` from a string that is not valid hex-64.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid content hash: expected 64 lowercase hex chars, got {length} chars")]
pub struct ContentHashParseError {
    pub length: usize,
}

impl ContentHash {
    /// Wrap a pre-validated hex-64 string. Callers must ensure the invariant.
    /// Used by the canonical hash computation in the service layer.
    #[allow(dead_code)]
    pub(crate) fn from_validated(hex: String) -> Self {
        debug_assert!(
            hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "ContentHash invariant violated"
        );
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ContentHash {
    type Err = ContentHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
            Ok(Self(s.to_string()))
        } else {
            Err(ContentHashParseError { length: s.len() })
        }
    }
}

// ---------------------------------------------------------------------------
// AgentName — the logical identity of an agent lineage
// ---------------------------------------------------------------------------

/// Logical agent name scoping a version lineage (e.g. `"weather-planner"`).
///
/// Must be non-empty, lowercase alphanumeric + hyphens, no leading/trailing
/// hyphens.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentName(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid agent name: {reason}")]
pub struct AgentNameParseError {
    pub reason: &'static str,
}

impl AgentName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentName {
    type Err = AgentNameParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(AgentNameParseError {
                reason: "name must not be empty",
            });
        }
        if s.starts_with('-') || s.ends_with('-') {
            return Err(AgentNameParseError {
                reason: "name must not start or end with a hyphen",
            });
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(AgentNameParseError {
                reason: "name must be lowercase alphanumeric with hyphens only",
            });
        }
        Ok(Self(s.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Version — monotonic integer within an AgentName lineage
// ---------------------------------------------------------------------------

/// Monotonically increasing version within a single `AgentName` lineage.
///
/// Starts at 1 for every new lineage (original or fork). Increments by 1 on
/// each publish. Never zero, never skipped, never reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(u32);

#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid version: version must be >= 1, got {value}")]
pub struct VersionError {
    pub value: u32,
}

impl Version {
    /// The first version in any lineage.
    pub const FIRST: Self = Self(1);

    /// Construct from a raw integer. Rejects zero.
    pub fn new(value: u32) -> Result<Self, VersionError> {
        if value == 0 {
            return Err(VersionError { value });
        }
        Ok(Self(value))
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }

    /// Produce the next sequential version.
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{n}", n = self.0)
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let digits = s.strip_prefix('v').unwrap_or(s);
        let value = digits.parse::<u32>().map_err(|_| VersionError { value: 0 })?;
        Self::new(value)
    }
}

// ---------------------------------------------------------------------------
// Generation — distance from root in the lineage DAG
// ---------------------------------------------------------------------------

/// Lineage depth from the original (root) agent. An original agent has
/// generation 0; each fork/derivation increments by 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Generation(u32);

impl Generation {
    /// The root generation — an agent with no parents.
    pub const ROOT: Self = Self(0);

    pub fn new(depth: u32) -> Self {
        Self(depth)
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn increment(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gen-{n}", n = self.0)
    }
}

// ---------------------------------------------------------------------------
// VersionRef — the human-friendly compound key
// ---------------------------------------------------------------------------

/// Human-readable agent reference: `(AgentName, Version)`.
///
/// Every `VersionRef` maps to exactly one `ContentHash`, and vice versa.
/// This is the user-facing key; the `ContentHash` is the storage key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VersionRef {
    pub name: AgentName,
    pub version: Version,
}

impl fmt::Display for VersionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{name}@{version}", name = self.name, version = self.version)
    }
}

// ---------------------------------------------------------------------------
// LineageEdgeId — unique identity for a lineage relationship
// ---------------------------------------------------------------------------

/// Opaque identifier for a single lineage edge (parent → child).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LineageEdgeId(String);

impl LineageEdgeId {
    pub fn from_uuid(id: uuid::Uuid) -> Self {
        Self(id.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LineageEdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

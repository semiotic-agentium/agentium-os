// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Canonical digest for the behavior-defining contents of an agent package.

use std::{collections::BTreeSet, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// SHA-256 digest of an agent package's canonical behavior set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentIdentityDigest(String);

/// A path and the SHA-256 digest of that entry's exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentIdentityEntry {
    path: String,
    sha256_hex: String,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AgentIdentityError {
    #[error("agent identity behavior set is empty")]
    EmptyBehaviorSet,
    #[error("invalid agent identity path `{path}`")]
    InvalidPath { path: String },
    #[error("duplicate or case-colliding agent identity path `{path}`")]
    DuplicatePath { path: String },
}

/// Rejection when parsing an identity digest that is not lowercase SHA-256 hex.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("invalid agent identity digest: expected 64 lowercase hex chars, got {length} chars")]
pub struct AgentIdentityDigestParseError {
    pub length: usize,
}

impl AgentIdentityEntry {
    /// Reduce exact file bytes to an identity entry.
    pub fn from_bytes(path: impl Into<String>, bytes: &[u8]) -> Result<Self, AgentIdentityError> {
        let path = path.into();
        validate_path(&path)?;
        Ok(Self {
            path,
            sha256_hex: format!("{:x}", Sha256::digest(bytes)),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl AgentIdentityDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    fn from_validated(hex: String) -> Self {
        debug_assert_eq!(hex.len(), 64);
        Self(hex)
    }
}

impl fmt::Display for AgentIdentityDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentIdentityDigest {
    type Err = AgentIdentityDigestParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() == 64
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            Ok(Self(s.to_owned()))
        } else {
            Err(AgentIdentityDigestParseError { length: s.len() })
        }
    }
}

/// Compute the fixed, path-sorted agent identity digest.
pub fn compute_agent_identity_digest(
    entries: &[AgentIdentityEntry],
) -> Result<AgentIdentityDigest, AgentIdentityError> {
    if entries.is_empty() {
        return Err(AgentIdentityError::EmptyBehaviorSet);
    }

    let mut entries = entries.to_vec();
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut paths = BTreeSet::new();
    for entry in &entries {
        if !paths.insert(entry.path.to_ascii_lowercase()) {
            return Err(AgentIdentityError::DuplicatePath {
                path: entry.path.clone(),
            });
        }
    }

    let mut payload = b"urn:karakara:agent-identity\n".to_vec();
    for entry in entries {
        payload.extend_from_slice(entry.path.as_bytes());
        payload.push(0);
        payload.extend_from_slice(entry.sha256_hex.as_bytes());
        payload.push(b'\n');
    }
    Ok(AgentIdentityDigest::from_validated(format!(
        "{:x}",
        Sha256::digest(payload)
    )))
}

fn validate_path(path: &str) -> Result<(), AgentIdentityError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(AgentIdentityError::InvalidPath {
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AgentIdentityEntry, compute_agent_identity_digest};

    #[test]
    fn digest_is_sorted_and_framed() {
        let digest = compute_agent_identity_digest(&[
            AgentIdentityEntry::from_bytes("src/b.ts", b"b").unwrap(),
            AgentIdentityEntry::from_bytes("src/a.ts", b"a").unwrap(),
        ])
        .unwrap();
        assert_eq!(
            digest.as_str(),
            "033d5aed34df444fb5815f8df8aed4bd4a16639dfe5f365401357ea76c508854"
        );
    }
}

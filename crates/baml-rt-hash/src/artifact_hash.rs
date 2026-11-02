// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! The [`ArtifactHash`] newtype, a SHA-256 digest of exact stored artifact bytes.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// SHA-256 digest of the exact bytes of a stored agent package.
///
/// Unlike [`crate::ContentHash`], this covers the `.tar.gz` blob itself, including its
/// compression framing and every packaged file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactHash(String);

/// Rejection when parsing an [`ArtifactHash`] that is not lowercase SHA-256 hex.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid artifact hash: expected 64 lowercase hex chars, got {length} chars")]
pub struct ArtifactHashParseError {
    pub length: usize,
}

impl ArtifactHash {
    /// Hash exact stored artifact bytes. This remains available for blob-store
    /// callers and for legacy records.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    /// Hash a canonical agent identity projection rather than the package
    /// archive. Agent artifact identity uses this form so executable bytes of
    /// an external tool do not re-key an agent whose tool interfaces did not
    /// change.
    pub fn from_identity_bytes(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ArtifactHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ArtifactHash {
    type Err = ArtifactHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() == 64
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            Ok(Self(s.to_owned()))
        } else {
            Err(ArtifactHashParseError { length: s.len() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ArtifactHash;

    #[test]
    fn hashes_exact_bytes() {
        assert_eq!(
            ArtifactHash::from_bytes(b"agent artifact").as_str(),
            "2e053f7783d6ef250ba77f7c7d63e0035b026a0f21243c87ba0455a664f2ef1a"
        );
        assert_ne!(
            ArtifactHash::from_bytes(b"agent artifact"),
            ArtifactHash::from_bytes(b"agent artifact\n")
        );
    }
}

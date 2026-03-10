//! The `ContentHash` newtype — a validated SHA-256 hex-64 digest.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// SHA-256 digest of the canonical agent source content.
///
/// Represented as lowercase hex (64 chars). Immutable after construction.
/// The only ways to obtain one are:
///
/// - Parse from a validated string via `FromStr`.
/// - Compute from source content via `CanonicalHasher`.
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
    /// Wrap a pre-validated hex-64 string produced by the hasher.
    ///
    /// Not public — only `CanonicalHasher::finish()` calls this.
    pub(crate) fn from_validated(hex: String) -> Self {
        debug_assert!(
            hex.len() == 64
                && hex
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "ContentHash invariant violated"
        );
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
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
        if s.len() == 64
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            Ok(Self(s.to_string()))
        } else {
            Err(ContentHashParseError { length: s.len() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_hash() {
        let hex = "a".repeat(64);
        let hash: ContentHash = hex.parse().unwrap();
        assert_eq!(hash.as_str(), hex);
    }

    #[test]
    fn parse_rejects_short() {
        let result = "abc".parse::<ContentHash>();
        assert!(result.is_err());
    }

    #[test]
    fn parse_rejects_uppercase() {
        let hex = "A".repeat(64);
        let result = hex.parse::<ContentHash>();
        assert!(result.is_err());
    }

    #[test]
    fn parse_rejects_non_hex() {
        let hex = "g".repeat(64);
        let result = hex.parse::<ContentHash>();
        assert!(result.is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let hex = "abcdef0123456789".repeat(4);
        let hash: ContentHash = hex.parse().unwrap();
        let json = serde_json::to_string(&hash).unwrap();
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, back);
    }
}

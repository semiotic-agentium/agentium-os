// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! The platform digest convention: JCS-canonicalised JSON + SHA-256, rendered
//! as `sha256:<64 lowercase hex>`.
//!
//! This is the single definition shared by host and guest. `baml-rt-tools`
//! re-exports it as `mcp_snapshot::{Digest, canonical_digest}` so existing
//! paths keep working.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Validated SHA-256 digest rendered on the wire as `sha256:<64 lowercase hex>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    pub fn new(value: impl AsRef<str>) -> Self {
        value
            .as_ref()
            .parse()
            .expect("invalid sha256 digest string")
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_prefixed_hex(self) -> String {
        format!("sha256:{}", hex_lower(&self.0))
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sha256:")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Digest {
    type Err = DigestParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix("sha256:")
            .ok_or(DigestParseError::MissingPrefix)?;
        if hex.len() != 64 {
            return Err(DigestParseError::InvalidLength { len: hex.len() });
        }
        let mut bytes = [0_u8; 32];
        let chunks = hex.as_bytes().as_chunks::<2>().0;
        for (idx, chunk) in chunks.iter().enumerate() {
            let hi = decode_hex_nibble(chunk[0]).ok_or(DigestParseError::InvalidHex)?;
            let lo = decode_hex_nibble(chunk[1]).ok_or(DigestParseError::InvalidHex)?;
            bytes[idx] = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_prefixed_hex())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DigestParseError {
    #[error("digest must start with `sha256:`")]
    MissingPrefix,
    #[error("sha256 digest must contain 64 hex chars, got {len}")]
    InvalidLength { len: usize },
    #[error("sha256 digest contains non-hex characters")]
    InvalidHex,
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_lower(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("write to string");
    }
    out
}

/// Hashes a value with JCS-canonicalised serde JSON then SHA-256, returning
/// the platform's `sha256:<hex>` digest.
pub fn canonical_digest<T: Serialize + ?Sized>(value: &T) -> Digest {
    let bytes = serde_jcs::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Digest::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_roundtrips_on_the_wire() {
        let digest = canonical_digest(&serde_json::json!({"a": 1}));
        let rendered = digest.to_string();
        assert!(rendered.starts_with("sha256:"));
        assert_eq!(rendered.parse::<Digest>().unwrap(), digest);
        let jv = serde_json::to_value(digest).unwrap();
        assert_eq!(jv, serde_json::Value::String(rendered));
    }

    #[test]
    fn rejects_bad_digests() {
        assert_eq!(
            "abc".parse::<Digest>().unwrap_err(),
            DigestParseError::MissingPrefix
        );
        assert!(matches!(
            "sha256:zz".parse::<Digest>().unwrap_err(),
            DigestParseError::InvalidLength { .. }
        ));
    }
}

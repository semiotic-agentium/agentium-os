//! Canonical content-addressable hashing for agent source bundles.
//!
//! This crate owns two things:
//!
//! 1. The `ContentHash` newtype — a validated SHA-256 hex-64 digest.
//! 2. The canonical hashing function that turns authored source content into
//!    a deterministic `ContentHash`.
//!
//! ## Canonical hash algorithm
//!
//! ```text
//! SHA-256(
//!   section("manifest", canonical_json(manifest.json))
//!   || for each .ts file sorted by path:
//!        section("ts", path || content)
//!   || for each .baml file sorted by path:
//!        section("baml", path || content)
//! )
//! ```
//!
//! Where `section(tag, data)` = `tag_len:u32le || tag || data_len:u64le || data`.
//!
//! Runtime-generated artefacts (d.ts, tsconfig, compiled JS, baml_client/)
//! are excluded. Two packages with identical authored source produce
//! identical hashes.

mod content_hash;
mod hasher;

pub use content_hash::{ContentHash, ContentHashParseError};
pub use hasher::{CanonicalHasher, HashInput, HashInputFile};

//! Canonical hashing algorithm for agent source bundles.
//!
//! The hasher consumes structured inputs in a deterministic order and produces
//! a `ContentHash`. The algorithm is designed so that:
//!
//! - File ordering is canonical (sorted by path).
//! - Each section is length-delimited to prevent ambiguity.
//! - The manifest is canonicalized (sorted keys, no trailing whitespace).
//! - Adding/removing a file always changes the hash.
//! - Reordering files does not change the hash (they're sorted).

use sha2::{Digest, Sha256};

use crate::content_hash::ContentHash;

/// A single source file to be included in the hash computation.
#[derive(Debug, Clone)]
pub struct HashInputFile<'a> {
    /// Relative path within the package (e.g. `src/index.ts`).
    pub path: &'a str,
    /// UTF-8 file content.
    pub content: &'a str,
}

/// The complete input to the canonical hash function.
///
/// This is a borrowed view — the caller retains ownership of the source data.
/// The hasher does not allocate for the source content itself.
#[derive(Debug, Clone)]
pub struct HashInput<'a> {
    /// The manifest.json content as a `serde_json::Value`. Will be
    /// canonicalized (sorted keys, compact format) before hashing.
    pub manifest: &'a serde_json::Value,
    /// TypeScript source files. Need not be sorted — the hasher sorts by path.
    pub ts_files: Vec<HashInputFile<'a>>,
    /// BAML prompt files. Need not be sorted — the hasher sorts by path.
    pub baml_files: Vec<HashInputFile<'a>>,
}

/// Computes the canonical `ContentHash` from structured source inputs.
///
/// ## Algorithm
///
/// ```text
/// SHA-256(
///   section("manifest", canonical_json(manifest))
///   || for each .ts file sorted by path:
///        section("ts", path || "\0" || content)
///   || for each .baml file sorted by path:
///        section("baml", path || "\0" || content)
/// )
/// ```
///
/// Where `section(tag, data)` feeds:
/// - `tag` length as 4-byte little-endian u32
/// - `tag` bytes
/// - `data` length as 8-byte little-endian u64
/// - `data` bytes
pub struct CanonicalHasher {
    state: Sha256,
}

impl CanonicalHasher {
    /// Create a new hasher.
    pub fn new() -> Self {
        Self {
            state: Sha256::new(),
        }
    }

    /// Compute the canonical hash from a complete input.
    pub fn hash(input: &HashInput<'_>) -> ContentHash {
        let mut h = Self::new();
        h.feed(input);
        h.finish()
    }

    /// Feed a complete input into the hasher.
    pub fn feed(&mut self, input: &HashInput<'_>) {
        // 1. Manifest — canonicalized JSON (sorted keys, compact)
        let canonical_manifest = canonical_json(input.manifest);
        self.feed_section("manifest", canonical_manifest.as_bytes());

        // 2. TypeScript files — sorted by path
        let mut ts_sorted: Vec<&HashInputFile<'_>> = input.ts_files.iter().collect();
        ts_sorted.sort_by_key(|f| f.path);
        for file in ts_sorted {
            let data = file_data(file);
            self.feed_section("ts", &data);
        }

        // 3. BAML files — sorted by path
        let mut baml_sorted: Vec<&HashInputFile<'_>> = input.baml_files.iter().collect();
        baml_sorted.sort_by_key(|f| f.path);
        for file in baml_sorted {
            let data = file_data(file);
            self.feed_section("baml", &data);
        }
    }

    /// Finalize and produce the `ContentHash`.
    pub fn finish(self) -> ContentHash {
        let digest = self.state.finalize();
        let hex = hex_encode(&digest);
        ContentHash::from_validated(hex)
    }

    /// Feed a single length-delimited section into the hash state.
    fn feed_section(&mut self, tag: &str, data: &[u8]) {
        // tag_len:u32le || tag || data_len:u64le || data
        let tag_bytes = tag.as_bytes();
        self.state.update((tag_bytes.len() as u32).to_le_bytes());
        self.state.update(tag_bytes);
        self.state.update((data.len() as u64).to_le_bytes());
        self.state.update(data);
    }
}

impl Default for CanonicalHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Produce canonical JSON: sorted keys, compact format, no trailing newline.
///
/// Uses `serde_json` serialization of a `Value` which already preserves
/// insertion order. We re-parse and re-serialize through a sorted structure
/// to ensure key ordering is deterministic.
fn canonical_json(value: &serde_json::Value) -> String {
    let sorted = sort_json_keys(value);
    serde_json::to_string(&sorted).expect("canonical JSON serialization should never fail")
}

/// Recursively sort all object keys in a JSON value.
fn sort_json_keys(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: Vec<(&String, serde_json::Value)> = map
                .iter()
                .map(|(k, v)| (k, sort_json_keys(v)))
                .collect();
            sorted.sort_by_key(|(k, _)| *k);
            let new_map: serde_json::Map<String, serde_json::Value> = sorted
                .into_iter()
                .map(|(k, v)| (k.clone(), v))
                .collect();
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sort_json_keys).collect())
        }
        other => other.clone(),
    }
}

/// Encode bytes as lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Build the data blob for a single file: `path || '\0' || content`.
fn file_data(file: &HashInputFile<'_>) -> Vec<u8> {
    let mut data = Vec::with_capacity(file.path.len() + 1 + file.content.len());
    data.extend_from_slice(file.path.as_bytes());
    data.push(0); // null separator
    data.extend_from_slice(file.content.as_bytes());
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> serde_json::Value {
        serde_json::json!({
            "name": "test-agent",
            "version": "1.0.0",
            "tools": ["calculator"],
            "discovery": {
                "description": "A test agent",
                "capabilities": ["math"]
            }
        })
    }

    #[test]
    fn deterministic_hash() {
        let manifest = sample_manifest();
        let input = HashInput {
            manifest: &manifest,
            ts_files: vec![
                HashInputFile {
                    path: "src/index.ts",
                    content: "export function run() {}",
                },
                HashInputFile {
                    path: "src/helper.ts",
                    content: "export function help() {}",
                },
            ],
            baml_files: vec![HashInputFile {
                path: "baml_src/main.baml",
                content: "function Greet { ... }",
            }],
        };

        let hash1 = CanonicalHasher::hash(&input);
        let hash2 = CanonicalHasher::hash(&input);
        assert_eq!(hash1, hash2, "same input must produce same hash");
    }

    #[test]
    fn file_order_irrelevant() {
        let manifest = sample_manifest();

        let input_a = HashInput {
            manifest: &manifest,
            ts_files: vec![
                HashInputFile {
                    path: "src/a.ts",
                    content: "a",
                },
                HashInputFile {
                    path: "src/b.ts",
                    content: "b",
                },
            ],
            baml_files: vec![],
        };

        let input_b = HashInput {
            manifest: &manifest,
            ts_files: vec![
                HashInputFile {
                    path: "src/b.ts",
                    content: "b",
                },
                HashInputFile {
                    path: "src/a.ts",
                    content: "a",
                },
            ],
            baml_files: vec![],
        };

        assert_eq!(
            CanonicalHasher::hash(&input_a),
            CanonicalHasher::hash(&input_b),
            "file ordering should not affect hash"
        );
    }

    #[test]
    fn manifest_key_order_irrelevant() {
        let manifest_a = serde_json::json!({"name": "x", "version": "1"});
        let manifest_b = serde_json::json!({"version": "1", "name": "x"});

        let input_a = HashInput {
            manifest: &manifest_a,
            ts_files: vec![],
            baml_files: vec![],
        };
        let input_b = HashInput {
            manifest: &manifest_b,
            ts_files: vec![],
            baml_files: vec![],
        };

        assert_eq!(
            CanonicalHasher::hash(&input_a),
            CanonicalHasher::hash(&input_b),
            "JSON key order should not affect hash"
        );
    }

    #[test]
    fn different_content_different_hash() {
        let manifest = sample_manifest();

        let input_a = HashInput {
            manifest: &manifest,
            ts_files: vec![HashInputFile {
                path: "src/index.ts",
                content: "version_a",
            }],
            baml_files: vec![],
        };
        let input_b = HashInput {
            manifest: &manifest,
            ts_files: vec![HashInputFile {
                path: "src/index.ts",
                content: "version_b",
            }],
            baml_files: vec![],
        };

        assert_ne!(
            CanonicalHasher::hash(&input_a),
            CanonicalHasher::hash(&input_b),
            "different content must produce different hash"
        );
    }

    #[test]
    fn adding_file_changes_hash() {
        let manifest = sample_manifest();

        let input_a = HashInput {
            manifest: &manifest,
            ts_files: vec![HashInputFile {
                path: "src/index.ts",
                content: "code",
            }],
            baml_files: vec![],
        };
        let input_b = HashInput {
            manifest: &manifest,
            ts_files: vec![
                HashInputFile {
                    path: "src/index.ts",
                    content: "code",
                },
                HashInputFile {
                    path: "src/extra.ts",
                    content: "",
                },
            ],
            baml_files: vec![],
        };

        assert_ne!(
            CanonicalHasher::hash(&input_a),
            CanonicalHasher::hash(&input_b),
            "adding a file must change the hash"
        );
    }

    #[test]
    fn empty_bundle_has_stable_hash() {
        let manifest = serde_json::json!({});
        let input = HashInput {
            manifest: &manifest,
            ts_files: vec![],
            baml_files: vec![],
        };
        let hash = CanonicalHasher::hash(&input);
        // Smoke test: should be a valid 64-char hex string
        assert_eq!(hash.as_str().len(), 64);
        // Pin the empty hash for regression detection
        insta::assert_snapshot!(hash.as_str());
    }

    #[test]
    fn ts_vs_baml_section_distinct() {
        // A .ts file and a .baml file with identical path+content should
        // produce different hashes because the section tag differs.
        let manifest = serde_json::json!({});
        let file = HashInputFile {
            path: "src/main",
            content: "same",
        };

        let input_ts = HashInput {
            manifest: &manifest,
            ts_files: vec![file.clone()],
            baml_files: vec![],
        };
        let input_baml = HashInput {
            manifest: &manifest,
            ts_files: vec![],
            baml_files: vec![file],
        };

        assert_ne!(
            CanonicalHasher::hash(&input_ts),
            CanonicalHasher::hash(&input_baml),
            "ts and baml sections must produce different hashes"
        );
    }
}

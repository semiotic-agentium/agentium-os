//! Row shape for `provenance_payload` (and archive hydration).

use serde::{Deserialize, Serialize};

/// How full JSON is stored: inline column, blob table, or future file bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StorageKind {
    #[default]
    Inline,
    Blob,
    /// Reserved for embedded Surreal file buckets.
    File,
}

impl StorageKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StorageKind::Inline => "inline",
            StorageKind::Blob => "blob",
            StorageKind::File => "file",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PayloadRecord {
    pub payload_id: String,
    pub activity_anchor_id: String,
    pub activity_id: Option<String>,
    pub payload_kind: String,
    /// Inline JSON text; empty when `content_hash` points at `provenance_payload_blob`.
    pub payload_json: String,
    /// Lowercase hex SHA-256 of canonical UTF-8 body when offloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub storage_kind: StorageKind,
    /// Logical object path (e.g. `tool_archives/{hash}`); hydration still uses `provenance_payload_blob` until buckets exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_key: Option<String>,
    /// BM25-indexed material (snippet / prefix) for `@@` search.
    #[serde(default)]
    pub search_text: String,
}

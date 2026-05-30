// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Canonical JSON hashing and payload offload helpers for provenance_payload / provenance_payload_blob.
//!
//! **Embedded SurrealDB:** the `surrealdb` Rust crate (v3, `kv-mem` / `kv-surrealkv`) does not expose a
//! stable client API for Surreal 3 file buckets on embedded engines. Large bodies therefore use the
//! `provenance_payload_blob` table keyed by `content_hash`, not object storage.
//!
//! Rows record `file_key` / `storage_kind` so a later embedded file API can use `storage_kind = "file"` and the
//! same logical `tool_archives/{hash}` key without changing `payload_id` or `archive_ref`.

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// UTF-8 byte length above which `tool_result` / `llm_result` bodies are stored in `provenance_payload_blob`.
pub const PAYLOAD_OFFLOAD_THRESHOLD_BYTES: usize = 16 * 1024;

/// Maximum characters indexed for BM25 (`search_text` column).
pub const PAYLOAD_SEARCH_TEXT_MAX_CHARS: usize = 32 * 1024;

/// Split one `add_event` into two transactions (graph then payloads) if limits are exceeded.
pub const MAX_TXN_BIND_COUNT: usize = 4096;
/// Approximate guard for parser / wire size (UTF-8 byte length of statement text inside `BEGIN…COMMIT`).
pub const MAX_TXN_SQL_BYTES: usize = 512 * 1024;

#[inline]
pub fn txn_should_split(bind_count: usize, sql_body_bytes: usize) -> bool {
    bind_count > MAX_TXN_BIND_COUNT || sql_body_bytes > MAX_TXN_SQL_BYTES
}

/// Logical object key for a future Surreal `tool_archives` bucket; today the bytes live in `provenance_payload_blob`.
#[inline]
pub fn logical_file_key_for_tool_archive(content_hash: &str) -> String {
    format!("tool_archives/{content_hash}")
}

/// Recursively sort object keys so equivalent JSON maps hash identically.
pub fn canonical_json_string(value: &Value) -> Result<String, serde_json::Error> {
    let v = canonicalize_value(value);
    serde_json::to_string(&v)
}

fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize_value(v)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
        _ => value.clone(),
    }
}

pub fn sha256_hex_utf8(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Truncate for FTS / grep — keeps a prefix of the searchable material.
pub fn search_text_snippet(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= PAYLOAD_SEARCH_TEXT_MAX_CHARS {
        return t.to_string();
    }
    t.chars().take(PAYLOAD_SEARCH_TEXT_MAX_CHARS).collect()
}

/// Decide offload: large `tool_result` / `llm_result` JSON bodies go to the blob table.
pub fn should_offload_payload(kind: &str, body_utf8_len: usize) -> bool {
    matches!(kind, "tool_result" | "llm_result") && body_utf8_len > PAYLOAD_OFFLOAD_THRESHOLD_BYTES
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared SurrealDB table names and payload SQL fragments (single source of truth).

pub(crate) const TBL_NODE: &str = "prov_node";
pub(crate) const TBL_EDGE: &str = "prov_edge";
pub(crate) const TBL_PAYLOAD: &str = "provenance_payload";
pub(crate) const TBL_PAYLOAD_BLOB: &str = "provenance_payload_blob";
/// Alias for SQL fragments that historically used `TBL_BLOB`.
pub(crate) const TBL_BLOB: &str = TBL_PAYLOAD_BLOB;
pub(crate) const TBL_ARCHIVE_PREFIX_REGISTRY: &str = "archive_prefix_registry";
pub(crate) const TBL_ARCHIVE_LOCAL_COUNTER: &str = "archive_local_counter";
pub(crate) const TBL_ARCHIVE_BODY: &str = "archive_body";

/// Column list for `PayloadRecord` round-trip SELECTs (must match serde fields).
pub(crate) const PAYLOAD_ROW_SELECT: &str = "payload_id, activity_anchor_id, activity_id, payload_kind, payload_json, content_hash, storage_kind, file_key, search_text";

/// FTS predicate for activity-scoped payload search; bind name `query_text`.
pub(crate) const FTS_PAYLOAD_ACTIVITY_WHERE: &str =
    "search_text @@ $query_text AND activity_id IS NOT NONE";

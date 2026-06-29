// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! SurrealDB DDL for the provenance store (`DEFINE INDEX`, FTS analyzer).

use surrealdb::{Surreal, engine::any::Any};

use super::helpers::map_surreal_error;
use crate::{
    error::Result,
    surreal_tables::{
        TBL_AGENT_PACKAGE_INSTANCE, TBL_ARCHIVE_BODY, TBL_ARCHIVE_LOCAL_COUNTER,
        TBL_ARCHIVE_PREFIX_REGISTRY, TBL_CONTEXT_COMPACTION_INDEX, TBL_CONTEXT_PICKER_INDEX,
        TBL_CONTEXT_PLANNING_INDEX, TBL_CONTEXT_TRANSCRIPT_INDEX, TBL_EDGE,
        TBL_HISTORY_REF_REGISTRY, TBL_NODE, TBL_PAYLOAD, TBL_PAYLOAD_BLOB, TBL_SESSION_REF_COUNTER,
    },
};

pub(super) const NS: &str = "provenance";
pub(super) const DB: &str = "store";

pub(super) async fn init_schema(db: &Surreal<Any>) -> Result<()> {
    // Define indexes for efficient queries.
    // prov_node: unique by node_id, indexed by label and common property lookups.
    let schema_queries = [
        // Node table: unique node_id, indexed by label
        format!("DEFINE INDEX IF NOT EXISTS idx_node_id ON {TBL_NODE} FIELDS node_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_node_label ON {TBL_NODE} FIELDS label"),
        // Activity anchor index (used by payload joins)
        format!("DEFINE INDEX IF NOT EXISTS idx_node_activity_anchor ON {TBL_NODE} FIELDS props.a2a_activity_anchor"),
        // Paginated metamodel reads: label filter + chronological sort keys.
        format!("DEFINE INDEX IF NOT EXISTS idx_node_label_event_order ON {TBL_NODE} FIELDS label, props.a2a_event_order"),
        format!("DEFINE INDEX IF NOT EXISTS idx_node_label_prov_time ON {TBL_NODE} FIELDS label, props.prov_time"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_node_label_activity_anchor ON {TBL_NODE} FIELDS label, props.a2a_activity_anchor"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_node_label_agent_type ON {TBL_NODE} FIELDS label, props.a2a_agent_type"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_node_label_plan_step ON {TBL_NODE} FIELDS label, props.a2a_plan_id, props.a2a_step_id"
        ),
        // Edge table: composite + selective compound indexes (avoid redundant single-column idx).
        format!("REMOVE INDEX IF EXISTS idx_edge_from ON {TBL_EDGE}"),
        format!("REMOVE INDEX IF EXISTS idx_edge_to ON {TBL_EDGE}"),
        format!("REMOVE INDEX IF EXISTS idx_edge_rel ON {TBL_EDGE}"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_composite ON {TBL_EDGE} FIELDS from_id, rel_type, to_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_to_rel ON {TBL_EDGE} FIELDS to_id, rel_type"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_to_rel_from_label ON {TBL_EDGE} FIELDS to_id, rel_type, from_label"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_from_label_rel ON {TBL_EDGE} FIELDS from_label, rel_type"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_edge_from_rel_to_label ON {TBL_EDGE} FIELDS from_id, rel_type, to_label"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_edge_rel_from_to_label ON {TBL_EDGE} FIELDS rel_type, from_label, to_label"
        ),
        // Head-pointer edges (`WAS_LAST_TRANSITIONED_TO`,
        // `WAS_LAST_EXECUTED_BY`) carry a cardinality-one invariant per
        // `(rel_type, from_id)`: a Task has exactly one current TaskState
        // and exactly one current AgentRuntimeInstance. SurrealDB v3 does
        // not support partial / WHERE-filtered UNIQUE indexes, so an
        // unconditional `(rel_type, from_id) UNIQUE` would break the
        // existing fan-out edges (e.g. `A2A_TASK_MESSAGE`,
        // `A2A_TASK_ARTIFACT`) that legitimately share a `from_id`. The
        // invariant is therefore enforced procedurally by
        // `surreal_write_batch::push_head_pointer_repoint`, which emits
        // `DELETE prov_edge WHERE from_id = ? AND rel_type = ?` followed
        // by an `UPSERT` for the new head, both inside the same
        // `BEGIN..COMMIT` transaction as the rest of the event's writes.
        // Payload table: unique payload_id, indexed by activity_anchor_id and activity_id
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_id ON {TBL_PAYLOAD} FIELDS payload_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_activity_anchor ON {TBL_PAYLOAD} FIELDS activity_anchor_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_activity ON {TBL_PAYLOAD} FIELDS activity_id, payload_kind"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_blob_hash ON {TBL_PAYLOAD_BLOB} FIELDS content_hash UNIQUE"),
        // Task storage is graph-backed. This schema initializes only the
        // provenance node/edge/payload tables required by
        // `TaskGraphReader` and live task-update delivery.
        // Cluster-safe session archive refs: stable prefix per (context, agent), monotonic local per prefix.
        format!("DEFINE TABLE IF NOT EXISTS {TBL_ARCHIVE_PREFIX_REGISTRY}"),
        format!("DEFINE INDEX IF NOT EXISTS idx_archive_prefix_ctx_agent ON {TBL_ARCHIVE_PREFIX_REGISTRY} FIELDS context_id, agent_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_archive_prefix_ctx_pfx ON {TBL_ARCHIVE_PREFIX_REGISTRY} FIELDS context_id, archive_prefix UNIQUE"),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_ARCHIVE_LOCAL_COUNTER}"),
        format!("DEFINE INDEX IF NOT EXISTS idx_archive_local_ctx_pfx ON {TBL_ARCHIVE_LOCAL_COUNTER} FIELDS context_id, archive_prefix UNIQUE"),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_ARCHIVE_BODY}"),
        format!("DEFINE INDEX IF NOT EXISTS idx_archive_body_lookup ON {TBL_ARCHIVE_BODY} FIELDS context_id, archive_prefix, archive_local UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_archive_body_anchor ON {TBL_ARCHIVE_BODY} FIELDS context_id, activity_anchor UNIQUE"),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_HISTORY_REF_REGISTRY}"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_history_ref_ctx_anchor_source ON {TBL_HISTORY_REF_REGISTRY} FIELDS context_id, activity_anchor, source UNIQUE"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_history_ref_ctx_n ON {TBL_HISTORY_REF_REGISTRY} FIELDS context_id, history_n"
        ),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_SESSION_REF_COUNTER}"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_session_ref_counter_ctx ON {TBL_SESSION_REF_COUNTER} FIELDS context_id UNIQUE"
        ),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_AGENT_PACKAGE_INSTANCE}"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_agent_pkg_instance_node ON {TBL_AGENT_PACKAGE_INSTANCE} FIELDS instance_node_id UNIQUE"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_agent_pkg_instance_package ON {TBL_AGENT_PACKAGE_INSTANCE} FIELDS agent_package"
        ),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_CONTEXT_PICKER_INDEX}"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_context_picker_ctx ON {TBL_CONTEXT_PICKER_INDEX} FIELDS context_id UNIQUE"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_context_picker_latest ON {TBL_CONTEXT_PICKER_INDEX} FIELDS latest_timestamp_ms"
        ),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_CONTEXT_TRANSCRIPT_INDEX}"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_transcript_ctx_order ON {TBL_CONTEXT_TRANSCRIPT_INDEX} FIELDS context_id, event_order"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_transcript_ctx_node ON {TBL_CONTEXT_TRANSCRIPT_INDEX} FIELDS context_id, node_id UNIQUE"
        ),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_CONTEXT_PLANNING_INDEX}"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_context_planning_ctx_task ON {TBL_CONTEXT_PLANNING_INDEX} FIELDS context_id, task_id UNIQUE"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_context_planning_ctx_order ON {TBL_CONTEXT_PLANNING_INDEX} FIELDS context_id, latest_planning_event_order"
        ),
        format!("DEFINE TABLE IF NOT EXISTS {TBL_CONTEXT_COMPACTION_INDEX}"),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_context_compaction_ctx_task ON {TBL_CONTEXT_COMPACTION_INDEX} FIELDS context_id, task_entity_id UNIQUE"
        ),
        format!(
            "DEFINE INDEX IF NOT EXISTS idx_context_compaction_ctx_order ON {TBL_CONTEXT_COMPACTION_INDEX} FIELDS context_id, event_order"
        ),
        // Full-text search on denormalized search_text (blob-backed bodies are not indexed in-table)
        "DEFINE ANALYZER IF NOT EXISTS payload_analyzer TOKENIZERS blank, class FILTERS snowball(english)".to_string(),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_search_fts ON {TBL_PAYLOAD} FIELDS search_text FULLTEXT ANALYZER payload_analyzer BM25"),
    ];
    let batch = schema_queries.join("; ");
    db.query(batch)
        .await
        .map_err(map_surreal_error)?
        .check()
        .map_err(map_surreal_error)?;
    Ok(())
}

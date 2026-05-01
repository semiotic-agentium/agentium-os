//! SurrealDB DDL for the provenance store (`DEFINE INDEX`, FTS analyzer).

use surrealdb::{Surreal, engine::any::Any};

use super::helpers::map_surreal_error;
use crate::{
    error::Result,
    surreal_tables::{
        TBL_A2A_MESSAGE, TBL_A2A_TASK, TBL_A2A_UPDATE, TBL_EDGE, TBL_NODE, TBL_PAYLOAD,
        TBL_PAYLOAD_BLOB,
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
        // Edge table: composite + selective compound indexes (avoid redundant single-column idx).
        format!("REMOVE INDEX IF EXISTS idx_edge_from ON {TBL_EDGE}"),
        format!("REMOVE INDEX IF EXISTS idx_edge_to ON {TBL_EDGE}"),
        format!("REMOVE INDEX IF EXISTS idx_edge_rel ON {TBL_EDGE}"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_composite ON {TBL_EDGE} FIELDS from_id, rel_type, to_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_to_rel ON {TBL_EDGE} FIELDS to_id, rel_type"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_to_rel_from_label ON {TBL_EDGE} FIELDS to_id, rel_type, from_label"),
        format!("DEFINE INDEX IF NOT EXISTS idx_edge_from_label_rel ON {TBL_EDGE} FIELDS from_label, rel_type"),
        // Payload table: unique payload_id, indexed by activity_anchor_id and activity_id
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_id ON {TBL_PAYLOAD} FIELDS payload_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_activity_anchor ON {TBL_PAYLOAD} FIELDS activity_anchor_id"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_activity ON {TBL_PAYLOAD} FIELDS activity_id, payload_kind"),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_blob_hash ON {TBL_PAYLOAD_BLOB} FIELDS content_hash UNIQUE"),
        // A2A task table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_task_id ON {TBL_A2A_TASK} FIELDS task_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_task_ctx ON {TBL_A2A_TASK} FIELDS context_id"),
        // A2A message table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_msg_id ON {TBL_A2A_MESSAGE} FIELDS msg_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_msg_task ON {TBL_A2A_MESSAGE} FIELDS task_id, seq"),
        // A2A update table
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_upd_id ON {TBL_A2A_UPDATE} FIELDS update_id UNIQUE"),
        format!("DEFINE INDEX IF NOT EXISTS idx_a2a_upd_task ON {TBL_A2A_UPDATE} FIELDS task_id, seq"),
        // Full-text search on denormalized search_text (blob-backed bodies are not indexed in-table)
        "DEFINE ANALYZER IF NOT EXISTS payload_analyzer TOKENIZERS blank, class FILTERS snowball(english)".to_string(),
        format!("DEFINE INDEX IF NOT EXISTS idx_payload_search_fts ON {TBL_PAYLOAD} FIELDS search_text FULLTEXT ANALYZER payload_analyzer BM25"),
    ];
    let batch = schema_queries.join("; ");
    db.query(batch).await.map_err(map_surreal_error)?;
    Ok(())
}

//! [`ProvenanceWriter`] implementation — normalized event write path.

use async_trait::async_trait;
use serde_json::Value;

use super::{SurrealProvenanceStore, payload::payload_records_from_event};
use crate::{
    error::{ProvenanceError, Result},
    normalizer::{NormalizeContext, validate_event},
    payload_record::StorageKind,
    payload_storage,
    store::ProvenanceWriter,
    surreal_write_batch::call_activity_id_from_normalized,
};

#[async_trait]
impl ProvenanceWriter for SurrealProvenanceStore {
    async fn add_event(&self, event: crate::events::ProvEvent) -> Result<()> {
        validate_event(&event)?;
        self.enforce_step_completion_gate(&event).await?;
        let mut payload_records = payload_records_from_event(&event)?;
        let context = match event.task_id() {
            Some(tid) => {
                let task_agent_id = self.get_task_agent_id(tid).await?;
                NormalizeContext { task_agent_id }
            }
            None => NormalizeContext::default(),
        };
        let normalized = self
            .normalizer
            .normalize_with_context(&event, Some(&context))?;
        let context_id_opt = event.context_id_opt().map(|c| c.as_str().to_string());
        let anchor = event.id().as_str().to_string();
        let activity_id = call_activity_id_from_normalized(&normalized, &anchor);

        let mut blob_bodies: Vec<(String, String)> = Vec::new();
        let mut inline_payload_bytes: usize = 0;
        for p in &mut payload_records {
            if let Some(ref a) = activity_id {
                p.activity_id = Some(a.clone());
            }
            if payload_storage::should_offload_payload(&p.payload_kind, p.payload_json.len()) {
                let v: Value = serde_json::from_str(&p.payload_json).map_err(|e| {
                    ProvenanceError::InvalidEvent {
                        activity_anchor: anchor.clone(),
                        reason: format!("payload json for offload: {e}"),
                    }
                })?;
                let canon = payload_storage::canonical_json_string(&v).map_err(|e| {
                    ProvenanceError::InvalidEvent {
                        activity_anchor: anchor.clone(),
                        reason: format!("canonical json for offload: {e}"),
                    }
                })?;
                let hash = payload_storage::sha256_hex_utf8(&canon);
                p.search_text = payload_storage::search_text_snippet(&canon);
                p.content_hash = Some(hash.clone());
                p.storage_kind = StorageKind::Blob;
                p.file_key = Some(payload_storage::logical_file_key_for_tool_archive(&hash));
                p.payload_json.clear();
                blob_bodies.push((hash, canon));
            } else {
                inline_payload_bytes = inline_payload_bytes.saturating_add(p.payload_json.len());
                p.search_text = payload_storage::search_text_snippet(&p.payload_json);
            }
        }

        let plans = crate::surreal_write_batch::build_event_write_plans(
            &normalized,
            context_id_opt.as_deref(),
            &payload_records,
            &blob_bodies,
        );
        let total_stmts: usize = plans.iter().map(|p| p.statement_count).sum();
        let total_binds: usize = plans.iter().map(|p| p.binds.len()).sum();
        tracing::debug!(
            target: "baml_rt_provenance::surreal",
            anchor = %anchor,
            txn_parts = plans.len(),
            statements = total_stmts,
            bind_count = total_binds,
            payload_rows = payload_records.len(),
            blob_rows = blob_bodies.len(),
            inline_payload_bytes,
            "provenance add_event write txn"
        );
        for plan in plans {
            self.run_event_write_plan(plan).await?;
        }

        if let (Some(cache), Some(ctx)) = (&self.mermaid_cache, context_id_opt.as_deref()) {
            cache.invalidate(ctx);
        }
        Ok(())
    }
}

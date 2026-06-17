// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Post-turn compaction hook on [`EffectEvent::A2aCompleted`].

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    bus::{EffectEvent, EffectSubscriber, EffectSubscriberTier},
    ids::ContextId,
};
use baml_rt_tools::prompt_projection::project_prompt_context;
use sha2::{Digest, Sha256};

use super::{
    compactor::ContextCompactionService,
    range::select_compactable_range,
    types::{ContextCompactionPolicy, ContextCompactionRecord, ContextCompactionTrigger},
};
use crate::{
    store::{ProvenanceContextReader, ProvenanceWriter},
    surreal_store::SurrealProvenanceStore,
};

/// Background subscriber: after each A2A turn, maybe compact sealed transcript prefix.
pub struct ContextCompactionSubscriber {
    store: Arc<SurrealProvenanceStore>,
    writer: Arc<dyn ProvenanceWriter>,
    policy: ContextCompactionPolicy,
}

impl ContextCompactionSubscriber {
    pub fn new(
        store: Arc<SurrealProvenanceStore>,
        writer: Arc<dyn ProvenanceWriter>,
        policy: ContextCompactionPolicy,
    ) -> Self {
        Self {
            store,
            writer,
            policy,
        }
    }

    pub async fn try_compact(&self, context_id: &ContextId, trigger: ContextCompactionTrigger) {
        let start = std::time::Instant::now();
        let outcome = match self.try_compact_inner(context_id, trigger).await {
            Ok(CompactionAttempt::Succeeded {
                pre_prompt_bytes,
                post_prompt_bytes,
                covered_rows,
            }) => {
                baml_rt_observability::metrics::record_context_compaction(
                    trigger.as_wire_str(),
                    "success",
                    start.elapsed(),
                    pre_prompt_bytes,
                    post_prompt_bytes,
                    covered_rows,
                );
                return;
            }
            Ok(CompactionAttempt::Skipped) => "skipped",
            Ok(CompactionAttempt::RejectedValidation) => "rejected_validation",
            Err(_) => "error",
        };
        baml_rt_observability::metrics::record_context_compaction(
            trigger.as_wire_str(),
            outcome,
            start.elapsed(),
            0,
            0,
            0,
        );
    }

    async fn try_compact_inner(
        &self,
        context_id: &ContextId,
        trigger: ContextCompactionTrigger,
    ) -> Result<CompactionAttempt, ()> {
        let Ok(index_rows) = self
            .store
            .fetch_transcript_index_rows_for_context(context_id.as_str(), None)
            .await
        else {
            return Ok(CompactionAttempt::Skipped);
        };
        let Ok(items) = self.store.conversation_context(context_id, None).await else {
            return Ok(CompactionAttempt::Skipped);
        };
        let Some(range) = select_compactable_range(&index_rows, &items, &self.policy, false, false)
        else {
            return Ok(CompactionAttempt::Skipped);
        };

        let pre_row_count = items.len() as u64;
        let prefix_items: Vec<_> = items
            .iter()
            .filter(|i| i.timestamp_ms <= range.covered_event_order_end)
            .cloned()
            .collect();
        let tail_count = pre_row_count.saturating_sub(prefix_items.len() as u64);
        let projection_items: Vec<_> = prefix_items
            .iter()
            .filter_map(|item| {
                baml_rt_conversation::provenance_item_to_projection_item(item.clone())
            })
            .collect();
        if projection_items.is_empty() {
            return Ok(CompactionAttempt::Skipped);
        }
        let ref_table = std::sync::Arc::new(baml_rt_tools::archive_refs::RefTable::new());
        let history = project_prompt_context(
            projection_items,
            &baml_rt_tools::tools::ToolRegistry::new(),
            &ref_table,
            None,
        );
        let summary = ContextCompactionService::summarize_from_history_json(&history);
        let rendered = baml_rt_tools::prompt_projection::format_conversation_history_transcript(
            history.as_array().unwrap_or(&vec![]),
        );
        let pre_prompt_bytes = rendered.len() as u64;
        let emergency_threshold_met =
            if matches!(trigger, ContextCompactionTrigger::PreModelEmergency) {
                let full_projection: Vec<_> = items
                    .iter()
                    .filter_map(|item| {
                        baml_rt_conversation::provenance_item_to_projection_item(item.clone())
                    })
                    .collect();
                let full_history = project_prompt_context(
                    full_projection,
                    &baml_rt_tools::tools::ToolRegistry::new(),
                    &ref_table,
                    None,
                );
                let full_rendered =
                    baml_rt_tools::prompt_projection::format_conversation_history_transcript(
                        full_history.as_array().unwrap_or(&vec![]),
                    );
                full_rendered.len() as u64 >= self.policy.prompt_bytes_threshold
            } else {
                false
            };

        let threshold_met = match trigger {
            ContextCompactionTrigger::PostTurnThreshold => {
                pre_row_count as usize >= self.policy.item_threshold
            }
            ContextCompactionTrigger::PreModelEmergency => emergency_threshold_met,
            ContextCompactionTrigger::ManualOperator => true,
        };
        if !threshold_met {
            return Ok(CompactionAttempt::Skipped);
        }

        let post_prompt_bytes = summary.len() as u64;
        let mut hasher = Sha256::new();
        hasher.update(rendered.as_bytes());
        let source_render_hash = format!("{:x}", hasher.finalize());

        let record = ContextCompactionRecord {
            context_id: context_id.clone(),
            task_id: None,
            covered_event_order_start: range.covered_event_order_start,
            covered_event_order_end: range.covered_event_order_end,
            covered_node_ids: range.covered_node_ids,
            summary_text: summary.clone(),
            trigger,
            recent_tail_retention: self.policy.recent_tail_retention,
            pre_row_count,
            post_row_count: tail_count + 1,
            pre_prompt_bytes,
            post_prompt_bytes,
            source_render_hash,
            excluded_unresolved: false,
        };
        let event = match ContextCompactionService::build_record(record, summary, &rendered) {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!(
                    context_id = %context_id,
                    error = %err,
                    "compaction summary failed validation"
                );
                return Ok(CompactionAttempt::RejectedValidation);
            }
        };
        self.writer.add_event(event).await.map_err(|err| {
            tracing::warn!(
                context_id = %context_id,
                error = %err,
                "failed to record compaction provenance event"
            );
        })?;
        Ok(CompactionAttempt::Succeeded {
            pre_prompt_bytes,
            post_prompt_bytes,
            covered_rows: prefix_items.len() as u64,
        })
    }
}

enum CompactionAttempt {
    Skipped,
    RejectedValidation,
    Succeeded {
        pre_prompt_bytes: u64,
        post_prompt_bytes: u64,
        covered_rows: u64,
    },
}

#[async_trait]
impl EffectSubscriber for ContextCompactionSubscriber {
    fn name(&self) -> &'static str {
        "context_compaction"
    }

    fn tier(&self) -> EffectSubscriberTier {
        EffectSubscriberTier::Background
    }

    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        let EffectEvent::A2aCompleted { context_id, .. } = event else {
            return Ok(());
        };
        self.try_compact(context_id, ContextCompactionTrigger::PostTurnThreshold)
            .await;
        Ok(())
    }
}

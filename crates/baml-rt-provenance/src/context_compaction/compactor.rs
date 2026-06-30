// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Compaction summary formatting, ref validation, and provenance event assembly.

use baml_rt_tools::{archive_refs::RefTable, citations::unresolved_wire_citations};

use super::types::ContextCompactionRecord;
use crate::{
    error::{ProvenanceError, Result},
    events::ProvEvent,
};

/// Wrap LLM prose in the compaction summary envelope.
#[must_use]
fn format_compaction_summary(prose_body: &str) -> String {
    format!("[conversation summary]\n{}", prose_body.trim())
}

/// Format envelope and validate cited refs. Called by all summarizer backends.
pub fn finalize_compaction_summary(prose: &str, ref_table: &RefTable) -> Result<String> {
    let summary = format_compaction_summary(prose);
    let unresolved = unresolved_wire_citations(&summary, ref_table);
    if unresolved.is_empty() {
        return Ok(summary);
    }
    Err(ProvenanceError::InvalidEvent {
        activity_anchor: "compaction-summary".to_string(),
        reason: format!(
            "compaction summary cites unresolved wire refs: {}",
            unresolved.join(", ")
        ),
    })
}

/// Assemble the compaction provenance event from a prepared record and final summary text.
pub fn build_compaction_record(
    mut record: ContextCompactionRecord,
    summary_text: String,
) -> ProvEvent {
    record.summary_text = summary_text.clone();
    record.post_prompt_bytes = summary_text.len() as u64;
    ProvEvent::context_compaction_recorded_global(
        record.context_id,
        record.task_id,
        record.covered_event_order_start,
        record.covered_event_order_end,
        record.covered_node_ids,
        summary_text,
        record.trigger,
        record.recent_tail_retention,
        record.pre_row_count,
        record.post_row_count,
        record.pre_prompt_bytes,
        record.post_prompt_bytes,
        record.source_render_hash,
        record.excluded_unresolved,
    )
}

#[cfg(test)]
mod tests {
    use baml_rt_tools::archive_refs::{ArchiveEntry, HistoryEntry, RefTable};

    use super::*;

    #[test]
    fn finalize_allows_prose_without_citations() {
        let table = RefTable::new();
        let summary =
            finalize_compaction_summary("paraphrased without handles", &table).expect("valid");
        assert!(summary.starts_with("[conversation summary]\n"));
        assert!(!summary.contains("Preserved refs:"));
    }

    #[test]
    fn finalize_resolves_archive_history_and_composite_refs() {
        let table = RefTable::new();
        table.insert_virtual_archive(
            1,
            ArchiveEntry::new(
                baml_rt_tools::archive_read::render_to_lines(&serde_json::json!({"x": 1})),
                "tool/ns".into(),
                None,
                "evt-archive".into(),
                "tool_result".into(),
            ),
        );
        table.insert_virtual_archive_ref(
            baml_rt_tools::archive_read::ShortRef::new_prefixed(2, 5),
            ArchiveEntry::new(
                baml_rt_tools::archive_read::render_to_lines(&serde_json::json!({"y": 2})),
                "tool/ns".into(),
                None,
                "evt-composite".into(),
                "tool_result".into(),
            ),
        );
        table.insert_virtual_history(
            2,
            HistoryEntry::new("evt-history".into(), "message".into()),
            "hello",
        );
        finalize_compaction_summary("see @1, @2/5, and #2", &table).expect("all refs resolve");
    }

    #[test]
    fn finalize_rejects_unresolved_handles() {
        let table = RefTable::new();
        let err =
            finalize_compaction_summary("stale @7 reference", &table).expect_err("unresolved");
        assert!(
            err.to_string().contains("@7"),
            "error should name unresolved ref: {err}"
        );
    }

    #[test]
    fn finalize_allows_omitting_source_refs() {
        let table = RefTable::new();
        let summary = finalize_compaction_summary("no refs", &table).expect("valid summary");
        assert!(summary.contains("no refs"));
    }
}

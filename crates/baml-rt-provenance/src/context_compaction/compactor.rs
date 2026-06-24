// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Compaction summary merge, validation, and provenance event assembly.

use std::collections::HashSet;

use super::types::ContextCompactionRecord;
use crate::{
    error::{ProvenanceError, Result},
    events::ProvEvent,
};

/// Extract `#N` and `@N` refs from text (base handles only; line suffixes ignored).
#[must_use]
pub fn extract_wire_refs(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'#' || bytes[i] == b'@' {
            let kind = bytes[i];
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i > start {
                let n = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
                out.insert(format!("{}{}", kind as char, n));
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Merge LLM (or fixed) prose with a deterministic ref appendix from the source transcript.
#[must_use]
pub fn merge_compaction_summary(prose_body: &str, source_rendered: &str) -> String {
    let mut refs: Vec<String> = extract_wire_refs(source_rendered).into_iter().collect();
    refs.sort();
    let body = prose_body.trim();
    let ref_clause = if refs.is_empty() {
        String::new()
    } else {
        format!("\nPreserved refs: {}", refs.join(", "))
    };
    format!("[conversation summary]\n{body}{ref_clause}")
}

/// Reject summaries that drop any `#N` or `@N` handle present in the source transcript.
pub fn validate_summary_preserves_wire_refs(summary: &str, source: &str) -> Result<()> {
    let source_refs = extract_wire_refs(source);
    if source_refs.is_empty() {
        return Ok(());
    }
    let summary_refs = extract_wire_refs(summary);
    let missing: Vec<_> = source_refs.difference(&summary_refs).cloned().collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ProvenanceError::InvalidEvent {
            activity_anchor: "compaction-summary".to_string(),
            reason: format!(
                "compaction summary dropped wire refs: {}",
                missing.join(", ")
            ),
        })
    }
}

/// Host service: validate and emit compaction provenance events.
pub struct ContextCompactionService;

impl ContextCompactionService {
    /// Deterministic finalize: merge refs + validate. Called by all summarizer backends.
    pub fn finalize_summary(prose: &str, source_rendered: &str) -> Result<String> {
        let merged = merge_compaction_summary(prose, source_rendered);
        validate_summary_preserves_wire_refs(&merged, source_rendered)?;
        Ok(merged)
    }

    pub fn build_record(mut record: ContextCompactionRecord, summary_text: String) -> ProvEvent {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_wire_refs_finds_hash_and_archive() {
        let refs = extract_wire_refs("user: #1 and @2 @12:L3");
        assert!(refs.contains("#1"));
        assert!(refs.contains("@2"));
        assert!(refs.contains("@12"));
        assert!(!refs.contains("@12:L3"));
    }

    #[test]
    fn merge_appends_all_source_refs() {
        let source = "user: read @12 and @3\nassistant: done #1";
        let merged = merge_compaction_summary("condensed prose", source);
        assert!(merged.contains("condensed prose"));
        assert!(merged.contains("@12"));
        assert!(merged.contains("@3"));
        assert!(merged.contains("#1"));
        validate_summary_preserves_wire_refs(&merged, source).expect("valid");
    }

    #[test]
    fn merge_omits_ref_clause_when_empty() {
        let merged = merge_compaction_summary("no refs here", "plain text");
        assert!(!merged.contains("Preserved refs:"));
    }

    #[test]
    fn finalize_summary_appends_refs_from_source_when_prose_omits_them() {
        let source = "tool: @7 blob";
        let summary =
            ContextCompactionService::finalize_summary("no refs", source).expect("merged refs");
        assert!(summary.contains("@7"));
        validate_summary_preserves_wire_refs(&summary, source).expect("valid");
    }
}

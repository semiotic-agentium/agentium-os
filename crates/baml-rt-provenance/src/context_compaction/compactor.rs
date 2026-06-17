// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Deterministic compaction summary synthesis and validation.

use std::collections::HashSet;

use baml_rt_tools::prompt_projection::format_conversation_history_transcript;
use serde_json::Value;

use super::types::ContextCompactionRecord;
use crate::{
    error::{ProvenanceError, Result},
    events::ProvEvent,
};

/// Extract `#N` and `@N` refs from text.
fn extract_wire_refs(text: &str) -> HashSet<String> {
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

/// Build a host-owned summary from rendered transcript lines, preserving wire refs.
#[must_use]
pub fn build_compaction_summary(rendered_transcript: &str) -> String {
    let refs: Vec<String> = extract_wire_refs(rendered_transcript).into_iter().collect();
    let mut lines: Vec<&str> = rendered_transcript
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.len() > 24 {
        lines = lines.split_off(lines.len() - 24);
    }
    let body = lines.join("\n");
    let ref_clause = if refs.is_empty() {
        String::new()
    } else {
        format!("\nPreserved refs: {}", refs.join(", "))
    };
    format!("[conversation summary]\n{body}{ref_clause}")
}

/// Reject summaries that drop archive handles present in the source transcript.
pub fn validate_summary_preserves_archive_refs(summary: &str, source: &str) -> Result<()> {
    let source_archives: HashSet<_> = extract_wire_refs(source)
        .into_iter()
        .filter(|r| r.starts_with('@'))
        .collect();
    if source_archives.is_empty() {
        return Ok(());
    }
    let summary_refs = extract_wire_refs(summary);
    let missing: Vec<_> = source_archives.difference(&summary_refs).cloned().collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ProvenanceError::InvalidEvent {
            activity_anchor: "compaction-summary".to_string(),
            reason: format!(
                "compaction summary dropped archive refs: {}",
                missing.join(", ")
            ),
        })
    }
}

/// Host service: synthesize, validate, and emit compaction provenance events.
pub struct ContextCompactionService;

impl ContextCompactionService {
    pub fn summarize_from_history_json(history: &Value) -> String {
        let rows = history.as_array().cloned().unwrap_or_default();
        let transcript = format_conversation_history_transcript(&rows);
        build_compaction_summary(&transcript)
    }

    pub fn build_record(
        record: ContextCompactionRecord,
        summary_text: String,
        source_rendered: &str,
    ) -> Result<ProvEvent> {
        validate_summary_preserves_archive_refs(&summary_text, source_rendered)?;
        Ok(ProvEvent::context_compaction_recorded_global(
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
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_preserves_archive_refs() {
        let source = "user: read @12 and @3\nassistant: done #1";
        let summary = build_compaction_summary(source);
        validate_summary_preserves_archive_refs(&summary, source).expect("valid");
    }

    #[test]
    fn rejects_dropped_archive_refs() {
        let source = "tool: @7 blob";
        let bad = "[conversation summary]\nno archives";
        assert!(validate_summary_preserves_archive_refs(bad, source).is_err());
    }
}

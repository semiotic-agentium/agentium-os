// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Transcript row ordering — single comparator for all operator timelines.

use std::cmp::Ordering;

use baml_rt_conversation::view::ProvenanceConversationContextItem;

#[must_use]
pub fn cmp_transcript_items(
    a: &ProvenanceConversationContextItem,
    b: &ProvenanceConversationContextItem,
) -> Ordering {
    a.timestamp_ms
        .cmp(&b.timestamp_ms)
        .then_with(|| a.activity_anchor.as_str().cmp(b.activity_anchor.as_str()))
}

pub fn sort_transcript_items(items: &mut [ProvenanceConversationContextItem]) {
    items.sort_by(cmp_transcript_items);
}

/// Rows strictly after `after` event order, capped at `limit`.
pub fn transcript_delta_rows(
    transcript: &[ProvenanceConversationContextItem],
    after: u64,
    limit: usize,
) -> Vec<ProvenanceConversationContextItem> {
    let mut rows: Vec<_> = transcript
        .iter()
        .filter(|item| item.timestamp_ms > after)
        .cloned()
        .collect();
    sort_transcript_items(&mut rows);
    rows.truncate(limit);
    rows
}

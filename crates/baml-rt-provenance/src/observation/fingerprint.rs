// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Canonical observation version hashing (transcript + metrics + page envelope).

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use serde_json;

use super::types::{LoadedObservation, ObservationVersion, TaskObservationMetrics};

/// Prompt-operation row inputs for version hashing.
#[derive(Debug, Clone, Copy)]
pub struct PromptOpsVersionRow<'a> {
    pub activity_anchor: &'a str,
    pub event_order: u64,
    pub prompt_context_bytes_current: u64,
    pub prompt_message_chars_current: u64,
}

/// Resume UI hints included in page version.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResumeVersionHints<'a> {
    pub awaiting_input: bool,
    pub input_required_prompt: Option<&'a str>,
}

/// Page envelope beyond transcript + task metrics.
#[derive(Debug, Clone, Copy, Default)]
pub struct PageVersionEnvelope<'a> {
    pub prompt_ops: &'a [PromptOpsVersionRow<'a>],
    pub prompt_context_bytes_session_current: Option<u64>,
    pub prompt_message_chars_session_current: Option<u64>,
    pub resume: ResumeVersionHints<'a>,
}

fn hash_transcript_items(hasher: &mut DefaultHasher, items: &[ProvenanceConversationContextItem]) {
    for item in items {
        item.timestamp_ms.hash(hasher);
        item.activity_anchor.as_str().hash(hasher);
        item.role.hash(hasher);
        content_fingerprint(&item.content).hash(hasher);
    }
}

fn hash_metrics(hasher: &mut DefaultHasher, metrics: Option<&TaskObservationMetrics>) {
    metrics.map(|m| m.llm_call_count).unwrap_or(0).hash(hasher);
}

/// Extend an in-progress hash with prompt telemetry and resume hints.
pub fn hash_page_envelope(hasher: &mut DefaultHasher, envelope: PageVersionEnvelope<'_>) {
    for op in envelope.prompt_ops {
        op.activity_anchor.hash(hasher);
        op.event_order.hash(hasher);
        op.prompt_context_bytes_current.hash(hasher);
        op.prompt_message_chars_current.hash(hasher);
    }
    envelope.prompt_context_bytes_session_current.hash(hasher);
    envelope.prompt_message_chars_session_current.hash(hasher);
    envelope.resume.awaiting_input.hash(hasher);
    envelope.resume.input_required_prompt.hash(hasher);
}

#[must_use]
pub fn observation_version_from_hasher(hasher: DefaultHasher) -> ObservationVersion {
    ObservationVersion(format!("obs-v1:{:x}", hasher.finish()))
}

fn content_fingerprint(content: &ConversationItemContent) -> String {
    serde_json::to_string(content).unwrap_or_default()
}

/// Hash transcript + task metrics only (slice fingerprint).
#[must_use]
pub fn observation_version_transcript(
    items: &[ProvenanceConversationContextItem],
    metrics: Option<&TaskObservationMetrics>,
) -> ObservationVersion {
    let mut hasher = DefaultHasher::new();
    hash_metrics(&mut hasher, metrics);
    hash_transcript_items(&mut hasher, items);
    observation_version_from_hasher(hasher)
}

#[must_use]
pub fn observation_version_from_loaded(obs: &LoadedObservation) -> ObservationVersion {
    observation_version_transcript(&obs.transcript, obs.metrics.as_ref())
}

/// Full page version: transcript + metrics + prompt telemetry + resume hints.
#[must_use]
pub fn observation_version_page(
    items: &[ProvenanceConversationContextItem],
    metrics: Option<&TaskObservationMetrics>,
    envelope: PageVersionEnvelope<'_>,
) -> ObservationVersion {
    let mut hasher = DefaultHasher::new();
    hash_metrics(&mut hasher, metrics);
    hash_transcript_items(&mut hasher, items);
    hash_page_envelope(&mut hasher, envelope);
    observation_version_from_hasher(hasher)
}

/// Hash planning + ops bundle slices for `/observe` SSE invalidation.
#[must_use]
pub fn observation_version_from_bundle(
    planning: &Option<super::types::LoadedPlanningSlice>,
    llm: &Option<crate::store::ProvenanceOpsQueryResponse>,
    tool: &Option<crate::store::ProvenanceOpsQueryResponse>,
) -> ObservationVersion {
    let mut hasher = DefaultHasher::new();
    if let Some(p) = planning {
        p.all_task_ids.len().hash(&mut hasher);
        p.tasks.len().hash(&mut hasher);
        for task in &p.tasks {
            task.task_id.hash(&mut hasher);
        }
    }
    if let Some(llm) = llm {
        llm.rows.len().hash(&mut hasher);
        llm.summary.count.hash(&mut hasher);
        llm.summary.failed_count.hash(&mut hasher);
    }
    if let Some(tool) = tool {
        tool.rows.len().hash(&mut hasher);
        tool.summary.count.hash(&mut hasher);
        tool.summary.failed_count.hash(&mut hasher);
    }
    observation_version_from_hasher(hasher)
}

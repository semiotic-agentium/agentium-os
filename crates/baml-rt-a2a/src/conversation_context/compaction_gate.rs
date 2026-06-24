// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Pre-model emergency compaction gate for prompt reads.

use baml_rt_core::context::RuntimeScope;
use baml_rt_provenance::{
    CompactionRequest, CompactionTriggerDecision, CompactionTriggerInput, CompactionTriggerSource,
    ContextCompactionSubscriber, evaluate_compaction_trigger, wire_history_byte_len,
};
use serde_json::Value;

/// True when projected wire history exceeds the model-aware emergency threshold.
#[must_use]
pub fn prompt_exceeds_emergency_threshold(
    rows: &[Value],
    subscriber: &ContextCompactionSubscriber,
) -> bool {
    let bytes = wire_history_byte_len(rows);
    let input = CompactionTriggerInput {
        source: CompactionTriggerSource::PreModel,
        item_count: rows.len(),
        prompt_bytes: bytes,
        safety: Default::default(),
        force: false,
    };
    matches!(
        evaluate_compaction_trigger(subscriber.trigger_policy(), &input),
        CompactionTriggerDecision::Run(_)
    )
}

/// Run synchronous pre-model compaction when the projected prompt exceeds the budget.
pub async fn run_pre_model_emergency_compaction(
    subscriber: &ContextCompactionSubscriber,
    scope: &RuntimeScope,
    rows: &[Value],
    in_flight: bool,
) {
    let request = CompactionRequest {
        context_id: scope.context_id().clone(),
        agent_id: scope.agent_id().clone(),
    };
    subscriber
        .evaluate_pre_model_from_rows(&request, rows, in_flight)
        .await;
}

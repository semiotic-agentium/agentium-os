// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Host-owned context compaction: range selection, projection, compactor, and hooks.

pub mod compactor;
pub mod digest;
pub mod partition;
pub mod prepare;
pub mod projection;
pub mod range;
pub mod render;
pub mod subscriber;
pub mod summarizer;
pub mod trigger;
pub mod types;

use baml_rt_llm_config::LlmClientConfig;
pub use baml_rt_llm_config::{
    CompactionTriggerPolicy, resolve_compaction_trigger_policy, trigger_policy_from_budget,
};
pub use compactor::{
    ContextCompactionService, extract_wire_refs, merge_compaction_summary,
    validate_summary_preserves_wire_refs,
};
pub use digest::{format_planning_digest, format_tail_preview};
pub use partition::partition_items_for_compaction;
pub use prepare::{CompactionPrepareError, PreparedCompaction, prepare_compaction};
pub use projection::{CompactionSummaryItem, apply_compaction_profile};
pub use range::{CompactableRange, item_is_live_planning_obligation, select_compactable_range};
pub use render::{
    CompactionRenderContext, prepare_render_context, render_items_for_context,
    render_items_with_ref_table, render_wire_history_rows, wire_history_byte_len,
};
pub use subscriber::{ContextCompactionSubscriber, resolve_safety_signals};
pub use summarizer::{
    CompactionSummarizeError, ConversationCompactionSummarizer, FixedCompactionSummarizer,
    HOST_COMPACTION_BAML_FUNCTION, compaction_runtime_scope,
};
pub use trigger::{
    CompactionDeferReason, CompactionSafetySignals, CompactionSkipReason,
    CompactionTriggerDecision, CompactionTriggerInput, CompactionTriggerSource,
    evaluate_compaction_trigger,
};
pub use types::{
    CompactionPrefixInput, CompactionRequest, ContextCompactionHead, ContextCompactionPolicy,
    ContextCompactionRecord, ContextCompactionTrigger,
};

/// Build legacy prepare/range policy from a trigger policy.
#[must_use]
pub fn context_compaction_policy_from_trigger(
    trigger_policy: &CompactionTriggerPolicy,
) -> ContextCompactionPolicy {
    ContextCompactionPolicy {
        item_threshold: trigger_policy.item_threshold,
        prompt_bytes_threshold: trigger_policy.emergency_prompt_bytes(),
        recent_tail_retention: trigger_policy.recent_tail_retention,
        model_id: trigger_policy.budget.model_id.clone(),
        budget_source: trigger_policy.budget.source,
    }
}

/// Resolve both trigger and legacy policies.
#[must_use]
pub fn resolve_compaction_policies(
    config: &LlmClientConfig,
    agent_package: Option<&str>,
    function_name: &str,
) -> (CompactionTriggerPolicy, ContextCompactionPolicy) {
    let trigger = resolve_compaction_trigger_policy(config, agent_package, function_name);
    let legacy = context_compaction_policy_from_trigger(&trigger);
    (trigger, legacy)
}

use crate::conversation_context_query::DEFAULT_LLM_CONTEXT_ITEM_CAP;

/// Post-turn compaction may run when item count reaches this threshold.
pub const DEFAULT_COMPACTION_ITEM_THRESHOLD: usize = DEFAULT_LLM_CONTEXT_ITEM_CAP;

/// Pre-model emergency compaction when serialized prompt bytes exceed this budget.
pub const DEFAULT_COMPACTION_PROMPT_BYTES_THRESHOLD: u64 = 32_768;

/// Rows kept verbatim at the end of the transcript after compaction.
pub const DEFAULT_RECENT_TAIL_RETENTION: usize = 12;

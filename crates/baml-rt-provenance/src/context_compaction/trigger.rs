// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Pure compaction trigger decision controller.

use baml_rt_llm_config::{CompactionTriggerPolicy, bytes_to_tokens};

use super::types::ContextCompactionTrigger;

/// Why compaction was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionSkipReason {
    BelowItemThreshold,
    BelowPromptThreshold,
    ManualWithoutForce,
}

impl CompactionSkipReason {
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::BelowItemThreshold => "below_item_threshold",
            Self::BelowPromptThreshold => "below_prompt_bytes_threshold",
            Self::ManualWithoutForce => "manual_without_force",
        }
    }
}

/// Why compaction was deferred (may retry later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionDeferReason {
    InFlightTurn,
    AwaitingInput,
}

impl CompactionDeferReason {
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::InFlightTurn => "in_flight_turn",
            Self::AwaitingInput => "awaiting_input",
        }
    }
}

/// Outcome of trigger evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTriggerDecision {
    Run(ContextCompactionTrigger),
    Skip(CompactionSkipReason),
    Defer(CompactionDeferReason),
}

impl CompactionTriggerDecision {
    #[must_use]
    pub fn result_label(self) -> &'static str {
        match self {
            Self::Run(_) => "success",
            Self::Skip(_) => "skipped",
            Self::Defer(_) => "deferred",
        }
    }

    #[must_use]
    pub fn reason_label(self) -> Option<&'static str> {
        match self {
            Self::Run(_) => None,
            Self::Skip(r) => Some(r.as_wire_str()),
            Self::Defer(r) => Some(r.as_wire_str()),
        }
    }
}

/// Which code path initiated the trigger evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTriggerSource {
    PostTurn,
    PreModel,
    Manual,
}

/// Runtime safety signals gathered by adapters before evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionSafetySignals {
    pub in_flight: bool,
    pub awaiting_input: bool,
}

/// Inputs for a single trigger evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionTriggerInput {
    pub source: CompactionTriggerSource,
    pub item_count: usize,
    pub prompt_bytes: u64,
    pub safety: CompactionSafetySignals,
    pub force: bool,
}

impl CompactionTriggerInput {
    #[must_use]
    pub fn estimated_prompt_tokens(&self) -> u64 {
        bytes_to_tokens(self.prompt_bytes)
    }
}

/// Evaluate whether compaction should run, skip, or defer.
#[must_use]
pub fn evaluate_compaction_trigger(
    policy: &CompactionTriggerPolicy,
    input: &CompactionTriggerInput,
) -> CompactionTriggerDecision {
    match input.source {
        CompactionTriggerSource::Manual => evaluate_manual(policy, input),
        CompactionTriggerSource::PostTurn => evaluate_post_turn(policy, input),
        CompactionTriggerSource::PreModel => evaluate_pre_model(policy, input),
    }
}

fn evaluate_manual(
    policy: &CompactionTriggerPolicy,
    input: &CompactionTriggerInput,
) -> CompactionTriggerDecision {
    if !input.force {
        return CompactionTriggerDecision::Skip(CompactionSkipReason::ManualWithoutForce);
    }
    apply_safety_gates(
        policy,
        input,
        ContextCompactionTrigger::ManualOperator,
        false,
    )
}

fn evaluate_post_turn(
    policy: &CompactionTriggerPolicy,
    input: &CompactionTriggerInput,
) -> CompactionTriggerDecision {
    if input.item_count < policy.item_threshold {
        return CompactionTriggerDecision::Skip(CompactionSkipReason::BelowItemThreshold);
    }
    if input.prompt_bytes > 0 {
        let tokens = input.estimated_prompt_tokens();
        if tokens < policy.budget.safe_prompt_tokens {
            return CompactionTriggerDecision::Skip(CompactionSkipReason::BelowPromptThreshold);
        }
    }
    apply_safety_gates(
        policy,
        input,
        ContextCompactionTrigger::PostTurnThreshold,
        true,
    )
}

fn evaluate_pre_model(
    policy: &CompactionTriggerPolicy,
    input: &CompactionTriggerInput,
) -> CompactionTriggerDecision {
    let tokens = input.estimated_prompt_tokens();
    let bytes = input.prompt_bytes;
    let threshold_met =
        tokens >= policy.budget.emergency_prompt_tokens || bytes >= policy.emergency_prompt_bytes();
    if !threshold_met {
        return CompactionTriggerDecision::Skip(CompactionSkipReason::BelowPromptThreshold);
    }
    apply_safety_gates(
        policy,
        input,
        ContextCompactionTrigger::PreModelEmergency,
        false,
    )
}

fn apply_safety_gates(
    policy: &CompactionTriggerPolicy,
    input: &CompactionTriggerInput,
    trigger: ContextCompactionTrigger,
    defer_on_in_flight: bool,
) -> CompactionTriggerDecision {
    if policy.defer_while_in_flight && input.safety.in_flight && defer_on_in_flight {
        return CompactionTriggerDecision::Defer(CompactionDeferReason::InFlightTurn);
    }
    if policy.defer_while_in_flight
        && input.safety.in_flight
        && matches!(input.source, CompactionTriggerSource::PreModel)
    {
        return CompactionTriggerDecision::Defer(CompactionDeferReason::InFlightTurn);
    }
    if policy.defer_while_awaiting_input
        && input.safety.awaiting_input
        && matches!(input.source, CompactionTriggerSource::PostTurn)
    {
        return CompactionTriggerDecision::Defer(CompactionDeferReason::AwaitingInput);
    }
    CompactionTriggerDecision::Run(trigger)
}

#[cfg(test)]
mod tests {
    use baml_rt_llm_config::{BudgetFreshness, BudgetSource, ModelContextBudget};

    use super::*;

    fn test_budget() -> ModelContextBudget {
        ModelContextBudget {
            model_id: "openai/gpt-4o-mini".to_string(),
            provider: "openrouter".to_string(),
            client_name: "OpenRouter".to_string(),
            context_window_tokens: 128_000,
            safe_prompt_tokens: 80_000,
            emergency_prompt_tokens: 110_000,
            output_reserve_tokens: 4096,
            source: BudgetSource::KnownModel,
            freshness: BudgetFreshness::NotApplicable,
            warning: None,
        }
    }

    fn test_policy() -> CompactionTriggerPolicy {
        CompactionTriggerPolicy::from_budget(test_budget(), 48, 12, true, true)
    }

    #[test]
    fn post_turn_skips_below_item_threshold() {
        let decision = evaluate_compaction_trigger(
            &test_policy(),
            &CompactionTriggerInput {
                source: CompactionTriggerSource::PostTurn,
                item_count: 10,
                prompt_bytes: 400_000,
                safety: CompactionSafetySignals::default(),
                force: false,
            },
        );
        assert_eq!(
            decision,
            CompactionTriggerDecision::Skip(CompactionSkipReason::BelowItemThreshold)
        );
    }

    #[test]
    fn post_turn_defers_when_awaiting_input() {
        let decision = evaluate_compaction_trigger(
            &test_policy(),
            &CompactionTriggerInput {
                source: CompactionTriggerSource::PostTurn,
                item_count: 100,
                prompt_bytes: 400_000,
                safety: CompactionSafetySignals {
                    awaiting_input: true,
                    ..Default::default()
                },
                force: false,
            },
        );
        assert_eq!(
            decision,
            CompactionTriggerDecision::Defer(CompactionDeferReason::AwaitingInput)
        );
    }

    #[test]
    fn pre_model_runs_at_emergency_threshold() {
        let decision = evaluate_compaction_trigger(
            &test_policy(),
            &CompactionTriggerInput {
                source: CompactionTriggerSource::PreModel,
                item_count: 100,
                prompt_bytes: 440_000,
                safety: CompactionSafetySignals::default(),
                force: false,
            },
        );
        assert_eq!(
            decision,
            CompactionTriggerDecision::Run(ContextCompactionTrigger::PreModelEmergency)
        );
    }

    #[test]
    fn pre_model_defers_when_in_flight() {
        let decision = evaluate_compaction_trigger(
            &test_policy(),
            &CompactionTriggerInput {
                source: CompactionTriggerSource::PreModel,
                item_count: 100,
                prompt_bytes: 500_000,
                safety: CompactionSafetySignals {
                    in_flight: true,
                    ..Default::default()
                },
                force: false,
            },
        );
        assert_eq!(
            decision,
            CompactionTriggerDecision::Defer(CompactionDeferReason::InFlightTurn)
        );
    }

    #[test]
    fn manual_requires_force() {
        let decision = evaluate_compaction_trigger(
            &test_policy(),
            &CompactionTriggerInput {
                source: CompactionTriggerSource::Manual,
                item_count: 1,
                prompt_bytes: 100,
                safety: CompactionSafetySignals::default(),
                force: false,
            },
        );
        assert_eq!(
            decision,
            CompactionTriggerDecision::Skip(CompactionSkipReason::ManualWithoutForce)
        );
    }
}

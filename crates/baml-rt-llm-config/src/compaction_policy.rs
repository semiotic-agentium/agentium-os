// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Model-aware compaction trigger policy resolution.

use crate::{LlmClientConfig, ModelContextBudget, resolve_effective_budget};

/// Policy resolved for the model that will consume the next prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionTriggerPolicy {
    pub budget: ModelContextBudget,
    pub item_threshold: usize,
    pub recent_tail_retention: usize,
    pub defer_while_in_flight: bool,
    pub defer_while_awaiting_input: bool,
}

impl CompactionTriggerPolicy {
    /// Build from resolved model budget and compaction defaults.
    #[must_use]
    pub fn from_budget(
        budget: ModelContextBudget,
        item_threshold: usize,
        recent_tail_retention: usize,
        defer_in_flight: bool,
        defer_awaiting: bool,
    ) -> Self {
        Self {
            budget,
            item_threshold,
            recent_tail_retention,
            defer_while_in_flight: defer_in_flight,
            defer_while_awaiting_input: defer_awaiting,
        }
    }

    #[must_use]
    pub fn emergency_prompt_bytes(&self) -> u64 {
        self.budget.emergency_prompt_bytes()
    }

    #[must_use]
    pub fn safe_prompt_bytes(&self) -> u64 {
        self.budget.safe_prompt_bytes()
    }
}

/// Resolve trigger policy for the model that will consume the next prompt.
#[must_use]
pub fn resolve_compaction_trigger_policy(
    config: &LlmClientConfig,
    agent_package: Option<&str>,
    function_name: &str,
) -> CompactionTriggerPolicy {
    let budget = resolve_effective_budget(config, agent_package, function_name);
    trigger_policy_from_budget(config, budget)
}

/// Resolve trigger policy from an already-resolved budget.
#[must_use]
pub fn trigger_policy_from_budget(
    config: &LlmClientConfig,
    budget: ModelContextBudget,
) -> CompactionTriggerPolicy {
    let defaults = &config.compaction.defaults;
    CompactionTriggerPolicy::from_budget(
        budget,
        defaults.item_threshold,
        defaults.recent_tail_retention,
        defaults.defer_while_in_flight,
        defaults.defer_while_awaiting_input,
    )
}

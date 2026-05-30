// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Plan-level context for drift scoring.
//!
//! When a task has an active committed plan, the [`PlanDriftContext`] carries
//! the strategic anchors (intent description, current step, plan objective)
//! into the drift scorer so it can measure alignment against the plan — not
//! just against the immediate prompt.

use serde::{Deserialize, Serialize};

/// A single plan step used as an embedding anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStepAnchor {
    pub step_id: String,
    pub description: String,
    pub order: u32,
}

/// Strategic context from a committed plan, supplied alongside the tactical
/// prompt/response pair when computing plan-anchored drift.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanDriftContext {
    /// The declared intent description (from `IntentResolved`).
    pub intent_description: String,

    /// The plan-level objective text (often the same as or derived from intent).
    pub plan_objective: String,

    /// The step currently being executed, if any.
    pub current_step: Option<PlanStepAnchor>,

    /// 0-based index of the current step in the plan.
    pub step_index: u32,

    /// Total number of steps in the committed plan.
    pub total_steps: u32,

    /// Whether this plan was produced by a supersession (revision).
    /// When `true`, the trajectory tracker applies revision leniency.
    pub is_revised_plan: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_drift_context_round_trips_through_serde() {
        let ctx = PlanDriftContext {
            intent_description: "Create a quarterly sales report".into(),
            plan_objective: "Generate and deliver the Q3 sales report".into(),
            current_step: Some(PlanStepAnchor {
                step_id: "step-extract".into(),
                description: "Extract sales data from the CRM".into(),
                order: 1,
            }),
            step_index: 1,
            total_steps: 3,
            is_revised_plan: false,
        };

        let json = serde_json::to_string(&ctx).expect("serialize");
        let deserialized: PlanDriftContext = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ctx, deserialized);
    }
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Operator-facing planning lifecycle rows (intent, plan, step transitions).
//! Not projected into BAML `conversation_transcript`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningEventKind {
    IntentResolved,
    IntentRevised,
    PlanCommitted,
    PlanSuperseded,
    PlanStepStatusChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningEventContent {
    pub kind: PlanningEventKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_status: Option<String>,
}

impl PlanningEventContent {
    #[must_use]
    pub fn is_meaningful(&self) -> bool {
        !self.summary.trim().is_empty()
    }
}

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateReasonCode {
    TierBelowGate,
    NoArtifact,
    StaleArtifact,
    CoversMismatch,
    DeficientNodes,
    NoPostconditions,
    RequirementsMet,
    Tier3Authorization,
    DryRunRecorded,
    InternalError,
}

impl GateReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TierBelowGate => "tier_below_gate",
            Self::NoArtifact => "no_artifact",
            Self::StaleArtifact => "stale_artifact",
            Self::CoversMismatch => "covers_mismatch",
            Self::DeficientNodes => "deficient_nodes",
            Self::NoPostconditions => "no_postconditions",
            Self::RequirementsMet => "requirements_met",
            Self::Tier3Authorization => "tier3_authorization",
            Self::DryRunRecorded => "dry_run_recorded",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateTelemetryEvent {
    pub tool_class: String,
    pub tier: u8,
    pub decision: String,
    pub reason_code: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deficient_nodes: Option<Vec<String>>,
}

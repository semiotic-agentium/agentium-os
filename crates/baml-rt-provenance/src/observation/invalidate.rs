// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Map committed provenance events to operator observation invalidation kinds.

use baml_rt_core::observation::kinds;

use crate::events::{ProvEvent, ProvEventData};

#[must_use]
pub fn observation_kinds_for_event(event: &ProvEvent) -> u8 {
    if event.context_id_opt().is_none() {
        return 0;
    }
    match event.data() {
        ProvEventData::IntentResolved { .. }
        | ProvEventData::PlanGenerated { .. }
        | ProvEventData::PlanStepStatusChanged { .. } => kinds::PLANNING,
        ProvEventData::LlmCallCompleted { .. }
        | ProvEventData::ToolCallCompleted { .. }
        | ProvEventData::PromptRejected { .. } => kinds::OPS | kinds::TRANSCRIPT,
        ProvEventData::MessageReceived { .. }
        | ProvEventData::MessageSent { .. }
        | ProvEventData::HostSourcePollRecorded { .. }
        | ProvEventData::HostDispatchAccepted { .. }
        | ProvEventData::HostDispatchRejected { .. }
        | ProvEventData::ToolSessionStep { .. } => kinds::TRANSCRIPT,
        _ => kinds::TRANSCRIPT | kinds::OPS,
    }
}

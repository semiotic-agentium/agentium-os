// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conversation context projection from provenance into BAML wire JSON.

mod compaction_gate;
mod projecting_provider;

pub use compaction_gate::{prompt_exceeds_emergency_threshold, run_pre_model_emergency_compaction};
pub use projecting_provider::{ProjectingConversationContextProvider, to_projection_item};

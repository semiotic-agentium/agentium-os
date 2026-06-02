// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Modular provenance read traits (transcript, planning, ops).

pub mod ops;
pub mod planning;
pub mod transcript;

pub use ops::{OpsPageSpec, OpsReader};
pub use planning::{PlanningReader, PlanningSliceSpec};
pub use transcript::{
    TranscriptEngine, TranscriptPage, TranscriptPageRequest, TranscriptProjectionProfile,
    TranscriptScopeWidening,
};

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Eval execution engine (chat + multi-turn).

mod manifest;
mod run;

pub use manifest::load_manifest;
pub use run::{EvalRunOptions, init_eval_manifest, report_last_run, run_eval};

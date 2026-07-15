// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Historic **episode** view: types and rendering live in [`baml_rt_conversation`]; graph-backed
//! assembly and I/O stay in this crate.

mod agent_gate_activity;
mod aggregates;
mod archive;
mod drift;
mod from_graph;
mod gate;
mod gate_row;
mod reader;

pub use agent_gate_activity::{
    AgentGateActivity, AgentGateActivityFilters, AgentGateCounts, GateIncidentRow, RankedCount,
    agent_has_gate_activity, aggregate_agent_gate_activity,
    aggregate_agent_gate_activity_from_rows,
};
pub(crate) use aggregates::token_summary_for_task;
pub use archive::{CachedEpisode, EpisodeArchiveSource, episode_ref_table};
/// Episode transcript / drift types and ref-prefix helpers.
pub use baml_rt_conversation::episode::*;
pub use baml_rt_conversation::render::{
    prefix_wire_citation, prefix_wire_citations_in_text, render_episode,
};
pub use drift::aggregate_task_drift;
pub use gate::{GateEventRow, TaskGateAggregate, aggregate_task_gate};
pub use reader::EpisodeReader;

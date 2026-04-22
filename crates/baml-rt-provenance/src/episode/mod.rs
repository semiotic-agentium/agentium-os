//! Historic **episode** view: types and rendering live in [`baml_rt_conversation`]; graph-backed
//! assembly and I/O stay in this crate.

mod aggregates;
mod archive;
mod drift;
mod from_graph;
mod reader;

pub use archive::{CachedEpisode, EpisodeArchiveSource, episode_ref_table};
/// Episode transcript / drift types and ref-prefix helpers.
pub use baml_rt_conversation::episode::*;
pub use baml_rt_conversation::render::{
    prefix_wire_citation, prefix_wire_citations_in_text, render_episode,
};
pub use drift::aggregate_task_drift;
pub use reader::EpisodeReader;

//! Typed operator observation: one graph slice, many projections.

mod engine;
mod fingerprint;
mod loader;
mod ops;
mod planning;
mod scope;
mod sql;
mod transcript_order;
mod types;

pub use engine::ObservationLoader;
pub use fingerprint::{
    PageVersionEnvelope, PromptOpsVersionRow, ResumeVersionHints, hash_page_envelope,
    observation_version_from_hasher, observation_version_from_loaded, observation_version_page,
    observation_version_transcript,
};
pub use planning::{task_ids_for_context, task_ids_for_scope};
pub use scope::{observation_scope_from_history, observation_scope_from_ops_filters};
pub use sql::after_event_order_filter_sql;
pub use transcript_order::{cmp_transcript_items, sort_transcript_items, transcript_delta_rows};
pub use types::{
    EventOrder, LoadedObservation, ObservationScope, ObservationVersion, OpsQueryMode,
    TaskObservationMetrics, TaskObservationScope, TemporalBound,
};

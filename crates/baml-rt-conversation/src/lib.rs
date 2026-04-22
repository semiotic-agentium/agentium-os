//! Agent **conversation view model**, projection into LLM-visible `conversation_history`, and
//! **episode** transcript shapes shared by BAML, HTTP history, and replayed archives.
//!
//! # Scope
//!
//! - **Inputs** are **already** graph-reconstructed rows ([`ProvenanceConversationContextItem`]
//!   and friends). This crate does not repair a broken write path; it renders and assembles
//!   views from typed data.
//! - **No database / Surreal I/O** in the default modules: unit tests do not need embedded DBs.
//! - **Isomorphism** with live [`baml_rt_tools::prompt_projection`] is policy: use the same
//!   [`baml_rt_tools::prompt_projection::ProjectionRenderOptions`] (e.g. [`episode_session_history_projection_options`](baml_rt_tools::prompt_projection::episode_session_history_projection_options))
//!   when materializing [`SessionHistoryLine`](episode::SessionHistoryLine) for episode replay.
//!
//! # Heuristics
//!
//! [`ConversationItemContent::is_meaningful`](view::ConversationItemContent::is_meaningful) and `StatusOnly` discards at the projection
//! boundary are the **documented** exceptions. Do not add read-time deduplication of graph
//! rows here — fix the emitter or the graph.

pub mod episode;
pub mod projection;
pub mod render;
pub mod session_history;
pub mod timeline;
pub mod view;

pub use projection::{
    projection_pairs_for_conv_item, provenance_item_to_projection_item,
    session_history_body_from_send_done_replay,
};
pub use render::{prefix_wire_citation, prefix_wire_citations_in_text, render_episode};
pub use session_history::assemble_session_history;

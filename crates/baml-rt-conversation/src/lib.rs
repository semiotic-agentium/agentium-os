//! Agent **conversation view model**, projection into LLM-visible `conversation_history`, and
//! **episode** transcript shapes shared by BAML, HTTP history, and replayed archives.
//!
//! # Scope
//!
//! - **Inputs** are **already** graph-reconstructed rows ([`ProvenanceConversationContextItem`]
//!   and friends). This crate does not repair a broken write path; it renders and assembles
//!   views from typed data.
//! - **No database / Surreal I/O** in the default modules: unit tests do not need embedded DBs.
//! - **Isomorphism** with live BAML tags: the pipeline is
//!   **provenance view row** → [`provenance_item_to_projection_item`] →
//!   [`project_projection_item_to_rows`] (or [`project_prompt_context`](baml_rt_tools::prompt_projection::project_prompt_context) for `ctx.tags`) **without**
//!   cross-item state. The A2A runtime uses the same sequence in
//!   `ProjectingConversationContextProvider` (`baml-rt-a2a` / `a2a_transport.rs`), not a separate
//!   fork. Episode [`assemble_session_history`](session_history::assemble_session_history) uses the
//!   same row primitive with [`ProjectionRenderOptions`] from
//!   [`episode_session_history_projection_options`], matching the provenance `EpisodeReader` path
//!   (`baml-rt-provenance` crate). Message rows may include a `citations: string[]` field in the tag JSON
//!   when graph refs are present.
//! - **Prefix cacheability (append-only before compaction):** default projection **extends** the
//!   item stream; it does not rewrite earlier rows on each new turn. See **R9** in
//!   [`docs/baml-rt-conversation-spec.md`](../../docs/baml-rt-conversation-spec.md).
//! - **Normative spec** (requirements, ecosystem comparison, gap analysis, traceability):
//!   [`docs/baml-rt-conversation-spec.md`](../../docs/baml-rt-conversation-spec.md).
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

pub use baml_rt_tools::prompt_projection::{
    ProjectedLineRole, ProjectionRenderOptions, episode_session_history_projection_options,
    project_projection_item_to_rows,
};
pub use projection::{
    projection_pairs_for_conv_item, provenance_item_to_projection_item,
    session_history_body_from_send_done_replay,
};
pub use render::{prefix_wire_citation, prefix_wire_citations_in_text, render_episode};
pub use session_history::assemble_session_history;

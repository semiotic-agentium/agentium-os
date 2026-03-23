//! Archive read: grep/paginate/cat-n over archived tool output.
//!
//! Three layers, each pure and independently testable:
//!
//! - **render**: JSON → YAML text lines (long strings become block scalars)
//! - **grep**: line-level substring/regex filter + offset/limit pagination
//! - **cat_n**: GNU `cat -n` style line numbering with original positions
//!
//! The module is stateless. Session-scoped ref tables (`@N → content`)
//! will live in a separate `archive_refs` module.

pub mod cat_n;
pub mod grep;
pub mod render;
pub mod rendered;
pub mod types;

pub use cat_n::{format_cat_n, format_cat_n_sequential};
pub use grep::grep_paginate;
pub use render::render_to_lines;
pub use rendered::RenderedContent;
pub use types::{
    GrepPage, GrepPattern, HistoryRef, LineOffset, LineWithPosition, PageLimit, ShortRef,
};

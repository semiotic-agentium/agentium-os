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
pub mod read_resolve;
pub mod render;
pub mod rendered;
pub mod session_read_body;
pub mod types;
pub mod virtual_source;

pub use cat_n::{format_cat_n, format_cat_n_sequential};
pub use grep::grep_paginate;
pub use read_resolve::{ResolvedArchiveRead, resolve_archive_for_read};
pub use render::render_to_lines;
pub use rendered::RenderedContent;
pub use session_read_body::{
    format_grep_page_as_session_read_body, format_session_read_body_from_json_value,
    format_session_read_body_from_rendered, session_read_command_line,
};
pub use types::{
    DEFAULT_TOOL_RESULT_INLINE_LINES, GrepPage, GrepPattern, HistoryRef, LineOffset,
    LineWithPosition, PageLimit, SEND_DONE_HISTORY_INLINE_LINES,
    SESSION_HISTORY_READ_REPLAY_MAX_LINES, ShortRef,
};
pub use virtual_source::{VirtualArchiveRow, VirtualArchiveSource, VirtualHistoryRow};

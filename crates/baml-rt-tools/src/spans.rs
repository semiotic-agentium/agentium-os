//! OpenTelemetry span instrumentation for tool operations.
//!
//! This module provides orthogonal span creation helpers following the pattern
//! from the OpenTelemetry instrumentation guide. All spans use static names
//! with dynamic data in structured fields.

use crate::tool_fsm::ToolSessionId;
use crate::tools::ToolName;
use tracing::Span;

/// Create span for tool registration operation.
///
/// Parent: Caller's span (auto-attached by tracing)
/// Children: None (registration is synchronous)
#[inline]
pub(crate) fn register_tool(tool_name: &ToolName, description: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.register_tool",
        tool = %tool_name,
        description = description,
    )
}

/// Create span for tool execution operation.
///
/// Parent: Caller's span (auto-attached by tracing)
/// Children: Tool session operations
#[inline]
pub(crate) fn execute_tool(tool_name: &str) -> Span {
    tracing::debug_span!("baml_rt_tools.execute_tool", tool = tool_name,)
}

/// Create span for opening a tool session.
///
/// Parent: Tool execution span or caller's span
/// Children: Session operations (send, next, finish)
#[inline]
pub(crate) fn open_session(session_id: &ToolSessionId, tool_name: &ToolName) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.open_session",
        session_id = %session_id,
        tool = %tool_name,
    )
}

/// Create span for sending input to a tool session.
///
/// Parent: Session span
/// Children: Tool execution
#[inline]
pub(crate) fn session_send(session_id: &ToolSessionId) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.session_send",
        session_id = %session_id,
    )
}

/// Create span for advancing a tool session.
///
/// Parent: Session span
/// Children: None
#[inline]
pub(crate) fn session_next(session_id: &ToolSessionId) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.session_next",
        session_id = %session_id,
    )
}

/// Create span for finishing a tool session.
///
/// Parent: Session span
/// Children: None
#[inline]
pub(crate) fn session_finish(session_id: &ToolSessionId) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.session_finish",
        session_id = %session_id,
    )
}

/// Create span for aborting a tool session.
///
/// Parent: Session span or drop handler
/// Children: None
#[inline]
pub(crate) fn session_abort(session_id: &ToolSessionId, reason: Option<&str>) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.session_abort",
        session_id = %session_id,
        reason = reason,
    )
}

#[inline]
pub(crate) fn notion_request(url: &str) -> Span {
    tracing::debug_span!("baml_rt_tools.notion_request", url = url)
}

#[inline]
pub(crate) fn notion_search_pages(query_len: Option<usize>, page_size: Option<u32>) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.notion_search_pages",
        query_len = query_len,
        page_size = page_size
    )
}

#[inline]
pub(crate) fn notion_get_page(page_id: &str) -> Span {
    tracing::debug_span!("baml_rt_tools.notion_get_page", page_id = page_id)
}

#[inline]
pub(crate) fn notion_get_page_blocks(block_id: &str) -> Span {
    tracing::debug_span!("baml_rt_tools.notion_get_page_blocks", block_id = block_id)
}

#[inline]
pub(crate) fn notion_fetch_page_summary(page_id: &str) -> Span {
    tracing::debug_span!("baml_rt_tools.notion_fetch_page_summary", page_id = page_id)
}

#[inline]
pub(crate) fn notion_fetch_child_blocks(parent_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.notion_fetch_child_blocks",
        parent_id = parent_id
    )
}

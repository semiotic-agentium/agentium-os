// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry span instrumentation for tool operations.
//!
//! This module provides orthogonal span creation helpers following the pattern
//! from the OpenTelemetry instrumentation guide. All spans use static names
//! with dynamic data in structured fields.

use tracing::Span;

use crate::{tool_fsm::ToolSessionId, tools::ToolName};

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
/// Children: Session operations (send, read, finish)
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

/// Create span for read operation on a tool session.
///
/// Parent: Session span
/// Children: None
#[inline]
pub(crate) fn session_read(session_id: &ToolSessionId) -> Span {
    tracing::debug_span!(
        "baml_rt_tools.session_read",
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

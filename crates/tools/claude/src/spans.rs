// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Debug-level span helpers for claude/dev session (OTel-style, orthogonal to business logic).

use tracing::Span;

/// Span for opening a claude/dev session.
#[inline]
pub(crate) fn session_open(session_id: &str, agent_id: &str, workspace: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_tools_claude.session_open",
        session_id = session_id,
        agent_id = agent_id,
        workspace = workspace,
    )
}

/// Span for send (enqueueing a turn); the actual stream consumption runs in a spawned task.
#[inline]
pub(crate) fn session_send(session_id: &str) -> Span {
    tracing::debug_span!("baml_rt_tools_claude.session_send", session_id = session_id,)
}

/// Span for the background task that consumes the SDK stream and pushes to the channel.
#[inline]
pub(crate) fn stream_turn_consumer(session_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_tools_claude.stream_turn_consumer",
        session_id = session_id,
    )
}

/// Span for waiting on the next item from the SDK stream (inside the consumer).
#[inline]
pub(crate) fn stream_next_await(session_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_tools_claude.stream_next_await",
        session_id = session_id,
    )
}

/// Span for session next (reading from channel and building step).
#[inline]
pub(crate) fn session_next(session_id: &str) -> Span {
    tracing::debug_span!("baml_rt_tools_claude.session_next", session_id = session_id,)
}

/// Span for the recv().await in next() when pending is empty (where we block on the channel).
#[inline]
pub(crate) fn session_next_recv_await(session_id: &str) -> Span {
    tracing::debug_span!(
        "baml_rt_tools_claude.session_next_recv_await",
        session_id = session_id,
    )
}

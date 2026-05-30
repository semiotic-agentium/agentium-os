// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry span helpers for the ClickUp BAML tool.

use tracing::Span;

#[inline]
pub(crate) fn list_teams() -> Span {
    tracing::debug_span!("baml_tools_clickup.list_teams")
}

#[inline]
pub(crate) fn list_spaces(team_id: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.list_spaces", team_id = team_id)
}

#[inline]
pub(crate) fn list_lists(space_id: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.list_lists", space_id = space_id)
}

#[inline]
pub(crate) fn list_tasks(list_id: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.list_tasks", list_id = list_id)
}

#[inline]
pub(crate) fn get_task(task_id: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.get_task", task_id = task_id)
}

#[inline]
pub(crate) fn create_task(list_id: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.create_task", list_id = list_id)
}

#[inline]
pub(crate) fn update_task(task_id: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.update_task", task_id = task_id)
}

#[inline]
pub(crate) fn delete_task(task_id: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.delete_task", task_id = task_id)
}

/// Root span for [`super::ClickUpTool::execute`] dispatch.
#[inline]
pub(crate) fn execute(action: &str) -> Span {
    tracing::debug_span!("baml_tools_clickup.execute", action = action)
}

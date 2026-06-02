// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Scope builders from HTTP/runner request shapes.

use baml_rt_core::ids::{ContextId, TaskId};

use super::types::{ObservationScope, TaskObservationScope, TemporalBound};

/// Build scope from conversation-history query fields.
#[must_use]
pub fn observation_scope_from_history(
    context_id: ContextId,
    task_id: Option<TaskId>,
    agent_package: Option<String>,
    after_event_order: Option<u64>,
) -> ObservationScope {
    ObservationScope {
        context_id,
        task: match task_id {
            Some(id) => TaskObservationScope::Task(id),
            None => TaskObservationScope::ContextWide,
        },
        agent_package,
        temporal: match after_event_order {
            Some(order) => TemporalBound::After(super::types::EventOrder(order)),
            None => TemporalBound::All,
        },
    }
}

/// Build scope from provenance ops filters (context optional for global queries).
#[must_use]
pub fn observation_scope_from_ops_filters(
    filters: &crate::store::ProvenanceOpsFilters,
) -> Option<ObservationScope> {
    let context_id = filters.context_id.clone()?;
    Some(ObservationScope {
        context_id,
        task: match filters.task_id.clone() {
            Some(id) => TaskObservationScope::Task(id),
            None => TaskObservationScope::ContextWide,
        },
        agent_package: filters.agent_package.clone(),
        temporal: TemporalBound::All,
    })
}

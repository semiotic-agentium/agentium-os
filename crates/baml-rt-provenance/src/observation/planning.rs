// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Task id resolution for planning reads from observation scope.

use baml_rt_core::ids::ContextId;

use super::types::{ObservationScope, TaskObservationScope};
use crate::{
    error::Result, surreal_store::SurrealProvenanceStore, task_graph_reader::TaskGraphReader,
};

/// Task ids to include in planning reads for this scope.
pub async fn task_ids_for_scope(
    store: &SurrealProvenanceStore,
    scope: &ObservationScope,
) -> Result<Vec<String>> {
    let mut task_ids: Vec<String> = match &scope.task {
        TaskObservationScope::Task(tid) => vec![tid.as_str().to_string()],
        TaskObservationScope::ContextWide => store
            .list_scoped(&scope.context_id)
            .await?
            .into_iter()
            .map(|r| r.task_id().as_str().to_string())
            .collect(),
    };
    task_ids.sort();
    task_ids.dedup();
    Ok(task_ids)
}

/// Legacy helper when only context id is known (context-wide scope).
pub async fn task_ids_for_context(
    store: &SurrealProvenanceStore,
    context_id: &str,
) -> Result<Vec<String>> {
    let scope =
        ObservationScope::context_wide(ContextId::from(context_id), None, Default::default());
    task_ids_for_scope(store, &scope).await
}

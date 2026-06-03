// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared provenance store bootstrap for integration and snapshot tests.

use std::{sync::Arc, time::Duration};

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, TaskId, UuidId};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceWriter, SurrealProvenanceStore, SurrealStoreBuilder,
};

/// Isolated in-memory Surreal store (one namespace per call — safe under parallel nextest).
pub async fn build_isolated_store() -> Arc<SurrealProvenanceStore> {
    SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("build isolated in-memory provenance store")
}

/// Deterministic agent id used across provenance snapshot scenarios.
pub fn provenance_agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000020").unwrap())
}

pub fn provenance_context_id(n: u64) -> ContextId {
    ContextId::new(n, n)
}

pub fn provenance_task_id(name: &str) -> TaskId {
    TaskId::from_external(ExternalId::new(name))
}

/// Wall-clock separation for episode duration assertions (~12ms).
pub async fn wall_clock_tick() {
    tokio::time::sleep(Duration::from_millis(12)).await;
}

/// Agent boot → task exists → execution started → submitted status.
pub async fn bootstrap_task(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    task_id: &TaskId,
    agent_id: &AgentId,
) {
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("test_agent").expect("agent_type"),
            "1.0.0".to_string(),
            "test@1.0.0".to_string(),
        ))
        .await
        .expect("agent_booted");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_exists(context_id.clone(), task_id.clone()))
        .await
        .expect("task_exists");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_execution_started(
            context_id.clone(),
            task_id.clone(),
            agent_id.clone(),
        ))
        .await
        .expect("task_execution_started");
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            None,
            Some("TASK_STATE_SUBMITTED".to_string()),
        ))
        .await
        .expect("task_status_submitted");
}

/// Mark task completed (working → completed).
pub async fn complete_task(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    task_id: &TaskId,
) {
    wall_clock_tick().await;
    store
        .add_event(ProvEvent::task_status_changed(
            context_id.clone(),
            task_id.clone(),
            Some("working".to_string()),
            Some("completed".to_string()),
        ))
        .await
        .expect("task_status_completed");
}

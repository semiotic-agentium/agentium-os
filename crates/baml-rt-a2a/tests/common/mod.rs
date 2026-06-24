// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for baml-rt-a2a integration tests.

use std::collections::HashMap;

use baml_rt_a2a::a2a_types::{Task, TaskState, TaskStatus};
use baml_rt_core::ids::{ContextId, TaskId};

/// Builds a minimal Task for testing (TaskStore / apply_task_delta).
/// Used by apply_task_delta_concurrent_test and task_fsm_property_test (other test binaries).
#[allow(dead_code)] // shared test helper; used by other test binaries
pub fn minimal_task(task_id: &TaskId, context_id: &ContextId, status: Option<TaskStatus>) -> Task {
    Task {
        id: Some(task_id.clone()),
        context_id: Some(context_id.clone()),
        status,
        artifacts: vec![],
        history: vec![],
        metadata: None,
        extra: HashMap::new(),
    }
}

/// Builds a TaskStatus with the given state string.
/// Used by apply_task_delta_concurrent_test and task_fsm_property_test (other test binaries).
#[allow(dead_code)] // shared test helper; used by other test binaries
pub fn task_status(state: &str) -> TaskStatus {
    TaskStatus {
        state: Some(TaskState::String(state.to_string())),
        ..Default::default()
    }
}

pub mod provenance {
    use std::sync::Arc;

    use baml_rt::QuickJSConfig;
    use baml_rt_a2a::A2aAgent;
    use baml_rt_provenance::{SurrealProvenanceStore, SurrealStoreBuilder};

    /// Builds an A2aAgent with a provenance writer (e.g. SurrealDB in-memory).
    /// Used by provenance_context_test and provenance_property_test (other test binaries).
    #[allow(dead_code)] // shared test helper; used by other test binaries
    pub async fn build_provenance_agent(
        store: Arc<SurrealProvenanceStore>,
        init_js: &str,
    ) -> A2aAgent {
        A2aAgent::builder()
            .with_surreal_store(store)
            .with_init_js(init_js)
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
            .with_compaction_summarizer(baml_rt_provenance::FixedCompactionSummarizer::test_stub())
            .build()
            .await
            .expect("build provenance agent")
    }

    /// Build an isolated SurrealDB store for integration tests.
    /// Avoids shared global in-memory state across test binaries.
    ///
    /// When one store is passed to a single agent, the same store (and connection) is used for
    /// both create-stream writes (live_result_pipeline.store_result) and subscribe read
    /// (repository.get); create-stream vs subscribe failures are then due to A2A messaging
    /// (task id on write path vs id in tasks.subscribe params), not connection scope.
    #[allow(dead_code)] // shared test helper; used by other test binaries
    pub async fn build_surreal_test_store() -> Arc<SurrealProvenanceStore> {
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build isolated surreal store")
    }

    /// Build in-memory shared SurrealDB store. Used to isolate file-backend vs in-memory behavior.
    #[allow(dead_code)] // shared test helper; used by other test binaries
    pub async fn build_surreal_in_memory_store() -> Arc<SurrealProvenanceStore> {
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build in-memory surreal store")
    }
}

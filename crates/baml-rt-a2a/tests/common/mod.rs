//! Shared helpers for baml-rt-a2a integration tests.

use std::collections::HashMap;

use baml_rt_a2a::a2a_types::{Task, TaskState, TaskStatus};
use baml_rt_core::ids::{ContextId, TaskId};

/// Builds a minimal Task for testing (TaskStore / apply_task_delta).
/// Used by apply_task_delta_concurrent_test and task_fsm_property_test (other test binaries).
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn task_status(state: &str) -> TaskStatus {
    TaskStatus {
        state: Some(TaskState::String(state.to_string())),
        ..Default::default()
    }
}

pub mod provenance {
    use std::{
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use baml_rt::QuickJSConfig;
    use baml_rt_a2a::A2aAgent;
    use baml_rt_provenance::{GraphqliteProvenanceStore, GraphqliteStoreBuilder};

    /// Builds an A2aAgent with a provenance writer (e.g. GraphQLite in-memory).
    /// Used by provenance_context_test and provenance_property_test (other test binaries).
    #[allow(dead_code)]
    pub async fn build_provenance_agent(
        store: Arc<GraphqliteProvenanceStore>,
        init_js: &str,
    ) -> A2aAgent {
        A2aAgent::builder()
            .with_graphqlite_store(store)
            .with_init_js(init_js)
            .with_effect_emitter(Arc::new(baml_rt_core::bus::BusWithEffects::new()))
            .with_quickjs_config(QuickJSConfig::new().with_max_attempts_ms(Some(15_000)))
            .build()
            .await
            .expect("build provenance agent")
    }

    /// Build an isolated file-backed GraphQLite store for integration tests.
    /// Avoids shared global in-memory state across test binaries.
    #[allow(dead_code)]
    pub fn build_graphqlite_test_store() -> Arc<GraphqliteProvenanceStore> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "baml-rt-a2a-provenance-{pid}-{unique}.db",
            pid = std::process::id(),
        ));
        GraphqliteStoreBuilder::file(path)
            .build()
            .expect("build isolated graphqlite store")
    }
}

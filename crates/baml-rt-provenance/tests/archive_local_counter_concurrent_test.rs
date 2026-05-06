//! Parallel `archive_next_local` first-touch races duplicate `CREATE`; losers must retry `UPDATE`.

use std::{collections::HashSet, sync::Arc};

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_provenance::SurrealStoreBuilder;
use uuid::Uuid;

#[tokio::test]
async fn parallel_archive_next_local_succeeds_when_create_races() {
    const PARALLEL: usize = 24;

    let store = Arc::new(
        SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .expect("build store"),
    );

    let context_id = ContextId::new(9191, 1);
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::nil()));

    let prefix = store
        .archive_ensure_prefix(&context_id, &agent_id)
        .await
        .expect("ensure prefix");

    let mut handles = Vec::with_capacity(PARALLEL);
    for _ in 0..PARALLEL {
        let store = Arc::clone(&store);
        let context_id = context_id.clone();
        handles.push(tokio::spawn(async move {
            store
                .archive_next_local(&context_id, prefix)
                .await
                .expect("next_local must tolerate duplicate-create losers")
        }));
    }

    let mut seen = HashSet::new();
    for h in handles {
        let n = h.await.expect("task join");
        assert!(seen.insert(n), "duplicate archive_local {n}");
    }

    assert_eq!(seen.len(), PARALLEL);
}

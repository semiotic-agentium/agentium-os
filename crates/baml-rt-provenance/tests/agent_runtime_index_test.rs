//! Agent runtime index registry behavior.

use baml_rt_core::{
    AgentInstanceId, AgentPackageName, AgentRouteKey, DispatchTarget,
    ids::{AgentId, ContextId, UuidId},
};
use baml_rt_provenance::{AgentType, ProvEvent, ProvenanceWriter, SurrealStoreBuilder};
use uuid::Uuid;

#[tokio::test]
async fn agent_booted_upserts_package_instance_registry() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let agent_id = AgentId::from_uuid(UuidId::new(Uuid::new_v4()));
    store
        .add_event(ProvEvent::agent_booted(
            agent_id.clone(),
            AgentType::new("perf_agent").expect("type"),
            "1.0.0".to_string(),
            "perf@1.0.0".to_string(),
        ))
        .await
        .expect("boot");

    let rows = store
        .db()
        .query("SELECT instance_node_id, agent_package, agent_id FROM agent_package_instance")
        .await
        .expect("query")
        .take::<Vec<serde_json::Value>>(0)
        .expect("rows");
    assert_eq!(rows.len(), 1, "expected one registry row: {rows:?}");
    assert_eq!(
        rows[0].get("agent_package").and_then(|v| v.as_str()),
        Some("perf_agent")
    );
    assert_eq!(
        rows[0].get("agent_id").and_then(|v| v.as_str()),
        Some(agent_id.as_str())
    );
}

#[tokio::test]
async fn dispatch_without_boot_does_not_index_route_stub() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("store");
    let ctx = ContextId::new(1, 2);
    store
        .add_event(ProvEvent::host_dispatch_accepted(
            ctx,
            "event:intake".to_string(),
            "host.source-records.v1".to_string(),
            DispatchTarget::with_optional_agent(
                AgentRouteKey::new(
                    AgentPackageName::parse("orphan-agent").expect("package"),
                    AgentInstanceId::default(),
                ),
                None,
            ),
            "clickup".to_string(),
            "clickup:list:1".to_string(),
        ))
        .await
        .expect("dispatch without target agent");

    let rows = store
        .db()
        .query("SELECT instance_node_id, agent_package, agent_id FROM agent_package_instance")
        .await
        .expect("query")
        .take::<Vec<serde_json::Value>>(0)
        .expect("rows");
    assert!(
        rows.is_empty(),
        "non-boot dispatch must not upsert registry rows: {rows:?}"
    );
}

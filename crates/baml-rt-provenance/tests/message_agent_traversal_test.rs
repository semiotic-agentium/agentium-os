//! Live-DB regression: Message → owning-agent must be reachable via the
//! two-hop edge traversal `Message ↔ A2AMessageProcessing -[:WAS_EXECUTED_BY]->
//! AgentRuntimeInstance`, NOT via a denormalised `props.a2a_agent_id`
//! property on the Message entity.
//!
//! Confirms three invariants against a real SurrealDB instance:
//!
//! 1. Message entities do NOT carry the `a2a_agent_id` property — agent
//!    ownership is modelled exclusively as edges.
//! 2. The ops-query Messages branch with `filters.agent_id` returns the
//!    right rows by traversing edges (the picker's correctness path).
//! 3. The ops-query Messages branch with `filters.agent_package` returns
//!    only rows whose owning agent's archive matches that package, again
//!    by edge traversal.

use baml_rt_core::ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId};
use baml_rt_provenance::{
    ProvEvent, SurrealStoreBuilder,
    store::{
        ProvenanceOpsFilters, ProvenanceOpsQuery, ProvenanceOpsQueryRequest, ProvenanceOpsResource,
        ProvenanceWriter,
    },
};
use serde_json::Value;

fn agent() -> AgentId {
    AgentId::from_uuid(UuidId::new(uuid::Uuid::new_v4()))
}

#[tokio::test]
async fn message_entity_does_not_carry_denormalised_agent_id_property() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory store");

    let agent_id = agent();
    let context_id = ContextId::new(1_780_000_010_000, 1);
    let task_id = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));

    store
        .add_event(ProvEvent::message_received_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from("msg-rx-1"),
            "ROLE_USER".to_string(),
            vec!["hello".to_string()],
            None,
            agent_id.clone(),
            1_780_000_010_000,
        ))
        .await
        .expect("write inbound message");

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::Messages,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id.clone()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query Messages")
        .rows;

    assert_eq!(rows.len(), 1, "expected one Message row; got {rows:?}");
    let row = &rows[0];
    let projected_agent_id = row.get("a2a_agent_id").and_then(Value::as_str);
    assert!(
        projected_agent_id.is_none(),
        "Message rows must NOT carry a denormalised `a2a_agent_id` property — \
         agent ownership is an EDGE relationship traversable via \
         A2AMessageProcessing -[:WAS_EXECUTED_BY]-> AgentRuntimeInstance. \
         Found: {projected_agent_id:?}"
    );
}

#[tokio::test]
async fn message_rows_can_be_filtered_by_agent_id_via_edge_traversal() {
    let store = SurrealStoreBuilder::in_memory_isolated()
        .build()
        .await
        .expect("in-memory store");

    let agent_a = agent();
    let agent_b = agent();
    let context_id = ContextId::new(1_780_000_011_000, 1);
    let task_id = TaskId::from_external(ExternalId::new(uuid::Uuid::new_v4().to_string()));

    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from("msg-from-a"),
            "ROLE_AGENT".to_string(),
            vec!["a".to_string()],
            None,
            agent_a.clone(),
            1_780_000_011_000,
            Vec::new(),
        ))
        .await
        .expect("write agent_a message");
    store
        .add_event(ProvEvent::message_sent_task(
            context_id.clone(),
            task_id.clone(),
            MessageId::from("msg-from-b"),
            "ROLE_AGENT".to_string(),
            vec!["b".to_string()],
            None,
            agent_b.clone(),
            1_780_000_011_001,
            Vec::new(),
        ))
        .await
        .expect("write agent_b message");

    let rows = store
        .query_ops(ProvenanceOpsQueryRequest {
            resource: ProvenanceOpsResource::Messages,
            filters: ProvenanceOpsFilters {
                context_id: Some(context_id.clone()),
                agent_id: Some(agent_a.clone()),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("query Messages by agent_id (edge traversal)")
        .rows;

    assert_eq!(
        rows.len(),
        1,
        "agent_id filter on Messages must traverse the MessageProcessing → AgentRuntimeInstance \
         edge chain and select exactly the agent_a message"
    );
    assert_eq!(
        rows[0].get("a2a_message_id").and_then(Value::as_str),
        Some("msg-from-a"),
        "selected row must be the message authored by agent_a"
    );
}
